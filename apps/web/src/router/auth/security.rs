//! Account security uses native forms; signing out everywhere and credential changes are done by Identity's primary
//! database transactions.
use std::time::Duration;

use super::*;
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, Page, csrf_forms};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/account", get(account))
        .route("/my/account/password", post(change_password))
        .route("/forgot-password", get(forgot_page).post(forgot))
        .route("/reset-password", get(reset_page).post(reset))
        .route("/my/account/logout-all", post(logout_all))
}

#[derive(Default, Deserialize)]
struct SecurityForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    token: String,
}
csrf_forms!(SecurityForm);

#[derive(Default, Deserialize)]
struct SecurityQuery {
    #[serde(default)]
    sent: bool,
    #[serde(default)]
    token: String,
}

#[derive(Default, Deserialize)]
struct AccountQuery {
    #[serde(default)]
    two_factor_disabled: bool,
    #[serde(default)]
    recovery_login: bool,
    #[serde(default)]
    mobile_bound: bool,
    #[serde(default)]
    mobile_unbound: bool,
    #[serde(default)]
    external_linked: String,
    #[serde(default)]
    external_unlinked: String,
}

/// Phone number and password state on the account security page.
struct MobileView {
    /// Accounts without a password (SMS registration): show "Set a password" instead of "Change password"; two-step
    /// verification and changing the phone number require setting a password first.
    has_password: bool,
    /// The bound number (masked).
    bound: Option<String>,
    /// A code has been sent in the binding step: the new number (masked).
    bind_sent_to: Option<String>,
    /// A code has been sent in the set-password step.
    confirm_sent: bool,
    message: String,
    notice: &'static str,
}

/// Third-party accounts on the account security page.
struct ExternalView {
    /// Linked ones, and ones open on this domain that can be linked; providers with neither are not listed.
    rows: Vec<ExternalRow>,
    /// A link back from authorization, waiting for the current password to confirm it.
    pending: Option<PendingLink>,
    /// Identity confirmed with a third party, so a password can be set: the provider name.
    confirmed: Option<&'static str>,
    message: String,
    notice: String,
}

struct PendingLink {
    value: &'static str,
    label: &'static str,
    display: String,
}

struct ExternalRow {
    value: &'static str,
    label: &'static str,
    /// Display name and link time when linked.
    bound: Option<(String, String)>,
    /// This domain allows this kind of login, so it can be linked or used to confirm identity.
    available: bool,
    /// The provider of the link that came back from authorization and is waiting for the password: this row no longer
    /// shows "not linked" or the link button.
    awaiting: bool,
}

/// Two-step verification state on the account security page; read from the database only when this page is opened.
struct TwoFactorView {
    enabled: String,
    recovery_remaining: i64,
    locked: String,
}

#[derive(Template)]
#[template(path = "settings/account.html")]
struct SecurityTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    message: &'a str,
    notice: &'a str,
    /// Errors of the two-step verification forms, shown in the two-step verification section rather than on the
    /// password change form at the top of the page.
    two_factor_message: &'a str,
    two_factor: Option<TwoFactorView>,
    mobile: MobileView,
    external: ExternalView,
    /// The account's email is not verified yet: linking, adding a phone number and turning on two-step verification
    /// wait until it is ([`identity::Member::email_unverified`]).
    email_unverified: bool,
}

pub(super) async fn account_response(
    state: &AppState,
    message: &str,
    status: StatusCode,
) -> Response {
    account_view(state, message, "", "", None, status).await
}

/// Used when the phone number and set-password forms fail: the message goes in the phone number section, and a step
/// whose code has been sent is still shown.
pub(super) async fn mobile_response(
    state: &AppState,
    headers: &HeaderMap,
    message: &str,
    status: StatusCode,
) -> Response {
    let mobile = MobileInput {
        headers: Some(headers),
        message,
        notice: "",
        external_message: "",
        external_notice: String::new(),
    };
    account_view(state, "", "", "", Some(mobile), status).await
}

/// Used when the third-party account forms and the authorization callback fail: the message goes in the third-party
/// account section, and a link awaiting confirmation is still shown.
pub(super) async fn external_response(
    state: &AppState,
    headers: &HeaderMap,
    message: &str,
    status: StatusCode,
) -> Response {
    let input = MobileInput {
        headers: Some(headers),
        message: "",
        notice: "",
        external_message: message,
        external_notice: String::new(),
    };
    account_view(state, "", "", "", Some(input), status).await
}

/// Inputs the phone number and third-party account sections need when the account security page opens: the request
/// headers (to read the step whose code has been sent and the link back from authorization awaiting confirmation) and
/// the message.
struct MobileInput<'a> {
    headers: Option<&'a HeaderMap>,
    message: &'a str,
    notice: &'static str,
    external_message: &'a str,
    external_notice: String,
}

/// Used when the forms to enable two-step verification, regenerate recovery codes or disable it fail: the message goes
/// in the two-step verification section.
pub(super) async fn two_factor_response(
    state: &AppState,
    message: &str,
    status: StatusCode,
) -> Response {
    account_view(state, "", "", message, None, status).await
}

async fn account_view(
    state: &AppState,
    message: &str,
    notice: &str,
    two_factor_message: &str,
    mobile: Option<MobileInput<'_>>,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    };
    let has_password = match state.identity.has_password(session.member.uid).await {
        Ok(has) => has,
        Err(error) => return identity_error(state, error),
    };
    let uid = session.member.uid;
    let headers = mobile.as_ref().and_then(|input| input.headers);
    let external_message = mobile
        .as_ref()
        .map_or_else(String::new, |input| input.external_message.to_owned());
    let external_notice = mobile
        .as_ref()
        .map_or_else(String::new, |input| input.external_notice.clone());
    let sent = |purpose| headers.and_then(|headers| super::sms::step(state, headers, uid, purpose));
    let mobile = MobileView {
        has_password,
        bound: session
            .member
            .mobile
            .as_deref()
            .map(identity::phone::mask_mobile),
        bind_sent_to: sent(identity::phone::SmsPurpose::Bind)
            .map(|step| identity::phone::mask_mobile(&step.mobile)),
        confirm_sent: sent(identity::phone::SmsPurpose::Confirm).is_some(),
        message: mobile
            .as_ref()
            .map_or_else(String::new, |input| input.message.to_owned()),
        notice: mobile.as_ref().map_or("", |input| input.notice),
    };
    let linked = match identity::external::linked_accounts(&state.db, uid).await {
        Ok(linked) => linked,
        Err(error) => return identity_error(state, error),
    };
    let pending_of = |purpose| {
        headers.and_then(|headers| super::external::pending(state, headers, purpose, uid))
    };
    let pending =
        pending_of(identity::external::FlowPurpose::Link).map(|(_, pending)| PendingLink {
            value: pending.provider.value(),
            label: pending.provider.label(),
            display: pending.display,
        });
    let available = super::external::shown(state).await;
    let rows = crate::oauth::Provider::ALL
        .into_iter()
        .filter_map(|provider| {
            let bound = linked
                .iter()
                .find(|link| link.provider == provider)
                .map(|link| (link.display.clone(), display_time(link.dateline)));
            let open = available.contains(&provider);
            (bound.is_some() || open).then_some(ExternalRow {
                value: provider.value(),
                label: provider.label(),
                bound,
                available: open,
                awaiting: pending
                    .as_ref()
                    .is_some_and(|pending| pending.value == provider.value()),
            })
        })
        .collect();
    let external = ExternalView {
        rows,
        pending,
        confirmed: pending_of(identity::external::FlowPurpose::Confirm)
            .filter(|_| !has_password)
            .map(|(_, pending)| pending.provider.label()),
        message: external_message,
        notice: external_notice,
    };
    let two_factor = match state.identity.two_factor_status(session.member.uid).await {
        Ok(status) => status,
        Err(error) => return identity_error(state, error),
    };
    let now = identity::now();
    let two_factor = two_factor.map(|status| TwoFactorView {
        enabled: display_time(status.enabled_at),
        recovery_remaining: status.recovery_remaining,
        locked: if status.locked_until > now {
            display_time(status.locked_until)
        } else {
            String::new()
        },
    });
    let mut p = meta(
        state,
        crate::i18n::current().frame_account_security().into(),
        crate::i18n::current()
            .settings_security_description(crate::site::site_name(&state.frame.settings)),
        "/my/account",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&SecurityTemplate {
        p,
        frame: crate::router::my::frame("account"),
        message,
        notice,
        two_factor_message,
        two_factor,
        mobile,
        external,
        email_unverified: session.member.email_unverified(),
    });
    *response.status_mut() = status;
    response
}

async fn account(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Response {
    let notice = if query.two_factor_disabled {
        crate::i18n::current().settings_notice_two_factor_disabled()
    } else if query.recovery_login {
        crate::i18n::current().settings_notice_recovery_login()
    } else {
        ""
    };
    let mobile_notice = if query.mobile_bound {
        crate::i18n::current().settings_notice_mobile_bound()
    } else if query.mobile_unbound {
        crate::i18n::current().settings_notice_mobile_unbound()
    } else {
        ""
    };
    let label =
        |value: &str| crate::oauth::Provider::parse(value).map(crate::oauth::Provider::label);
    let external_notice = if let Some(label) = label(&query.external_linked) {
        crate::i18n::current().settings_notice_external_linked(label)
    } else if let Some(label) = label(&query.external_unlinked) {
        crate::i18n::current().settings_notice_external_unlinked(label)
    } else {
        String::new()
    };
    let mobile = MobileInput {
        headers: Some(&headers),
        message: "",
        notice: mobile_notice,
        external_message: "",
        external_notice,
    };
    account_view(&state, "", notice, "", Some(mobile), StatusCode::OK).await
}

pub(super) fn signed_out(state: &AppState, path: &str) -> Response {
    let mut response = redirect(path);
    response.extensions_mut().insert(IdentityChanged);
    set_cookie(&mut response, state.viewer.secure, true, "", 0);
    set_cookie(&mut response, state.viewer.secure, false, "", 0);
    auxiliary_set_cookie(&mut response, "gonof_return", "", 0);
    response
}

async fn change_password(Page(state): Page, form: CheckedForm<SecurityForm>) -> Response {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return account_response(
                &state,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(..) => {
            return account_response(
                &state,
                crate::i18n::current().account_error_stale_password(),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    };
    let attempt = match password_attempt(&state, session.member.uid).await {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            return account_response(
                &state,
                crate::i18n::current().settings_error_password_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    };
    if form.password != form.password2 {
        if let Err(error) = attempt.release().await {
            return database_error(&state, error);
        }
        return account_response(
            &state,
            crate::i18n::current().account_error_new_password_mismatch(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    let check = state
        .identity
        .check_password_change(
            &state.viewer.raw_sid,
            &form.current_password,
            &form.password,
        )
        .await;
    let result = match password_result(attempt, check).await {
        Ok(verified) => state.identity.change_verified_password(verified).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => signed_out(&state, "/login?password_changed=true"),
        Err(IdentityError::Unauthorized) => signed_out(&state, "/login?session_ended=true"),
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            account_response(&state, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

/// Password changes and two-step verification share two buckets. The two-step verification confirmation form checks no
/// password; it only needs to see whether quota remains.
pub(super) async fn password_changes_blocked(state: &AppState, uid: i64) -> Result<bool, Response> {
    state
        .limit_blocked(&password_change_rules(state, uid))
        .await
}

pub(super) async fn password_attempt(
    state: &AppState,
    uid: i64,
) -> Result<Option<crate::rate_limit::FailureAttempt<'_>>, Response> {
    state
        .limits
        .reserve_failure(password_change_rules(state, uid).into())
        .await
        .map_err(|error| database_error(state, error))
}

/// The quota is kept only when the current password is wrong; other reservations are returned before the account
/// change is submitted.
pub(super) async fn password_result<T>(
    attempt: crate::rate_limit::FailureAttempt<'_>,
    result: Result<T, IdentityError>,
) -> Result<T, IdentityError> {
    // Recognized by the message in this request's language, the same call that produced it.
    if !matches!(
        &result,
        Err(IdentityError::Validation(message))
            if *message == crate::i18n::current().account_error_wrong_password()
    ) {
        attempt.release().await?;
    }
    result
}

/// Two buckets: per account and per IP, with a 5-minute window (constant); the counts are admin settings.
fn password_change_rules(state: &AppState, uid: i64) -> [Rule; 2] {
    let limits = &state.frame.settings.account;
    [
        Rule::new(
            "password-change-account",
            uid,
            attempts(limits.password_change_account_per_5min),
            300,
        ),
        Rule::new(
            "password-change-ip",
            &state.viewer.client_ip,
            attempts(limits.password_change_ip_per_5min),
            300,
        ),
    ]
}

#[derive(Template)]
#[template(path = "auth/forgot.html")]
struct ForgotTemplate<'a> {
    p: PageMeta<'a>,
    email: &'a str,
    message: &'a str,
    sent: bool,
}

fn forgot_response(
    state: &AppState,
    email: &str,
    message: &str,
    sent: bool,
    status: StatusCode,
) -> Response {
    let mut response = render(&ForgotTemplate {
        p: meta(
            state,
            crate::i18n::current().account_forgot_title().into(),
            crate::i18n::current()
                .account_forgot_description(crate::site::site_name(&state.frame.settings)),
            "/forgot-password",
            "",
            "",
        ),
        email,
        message,
        sent,
    });
    *response.status_mut() = status;
    response
}

async fn forgot_page(
    Page(state): Page,
    query: Result<Query<SecurityQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return forgot_response(
            &state,
            "",
            crate::i18n::current().account_error_page_params(),
            false,
            StatusCode::BAD_REQUEST,
        );
    };
    forgot_response(&state, "", "", query.sent, StatusCode::OK)
}

async fn forgot(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<Viewer>,
    form: CheckedForm<SecurityForm>,
) -> Response {
    let started = tokio::time::Instant::now();
    let state = match page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return response,
    };
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return forgot_response(
                &state,
                "",
                crate::i18n::current().common_error_form(),
                false,
                StatusCode::BAD_REQUEST,
            );
        }
        CheckedForm::StaleToken(form, reason) => {
            return forgot_response(
                &state,
                &form.email,
                &crate::i18n::current().account_error_stale_refresh(reason),
                false,
                StatusCode::FORBIDDEN,
            );
        }
    };
    let limits = &state.frame.settings.account;
    let email_key = identity::identifier_limit_key(&form.email);
    // Short-circuit level by level: a request over the IP limit writes no key into the e-mail bucket. E-mail addresses
    // can be made up freely by the submitter, so without short-circuiting a single IP could stuff unlimited new keys
    // into the rate limit table.
    // Three buckets in one round trip (one script when Redis is enabled).
    let rules = [
        Rule::new(
            "forgot-ip",
            &state.viewer.client_ip,
            attempts(limits.forgot_ip_per_hour),
            3600,
        ),
        Rule::new(
            "forgot-email-short",
            &email_key,
            attempts(limits.forgot_email_per_minute),
            60,
        ),
        Rule::new(
            "forgot-email-long",
            &email_key,
            attempts(limits.forgot_email_per_hour),
            3600,
        ),
    ];
    let response = match state.limit(&rules).await {
        Ok(false) => forgot_response(
            &state,
            &form.email,
            crate::i18n::current().account_error_apply_too_many(),
            false,
            StatusCode::TOO_MANY_REQUESTS,
        ),
        Ok(true) => match state.identity.request_password_reset(&form.email).await {
            Ok(())
            | Err(
                IdentityError::Validation(_)
                | IdentityError::Unauthorized
                | IdentityError::Forbidden
                | IdentityError::NotFound,
            ) => redirect("/forgot-password?sent=true"),
            Err(error) => identity_error(&state, error),
        },
        Err(unavailable) => unavailable,
    };
    // Whether the e-mail exists, can be recovered or is frozen does not change the response, and no fast-miss branch is
    // exposed.
    tokio::time::sleep_until(started + Duration::from_millis(250)).await;
    response
}

#[derive(Template)]
#[template(path = "auth/reset.html")]
struct ResetTemplate<'a> {
    p: PageMeta<'a>,
    token: &'a str,
    valid: bool,
    /// Resetting claims an account whose email was never verified ([`identity::ResetPreview`]); the page says so.
    claims: bool,
    message: &'a str,
    ttl: String,
}

/// `preview` is `None` when the link cannot be used.
fn reset_response(
    state: &AppState,
    token: &str,
    preview: Option<identity::ResetPreview>,
    message: &str,
    status: StatusCode,
) -> Response {
    let valid = preview.is_some();
    let mut response = render(&ResetTemplate {
        p: meta(
            state,
            crate::i18n::current().account_reset_title().into(),
            crate::i18n::current()
                .account_reset_description(crate::site::site_name(&state.frame.settings)),
            "/reset-password",
            "",
            "",
        ),
        token: if valid { token } else { "" },
        valid,
        claims: preview.is_some_and(|preview| preview.claims),
        message,
        ttl: display_duration(
            state
                .frame
                .settings
                .account
                .reset_ttl_minutes
                .saturating_mul(60),
        ),
    });
    *response.status_mut() = status;
    protect_reset_response(response)
}

fn protect_reset_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin"),
    );
    response
}

async fn reset_page(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<Viewer>,
    query: Result<Query<SecurityQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let state = match page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return protect_reset_response(response),
    };
    let Ok(Query(query)) = query else {
        return reset_response(
            &state,
            "",
            None,
            crate::i18n::current().account_error_reset_params(),
            StatusCode::BAD_REQUEST,
        );
    };
    match state.identity.password_reset_preview(&query.token).await {
        Ok(Some(preview)) => {
            reset_response(&state, &query.token, Some(preview), "", StatusCode::OK)
        }
        Ok(None) => reset_response(
            &state,
            "",
            None,
            crate::i18n::current().account_error_reset_invalid(),
            StatusCode::BAD_REQUEST,
        ),
        Err(error) => protect_reset_response(identity_error(&state, error)),
    }
}

async fn reset(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<Viewer>,
    form: CheckedForm<SecurityForm>,
) -> Response {
    let state = match page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return protect_reset_response(response),
    };
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return reset_response(
                &state,
                "",
                None,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            );
        }
        CheckedForm::StaleToken(form, reason) => {
            // Only the shape of the token is checked here; the page shown when the link was opened already said
            // whether resetting claims the account.
            let shaped =
                form.token.len() == 64 && form.token.bytes().all(|v| v.is_ascii_hexdigit());
            return reset_response(
                &state,
                &form.token,
                shaped.then_some(identity::ResetPreview { claims: false }),
                &crate::i18n::current().account_error_stale_new_password(reason),
                StatusCode::FORBIDDEN,
            );
        }
    };
    let limits = &state.frame.settings.account;
    match state
        .limit(&[
            Rule::new(
                "reset-ip",
                &state.viewer.client_ip,
                attempts(limits.reset_ip_per_5min),
                300,
            ),
            Rule::new(
                "reset-token",
                identity::token_hash(&form.token),
                attempts(limits.reset_token_per_5min),
                300,
            ),
        ])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return reset_response(
                &state,
                "",
                None,
                crate::i18n::current().account_error_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    let preview = match state.identity.password_reset_preview(&form.token).await {
        Ok(Some(preview)) => preview,
        Ok(None) => {
            return reset_response(
                &state,
                "",
                None,
                crate::i18n::current().account_error_reset_invalid(),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
        Err(error) => return protect_reset_response(identity_error(&state, error)),
    };
    if form.password != form.password2 {
        return reset_response(
            &state,
            &form.token,
            Some(preview),
            crate::i18n::current().account_error_new_password_mismatch(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    match state
        .identity
        .reset_password(&form.token, &form.password)
        .await
    {
        Ok(()) => protect_reset_response(signed_out(&state, "/login?password_reset=true")),
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            let preview = match state.identity.password_reset_preview(&form.token).await {
                Ok(preview) => preview,
                Err(error) => return protect_reset_response(identity_error(&state, error)),
            };
            reset_response(
                &state,
                &form.token,
                preview,
                if preview.is_some() {
                    message
                } else {
                    crate::i18n::current().account_error_reset_invalid()
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
        Err(error) => protect_reset_response(identity_error(&state, error)),
    }
}

async fn logout_all(Page(state): Page, CsrfForm(_): CsrfForm<ActionForm>) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    match state.identity.logout_all(&state.viewer.raw_sid).await {
        Ok(()) => signed_out(&state, "/login?logged_out_all=true"),
        Err(IdentityError::Unauthorized) => signed_out(&state, "/login?session_ended=true"),
        Err(error) => identity_error(&state, error),
    }
}

/// The account security rate limit counts come from `settings.account.*` (admin "Security › Account security",
/// docs/settings-and-caching.md, "Rate limits"); the window is in the key name and stays constant. Hard lower bounds
/// were already validated when decoding.
fn attempts(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}
