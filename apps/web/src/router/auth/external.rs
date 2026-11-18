//! Third-party login pages: starting authorization, the callback, "complete
//! registration" for third-party sign-up, and in account security binding, unbinding, and setting a password after
//! confirming identity through a third party. All are native forms and plain links, usable without JS.
//!
//! - Authorization starts from a plain link (`GET /auth/{google|github}`), not a form: redirecting to the provider
//!   after a form submission would be blocked by `form-action 'self'`. Login CSRF is prevented by binding `state` to an
//!   encrypted cookie written when this browser started authorization; binding and confirmation additionally require,
//!   at callback time, that the session is the member who started it and that the credentials version is unchanged.
//! - The identity confirmed by the callback goes into another encrypted cookie, read when completing registration,
//!   confirming a binding (entering the current password) and setting a password. Both cookies use the `__Host-`
//!   prefix, `Path=/` and `SameSite=Lax` (the provider's redirect back is a top-level GET, so they are sent).
//! - The callback URL is built from this request's site address (with the sub-path prefix):
//!   `{site address}/auth/{provider}/callback`.
use axum::extract::Path;
use subtle::ConstantTimeEq;

use super::*;
use crate::identity::external::{ExternalLoginOutcome, FLOW_TTL, Flow, FlowPurpose, PENDING_TTL};
use crate::oauth::{Challenge, Provider};
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, Page, csrf_forms};

const REGISTER_PATH: &str = "/register/external";
const ACCOUNT_EXTERNAL: &str = "/my/account#external";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/{provider}", get(start))
        .route("/auth/{provider}/callback", get(callback))
        .route(REGISTER_PATH, get(register_page).post(register))
        .route("/my/account/external/link", post(link))
        .route("/my/account/external/unlink", post(unlink))
        .route("/my/account/password/external", post(password_set))
}

fn flow_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_oauth"
    } else {
        "gonof_oauth"
    }
}

fn pending_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_external"
    } else {
        "gonof_external"
    }
}

fn set_flow(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    super::sms::set_named(response, flow_name(secure), secure, value, max_age);
}

fn set_pending(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    super::sms::set_named(response, pending_name(secure), secure, value, max_age);
}

/// The identity confirmed by this request's callback; `None` when the purpose or member does not match, or it expired.
pub(super) fn pending(
    state: &AppState,
    headers: &HeaderMap,
    purpose: FlowPurpose,
    uid: i64,
) -> Option<(String, identity::external::Pending)> {
    let raw = unique_cookie(headers, pending_name(state.viewer.secure), 4096)?;
    state
        .identity
        .open_pending(raw, purpose, uid)
        .map(|pending| (raw.to_owned(), pending))
}

/// Which third parties this request's login, registration and account security pages show: those ticked whose admin
/// credentials are configured. Reads nothing from the database when none is ticked.
pub(super) async fn shown(state: &AppState) -> Vec<Provider> {
    let checked: Vec<Provider> = Provider::ALL
        .into_iter()
        .filter(|provider| crate::login::provider_checked(&state.frame.settings, provider.value()))
        .collect();
    if checked.is_empty() {
        return checked;
    }
    match crate::sms::current(&state.db).await {
        Ok(providers) => checked
            .into_iter()
            .filter(|provider| providers.oauth(*provider).configured())
            .collect(),
        Err(error) => {
            tracing::warn!(error = %error, "failed to read the third-party login configuration; the login page hides third-party entries for now");
            Vec::new()
        }
    }
}

/// Callback URL: this request's site address (with the sub-path prefix) plus `/auth/{provider}/callback`.
pub(crate) fn callback_url(site_url: &str, provider: Provider) -> String {
    format!(
        "{}/auth/{}/callback",
        site_url.trim_end_matches('/'),
        provider.value()
    )
}

fn not_enabled(state: &AppState, provider: Provider) -> Response {
    error_page(
        state,
        StatusCode::NOT_FOUND,
        &crate::i18n::current().account_error_provider_off(provider.label()),
    )
}

fn with_return(path: &str, return_to: &str) -> String {
    let destination = safe_return(return_to);
    if destination.is_empty() {
        return path.to_owned();
    }
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("return_to", &destination);
    format!("{path}?{}", query.finish())
}

#[derive(Default, Deserialize)]
struct StartQuery {
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    return_to: String,
}

async fn start(
    Page(state): Page,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
) -> Response {
    let Some(provider) = Provider::parse(&provider) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    let Some(purpose) = FlowPurpose::parse(&query.purpose) else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().account_error_purpose(),
        );
    };
    if !crate::login::provider_checked(&state.frame.settings, provider.value()) {
        return not_enabled(&state, provider);
    }
    let (uid, credentials_version) = match (purpose, &state.viewer.session) {
        (FlowPurpose::Login, Some(_)) => return redirect("/"),
        (FlowPurpose::Login, None) => (0, 0),
        (_, None) => return redirect("/login?return_to=%2Fmy%2Faccount"),
        // Refused before the provider is asked: the callback and the confirmation would refuse anyway.
        (FlowPurpose::Link, Some(session)) if session.member.email_unverified() => {
            let message = crate::i18n::current().account_error_verify_email_first();
            return account_failure(&state, &headers, message, StatusCode::CONFLICT).await;
        }
        (_, Some(session)) => (session.member.uid, session.member.credentials_version),
    };
    let (client_id, _) = match state.identity.oauth_credentials(provider).await {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return not_enabled(&state, provider),
        Err(error) => return identity_error(&state, error),
    };
    let challenge = Challenge::new();
    let flow = Flow {
        provider,
        purpose,
        uid,
        credentials_version,
        challenge: challenge.clone(),
        return_to: safe_return(&query.return_to),
        expires_at: identity::now() + FLOW_TTL,
    };
    let sealed = match state.identity.seal_flow(&flow) {
        Ok(sealed) => sealed,
        Err(error) => return identity_error(&state, error),
    };
    let location = crate::oauth::authorize_url(
        state.identity.oauth_endpoints(),
        provider,
        &client_id,
        &callback_url(state.site_url(), provider),
        &challenge,
    );
    // An external address; `href` returns it unchanged (the guard requires every redirect header to pass through it).
    let mut response = match HeaderValue::from_str(&crate::site::href(&location)) {
        Ok(value) => (StatusCode::FOUND, [(header::LOCATION, value)]).into_response(),
        Err(_) => {
            return error_page(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::i18n::current().account_error_authorize_url(),
            );
        }
    };
    set_flow(&mut response, state.viewer.secure, &sealed, FLOW_TTL);
    response
}

#[derive(Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
}

async fn callback(
    Page(state): Page,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(provider) = Provider::parse(&provider) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    let flow = unique_cookie(&headers, flow_name(state.viewer.secure), 4096)
        .and_then(|raw| state.identity.open_flow(raw))
        .filter(|flow| flow.provider == provider);
    let mut response = match flow {
        Some(flow) => finish(&state, &headers, provider, flow, &query).await,
        None => {
            login_failure(
                &state,
                crate::i18n::current().account_error_flow_login(),
                StatusCode::FORBIDDEN,
            )
            .await
        }
    };
    // One authorization is used only once.
    set_flow(&mut response, state.viewer.secure, "", 0);
    response
}

/// Error for the login purpose: back to the login page with the reason shown.
async fn login_failure(state: &AppState, message: &str, status: StatusCode) -> Response {
    let form = AuthForm::default();
    account_page(state, false, &form, message, status).await
}

/// Error for the binding and confirmation purposes: back to the third-party accounts part of the account security
/// page.
async fn account_failure(
    state: &AppState,
    headers: &HeaderMap,
    message: &str,
    status: StatusCode,
) -> Response {
    security::external_response(state, headers, message, status).await
}

async fn failure(
    state: &AppState,
    headers: &HeaderMap,
    purpose: FlowPurpose,
    message: &str,
    status: StatusCode,
) -> Response {
    match purpose {
        FlowPurpose::Login => login_failure(state, message, status).await,
        FlowPurpose::Link | FlowPurpose::Confirm => {
            account_failure(state, headers, message, status).await
        }
    }
}

async fn finish(
    state: &AppState,
    headers: &HeaderMap,
    provider: Provider,
    flow: Flow,
    query: &CallbackQuery,
) -> Response {
    let purpose = flow.purpose;
    let state_ok = !query.state.is_empty()
        && bool::from(
            query
                .state
                .as_bytes()
                .ct_eq(flow.challenge.state.as_bytes()),
        );
    if !state_ok {
        return failure(
            state,
            headers,
            purpose,
            crate::i18n::current().account_error_flow_retry(),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    if !query.error.is_empty() || query.code.is_empty() {
        let message = crate::i18n::current().account_error_provider_cancelled(provider.label());
        return failure(state, headers, purpose, &message, StatusCode::OK).await;
    }
    // The session must still be the member who started it with an unchanged credentials version; the login purpose
    // requires not being signed in.
    let member = match (purpose, &state.viewer.session) {
        (FlowPurpose::Login, Some(_)) => return redirect("/"),
        (FlowPurpose::Login, None) => None,
        (_, Some(session))
            if session.member.uid == flow.uid
                && session.member.credentials_version == flow.credentials_version =>
        {
            Some(session.member.clone())
        }
        (_, _) => return redirect("/login?session_ended=true"),
    };
    match state
        .limit(&[Rule::new(
            "login-ip",
            &state.viewer.client_ip,
            u32::try_from(state.frame.settings.account.login_ip_per_5min).unwrap_or(0),
            300,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return failure(
                state,
                headers,
                purpose,
                crate::i18n::current().account_error_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    let (client_id, secret) = match state.identity.oauth_credentials(provider).await {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return not_enabled(state, provider),
        Err(error) => return identity_error(state, error),
    };
    let profile = match crate::oauth::exchange(
        state.identity.oauth_endpoints(),
        provider,
        &client_id,
        &secret,
        &callback_url(state.site_url(), provider),
        &query.code,
        &flow.challenge,
    )
    .await
    {
        Ok(profile) => profile,
        Err(error) => {
            tracing::warn!(provider = provider.value(), error = %error, "third-party authorization did not complete");
            let message = crate::i18n::current().account_error_provider_failed(provider.label());
            return failure(state, headers, purpose, &message, StatusCode::BAD_GATEWAY).await;
        }
    };
    let destination = safe_return(&flow.return_to);
    match (purpose, member) {
        (FlowPurpose::Login, _) => {
            login_outcome(state, provider, profile, headers, &destination).await
        }
        (FlowPurpose::Link, Some(member)) => {
            match state
                .identity
                .prepare_link(&member, provider, profile)
                .await
            {
                Ok(token) => {
                    let mut response = redirect(ACCOUNT_EXTERNAL);
                    set_pending(&mut response, state.viewer.secure, &token, PENDING_TTL);
                    response
                }
                Err(IdentityError::Conflict(message) | IdentityError::Validation(message)) => {
                    account_failure(state, headers, message, StatusCode::CONFLICT).await
                }
                Err(error) => identity_error(state, error),
            }
        }
        (FlowPurpose::Confirm, Some(member)) => {
            match state
                .identity
                .confirm_external(&member, provider, profile)
                .await
            {
                Ok(token) => {
                    let mut response = redirect("/my/account#password");
                    set_pending(
                        &mut response,
                        state.viewer.secure,
                        &token,
                        identity::external::CONFIRM_TTL,
                    );
                    response
                }
                Err(IdentityError::Conflict(message) | IdentityError::Validation(message)) => {
                    account_failure(state, headers, message, StatusCode::CONFLICT).await
                }
                Err(error) => identity_error(state, error),
            }
        }
        (_, None) => redirect("/login?session_ended=true"),
    }
}

async fn login_outcome(
    state: &AppState,
    provider: Provider,
    profile: crate::oauth::Profile,
    headers: &HeaderMap,
    destination: &str,
) -> Response {
    let metadata = session_metadata(state, headers).with_device(&device_marker(state, headers));
    match state
        .identity
        .external_login(provider, profile, &metadata)
        .await
    {
        Ok(ExternalLoginOutcome::Login(identity::LoginOutcome::Session(grant))) => grant_response(
            state,
            *grant,
            if destination.is_empty() {
                "/"
            } else {
                destination
            },
        ),
        Ok(ExternalLoginOutcome::Login(identity::LoginOutcome::SecondFactor(pending))) => {
            let mut response = redirect(&with_return("/login/verify", destination));
            two_factor::set_pending(
                &mut response,
                state.viewer.secure,
                &pending.token,
                identity::SECOND_FACTOR_PENDING_TTL,
            );
            response
        }
        Ok(ExternalLoginOutcome::Register(token)) => {
            let mut response = redirect(&with_return(REGISTER_PATH, destination));
            set_pending(&mut response, state.viewer.secure, &token, PENDING_TTL);
            response
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            login_failure(state, message, StatusCode::CONFLICT).await
        }
        Err(IdentityError::Suspended(message)) => {
            login_failure(state, &message, StatusCode::FORBIDDEN).await
        }
        Err(IdentityError::Unauthorized | IdentityError::Forbidden) => {
            login_failure(
                state,
                crate::i18n::current().account_error_cannot_log_in(),
                StatusCode::UNAUTHORIZED,
            )
            .await
        }
        Err(error) => identity_error(state, error),
    }
}

#[derive(Template)]
#[template(path = "auth/external_register.html")]
struct RegisterTemplate<'a> {
    p: PageMeta<'a>,
    label: &'static str,
    display: &'a str,
    /// The email verified by the provider: used directly, not filled in separately.
    verified_email: Option<&'a str>,
    form: &'a AuthForm,
    invitation_required: bool,
    message: &'a str,
}

fn register_response(
    state: &AppState,
    pending: &identity::external::Pending,
    form: &AuthForm,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut response = render(&RegisterTemplate {
        p: meta(
            state,
            crate::i18n::current().account_finish_signup().into(),
            crate::i18n::current().account_external_description(
                pending.provider.label(),
                crate::site::site_name(&state.frame.settings),
            ),
            REGISTER_PATH,
            "",
            "",
        ),
        label: pending.provider.label(),
        display: &pending.display,
        verified_email: pending.email.as_deref(),
        form,
        invitation_required: state.frame.settings.signup.mode == "invite",
        message,
    });
    *response.status_mut() = status;
    response
}

#[derive(Default, Deserialize)]
struct RegisterQuery {
    #[serde(default)]
    return_to: String,
}

async fn register_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<RegisterQuery>,
) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    let Some((_, pending)) = pending(&state, &headers, FlowPurpose::Login, 0) else {
        return login_failure(
            &state,
            crate::i18n::current().account_error_external_login_expired(),
            StatusCode::OK,
        )
        .await;
    };
    let mut form = AuthForm {
        return_to: safe_return(&query.return_to),
        username: pending.suggestion.clone(),
        ..Default::default()
    };
    if let Err(error) = show_policies(&state, &mut form).await {
        return identity_error(&state, error);
    }
    register_response(&state, &pending, &form, "", StatusCode::OK)
}

async fn register(Page(state): Page, headers: HeaderMap, form: CheckedForm<AuthForm>) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    let Some((token, pending)) = pending(&state, &headers, FlowPurpose::Login, 0) else {
        return login_failure(
            &state,
            crate::i18n::current().account_error_external_login_expired(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let mut form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().common_error_form(),
            );
        }
        CheckedForm::StaleToken(form, reason) => {
            return register_response(
                &state,
                &pending,
                &form,
                &crate::i18n::current().account_error_stale_reason(reason),
                StatusCode::FORBIDDEN,
            );
        }
    };
    // Without ticking the agreement the registration transaction is not entered (same as password registration).
    if form.accept_terms != "1" {
        return register_response(
            &state,
            &pending,
            &form,
            crate::i18n::current().account_error_consent(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    let consent = match policy_consent(&state, &mut form).await {
        Ok(Some(consent)) => consent,
        Ok(None) => {
            return register_response(
                &state,
                &pending,
                &form,
                crate::i18n::current().account_error_terms_updated(),
                StatusCode::CONFLICT,
            );
        }
        Err(error) => return identity_error(&state, error),
    };
    match state
        .limit(&[Rule::new(
            "register",
            &state.viewer.client_ip,
            u32::try_from(state.frame.settings.signup.ip_hourly_limit).unwrap_or(0),
            3600,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return register_response(
                &state,
                &pending,
                &form,
                crate::i18n::current().account_error_register_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    let channel = registration_source::read(&headers, &state.viewer);
    let metadata = session_metadata(&state, &headers)
        .with_device(&device_marker(&state, &headers))
        .with_registration_source(state.viewer.origin.as_deref(), channel.as_deref());
    let unverified_email = pending.email.is_none() && !form.email.trim().is_empty();
    match state
        .identity
        .register_with_external(
            &token,
            &form.username,
            &form.email,
            &form.invite,
            &metadata,
            Some(consent),
        )
        .await
    {
        Ok(grant) => {
            let destination = safe_return(&form.return_to);
            let path = if unverified_email {
                "/verify-email"
            } else if destination.is_empty() {
                "/"
            } else {
                &destination
            };
            let mut response = grant_response(&state, grant, path);
            set_pending(&mut response, state.viewer.secure, "", 0);
            if channel.is_some() {
                registration_source::write(&mut response, &state.viewer, None);
            }
            if unverified_email {
                auxiliary_set_cookie(&mut response, "gonof_return", &destination, 3600);
            }
            response
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            register_response(
                &state,
                &pending,
                &form,
                message,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
        Err(IdentityError::Suspended(message)) => {
            register_response(&state, &pending, &form, &message, StatusCode::FORBIDDEN)
        }
        Err(error) => identity_error(&state, error),
    }
}

#[derive(Default, Deserialize)]
struct ExternalForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
    #[serde(default)]
    provider: String,
}
csrf_forms!(ExternalForm);

/// Common precondition of these account security forms: the form is valid and the member is signed in.
fn account_form(
    state: &AppState,
    form: CheckedForm<ExternalForm>,
) -> Result<(ExternalForm, &identity::AuthSession), &'static str> {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => return Err(crate::i18n::current().common_error_form()),
        CheckedForm::StaleToken(..) => return Err(crate::i18n::current().account_error_stale()),
    };
    let session = state
        .viewer
        .session
        .as_ref()
        .ok_or(crate::i18n::current().account_error_login_first())?;
    Ok((form, session))
}

async fn link(Page(state): Page, headers: HeaderMap, form: CheckedForm<ExternalForm>) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return account_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let uid = session.member.uid;
    let Some((token, _)) = pending(&state, &headers, FlowPurpose::Link, uid) else {
        return account_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_link_expired(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let attempt = match security::password_attempt(&state, uid).await {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            return account_failure(
                &state,
                &headers,
                crate::i18n::current().account_error_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    };
    let result = state
        .identity
        .link_external(&state.viewer.raw_sid, &form.current_password, &token)
        .await;
    match security::password_result(attempt, result).await {
        Ok(provider) => {
            let mut response = redirect(&format!(
                "/my/account?external_linked={}#external",
                provider.value()
            ));
            response.extensions_mut().insert(IdentityChanged);
            set_pending(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            account_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn unlink(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<ExternalForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return account_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let Some(provider) = Provider::parse(&form.provider) else {
        return account_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_unlink_provider(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let attempt = match security::password_attempt(&state, session.member.uid).await {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            return account_failure(
                &state,
                &headers,
                crate::i18n::current().account_error_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    };
    let result = state
        .identity
        .unlink_external(&state.viewer.raw_sid, &form.current_password, provider)
        .await;
    match security::password_result(attempt, result).await {
        Ok(()) => {
            let mut response = redirect(&format!(
                "/my/account?external_unlinked={}#external",
                provider.value()
            ));
            response.extensions_mut().insert(IdentityChanged);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            account_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn password_set(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<ExternalForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return account_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let Some((token, _)) = pending(&state, &headers, FlowPurpose::Confirm, session.member.uid)
    else {
        return account_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_confirm_expired(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    if form.password != form.password2 {
        return account_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_password_mismatch(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    match state
        .identity
        .set_password_with_external(&state.viewer.raw_sid, &token, &form.password)
        .await
    {
        Ok(()) => {
            let mut response = security::signed_out(&state, "/login?password_set=true");
            set_pending(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            account_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_urls_keep_the_sub_path() {
        assert_eq!(
            callback_url("https://test.example.com", Provider::Google),
            "https://test.example.com/auth/google/callback"
        );
        assert_eq!(
            callback_url("https://privacy.example.com/feedback/", Provider::Github),
            "https://privacy.example.com/feedback/auth/github/callback"
        );
    }
}
