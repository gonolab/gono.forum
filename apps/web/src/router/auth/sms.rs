//! Web pages for SMS verification codes: SMS login, the "Finish signing up" step of SMS
//! registration, binding, changing and unbinding a mobile number under account security, and setting a password for
//! accounts without one. All are plain forms that work without JS.
//!
//! After a code is sent, the "enter the code" step keeps the number in an encrypted cookie
//! ([`identity::phone::SmsStep`]), so the form does not carry the number; a number verified by SMS but not yet
//! registered is kept in another encrypted cookie and read when registration is completed. Both cookies use the
//! `__Host-` prefix and `Path=/`.
use super::*;
use crate::identity::phone::{SmsLoginOutcome, SmsPurpose, SmsSendError, mask_mobile};
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, Page, csrf_forms};

const LOGIN_PATH: &str = "/login/sms";
const REGISTER_PATH: &str = "/register/mobile";
const ACCOUNT_MOBILE: &str = "/my/account#mobile";
const ACCOUNT_PASSWORD: &str = "/my/account#password";
/// Sending codes is also rate limited per IP: the window is 1 hour (a constant), the count is the admin setting
/// `settings.sms.ip_hourly_limit`.
const SEND_IP_WINDOW: i64 = 3600;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(LOGIN_PATH, get(login_page).post(login))
        .route("/login/sms/send", post(send))
        .route(REGISTER_PATH, get(register_page).post(register))
        .route("/my/account/mobile/send", post(mobile_send))
        .route("/my/account/mobile", post(mobile_bind))
        .route("/my/account/mobile/unbind", post(mobile_unbind))
        .route("/my/account/password/send", post(password_send))
        .route("/my/account/password/set", post(password_set))
}

fn step_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_sms"
    } else {
        "gonof_sms"
    }
}

fn signup_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_signup"
    } else {
        "gonof_signup"
    }
}

pub(super) fn set_named(
    response: &mut Response,
    name: &str,
    secure: bool,
    value: &str,
    max_age: i64,
) {
    let flag = if secure { "; Secure" } else { "" };
    if let Ok(header) = HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
        max_age.max(0)
    )) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

fn set_step(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    set_named(response, step_name(secure), secure, value, max_age);
}

fn set_signup(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    set_named(response, signup_name(secure), secure, value, max_age);
}

/// This request's "enter the code" step; `None` when the initiator or purpose does not match, or it has expired.
pub(super) fn step(
    state: &AppState,
    headers: &HeaderMap,
    uid: i64,
    purpose: SmsPurpose,
) -> Option<identity::phone::SmsStep> {
    let raw = unique_cookie(headers, step_name(state.viewer.secure), 2048)?;
    state.identity.open_sms_step(raw, uid, purpose)
}

fn signup_token(state: &AppState, headers: &HeaderMap) -> Option<String> {
    unique_cookie(headers, signup_name(state.viewer.secure), 2048).map(str::to_owned)
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

/// Message and status code for a failed code send.
fn send_failure(error: SmsSendError) -> Result<(String, StatusCode), IdentityError> {
    Ok(match error {
        SmsSendError::Wait(seconds) => (
            crate::i18n::current().account_error_sms_wait(seconds),
            StatusCode::TOO_MANY_REQUESTS,
        ),
        SmsSendError::MobileDaily => (
            crate::i18n::current()
                .account_error_sms_mobile_daily()
                .into(),
            StatusCode::TOO_MANY_REQUESTS,
        ),
        SmsSendError::SiteDaily => (
            crate::i18n::current().account_error_sms_site_daily().into(),
            StatusCode::TOO_MANY_REQUESTS,
        ),
        SmsSendError::Rejected(message) => (message.into(), StatusCode::UNPROCESSABLE_ENTITY),
        SmsSendError::Identity(IdentityError::Validation(message)) => {
            (message.into(), StatusCode::UNPROCESSABLE_ENTITY)
        }
        SmsSendError::Identity(IdentityError::Suspended(message)) => {
            (message, StatusCode::FORBIDDEN)
        }
        SmsSendError::Identity(error) => return Err(error),
    })
}

/// Number of code sends per IP per hour.
async fn send_allowed(state: &AppState) -> Result<bool, Response> {
    state
        .limit(&[Rule::new(
            "sms-send-ip",
            &state.viewer.client_ip,
            u32::try_from(state.frame.settings.sms.ip_hourly_limit).unwrap_or(0),
            SEND_IP_WINDOW,
        )])
        .await
}

#[derive(Default, Deserialize)]
struct SmsForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    mobile: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
    #[serde(default)]
    return_to: String,
}
csrf_forms!(SmsForm);

#[derive(Default, Deserialize)]
struct SmsQuery {
    #[serde(default)]
    return_to: String,
    #[serde(default)]
    expired: bool,
}

#[derive(Template)]
#[template(path = "auth/sms.html")]
struct SmsTemplate<'a> {
    p: PageMeta<'a>,
    return_to: &'a str,
    /// The `?return_to=…` carried by the tab links; empty string when absent.
    return_query: String,
    /// The masked number once a code has been sent; shows the "enter the code" step.
    sent_to: Option<String>,
    mobile: &'a str,
    message: &'a str,
    registration_open: bool,
}

fn sms_page(
    state: &AppState,
    headers: &HeaderMap,
    return_to: &str,
    mobile: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    let sent_to = step(state, headers, 0, SmsPurpose::Login).map(|step| mask_mobile(&step.mobile));
    let mut response = render(&SmsTemplate {
        p: meta(
            state,
            crate::i18n::current().account_sms_login().into(),
            crate::i18n::current()
                .account_sms_description(crate::site::site_name(&state.frame.settings)),
            LOGIN_PATH,
            "",
            "",
        ),
        return_to: &safe_return(return_to),
        return_query: with_return("", return_to),
        sent_to,
        mobile,
        message,
        registration_open: state.frame.settings.signup.mode != "closed",
    });
    *response.status_mut() = status;
    response
}

fn sms_disabled(state: &AppState) -> Option<Response> {
    (!crate::login::sms_enabled(&state.frame.settings)).then(|| {
        error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_error_sms_off(),
        )
    })
}

async fn login_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<SmsQuery>,
) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    if let Some(disabled) = sms_disabled(&state) {
        return disabled;
    }
    let message = if query.expired {
        crate::i18n::current().account_error_mobile_expired()
    } else {
        ""
    };
    sms_page(
        &state,
        &headers,
        &query.return_to,
        "",
        message,
        StatusCode::OK,
    )
}

fn stale(form: &CheckedForm<SmsForm>) -> Option<(&SmsForm, String)> {
    match form {
        CheckedForm::StaleToken(form, reason) => Some((form, reason.to_string())),
        _ => None,
    }
}

async fn send(Page(state): Page, headers: HeaderMap, form: CheckedForm<SmsForm>) -> Response {
    if let Some((form, reason)) = stale(&form) {
        return sms_page(
            &state,
            &headers,
            &form.return_to,
            &form.mobile,
            &crate::i18n::current().account_error_stale_reason(reason),
            StatusCode::FORBIDDEN,
        );
    }
    let CheckedForm::Valid(form) = form else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().common_error_form(),
        );
    };
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    if let Some(disabled) = sms_disabled(&state) {
        return disabled;
    }
    let mobile = match identity::phone::normalize_mobile(&form.mobile) {
        Ok(mobile) => mobile,
        Err(IdentityError::Validation(message)) => {
            return sms_page(
                &state,
                &headers,
                &form.return_to,
                &form.mobile,
                message,
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
        Err(error) => return identity_error(&state, error),
    };
    match send_allowed(&state).await {
        Ok(true) => {}
        Ok(false) => {
            return sms_page(
                &state,
                &headers,
                &form.return_to,
                &form.mobile,
                crate::i18n::current().account_error_sms_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .send_sms_code(&mobile, SmsPurpose::Login, &state.viewer.client_ip)
        .await
    {
        Ok(()) => {}
        Err(error) => {
            return match send_failure(error) {
                Ok((message, status)) => sms_page(
                    &state,
                    &headers,
                    &form.return_to,
                    &form.mobile,
                    &message,
                    status,
                ),
                Err(error) => identity_error(&state, error),
            };
        }
    }
    let sealed = match state.identity.seal_sms_step(0, &mobile, SmsPurpose::Login) {
        Ok(sealed) => sealed,
        Err(error) => return identity_error(&state, error),
    };
    let mut response = redirect(&with_return(LOGIN_PATH, &form.return_to));
    set_step(
        &mut response,
        state.viewer.secure,
        &sealed,
        identity::phone::STEP_TTL,
    );
    response
}

async fn login(Page(state): Page, headers: HeaderMap, form: CheckedForm<SmsForm>) -> Response {
    if let Some((form, reason)) = stale(&form) {
        return sms_page(
            &state,
            &headers,
            &form.return_to,
            "",
            &crate::i18n::current().account_error_stale_code(reason),
            StatusCode::FORBIDDEN,
        );
    }
    let CheckedForm::Valid(form) = form else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().common_error_form(),
        );
    };
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    if let Some(disabled) = sms_disabled(&state) {
        return disabled;
    }
    let Some(step) = step(&state, &headers, 0, SmsPurpose::Login) else {
        return sms_page(
            &state,
            &headers,
            &form.return_to,
            "",
            crate::i18n::current().account_error_code_first(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
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
            return sms_page(
                &state,
                &headers,
                &form.return_to,
                "",
                crate::i18n::current().account_error_login_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    let metadata = session_metadata(&state, &headers).with_device(&device_marker(&state, &headers));
    let destination = safe_return(&form.return_to);
    match state
        .identity
        .sms_login(&step.mobile, &form.code, &metadata)
        .await
    {
        Ok(SmsLoginOutcome::Login(identity::LoginOutcome::Session(grant))) => {
            let mut response = grant_response(
                &state,
                *grant,
                if destination.is_empty() {
                    "/"
                } else {
                    &destination
                },
            );
            set_step(&mut response, state.viewer.secure, "", 0);
            response
        }
        Ok(SmsLoginOutcome::Login(identity::LoginOutcome::SecondFactor(pending))) => {
            let mut response = redirect(&with_return("/login/verify", &destination));
            two_factor::set_pending(
                &mut response,
                state.viewer.secure,
                &pending.token,
                identity::SECOND_FACTOR_PENDING_TTL,
            );
            set_step(&mut response, state.viewer.secure, "", 0);
            response
        }
        Ok(SmsLoginOutcome::Register(token)) => {
            let mut response = redirect(&with_return(REGISTER_PATH, &destination));
            set_signup(
                &mut response,
                state.viewer.secure,
                &token,
                identity::phone::REGISTRATION_TTL,
            );
            set_step(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => sms_page(
            &state,
            &headers,
            &form.return_to,
            "",
            message,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        Err(IdentityError::Suspended(message)) => sms_page(
            &state,
            &headers,
            &form.return_to,
            "",
            &message,
            StatusCode::FORBIDDEN,
        ),
        Err(IdentityError::Unauthorized | IdentityError::Forbidden) => sms_page(
            &state,
            &headers,
            &form.return_to,
            "",
            crate::i18n::current().account_error_cannot_log_in(),
            StatusCode::UNAUTHORIZED,
        ),
        Err(error) => identity_error(&state, error),
    }
}

#[derive(Template)]
#[template(path = "auth/mobile_register.html")]
struct RegisterTemplate<'a> {
    p: PageMeta<'a>,
    mobile: String,
    form: &'a AuthForm,
    invitation_required: bool,
    message: &'a str,
}

fn register_response(
    state: &AppState,
    mobile: &str,
    form: &AuthForm,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut response = render(&RegisterTemplate {
        p: meta(
            state,
            crate::i18n::current().account_finish_signup().into(),
            crate::i18n::current()
                .account_mobile_description(crate::site::site_name(&state.frame.settings)),
            REGISTER_PATH,
            "",
            "",
        ),
        mobile: mask_mobile(mobile),
        form,
        invitation_required: state.frame.settings.signup.mode == "invite",
        message,
    });
    *response.status_mut() = status;
    response
}

async fn register_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<SmsQuery>,
) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    let Some(mobile) =
        signup_token(&state, &headers).and_then(|token| state.identity.registration_mobile(&token))
    else {
        return redirect(&format!("{LOGIN_PATH}?expired=true"));
    };
    let mut form = AuthForm {
        return_to: safe_return(&query.return_to),
        ..Default::default()
    };
    if let Err(error) = show_policies(&state, &mut form).await {
        return identity_error(&state, error);
    }
    register_response(&state, &mobile, &form, "", StatusCode::OK)
}

async fn register(Page(state): Page, headers: HeaderMap, form: CheckedForm<AuthForm>) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    let token = signup_token(&state, &headers);
    let Some(mobile) = token
        .as_deref()
        .and_then(|token| state.identity.registration_mobile(token))
    else {
        return redirect(&format!("{LOGIN_PATH}?expired=true"));
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
                &mobile,
                &form,
                &crate::i18n::current().account_error_stale_reason(reason),
                StatusCode::FORBIDDEN,
            );
        }
    };
    // Without the consent checkbox the registration transaction is not entered (same as password registration).
    if form.accept_terms != "1" {
        return register_response(
            &state,
            &mobile,
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
                &mobile,
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
                &mobile,
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
    let with_email = !form.email.trim().is_empty();
    match state
        .identity
        .register_with_mobile(
            token.as_deref().unwrap_or_default(),
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
            let path = if with_email {
                "/verify-email"
            } else if destination.is_empty() {
                "/"
            } else {
                &destination
            };
            let mut response = grant_response(&state, grant, path);
            set_signup(&mut response, state.viewer.secure, "", 0);
            if channel.is_some() {
                registration_source::write(&mut response, &state.viewer, None);
            }
            if with_email {
                auxiliary_set_cookie(&mut response, "gonof_return", &destination, 3600);
            }
            response
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            register_response(
                &state,
                &mobile,
                &form,
                message,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
        Err(IdentityError::Suspended(message)) => {
            register_response(&state, &mobile, &form, &message, StatusCode::FORBIDDEN)
        }
        Err(error) => identity_error(&state, error),
    }
}

/// Common precondition for these forms on the account security page: the form is valid and the visitor is logged in.
fn account_form(
    state: &AppState,
    form: CheckedForm<SmsForm>,
) -> Result<(SmsForm, &identity::AuthSession), &'static str> {
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

async fn mobile_failure(
    state: &AppState,
    headers: &HeaderMap,
    message: &str,
    status: StatusCode,
) -> Response {
    security::mobile_response(state, headers, message, status).await
}

async fn mobile_send(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<SmsForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return mobile_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let uid = session.member.uid;
    let mobile = match identity::phone::normalize_mobile(&form.mobile) {
        Ok(mobile) => mobile,
        Err(IdentityError::Validation(message)) => {
            return mobile_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY)
                .await;
        }
        Err(error) => return identity_error(&state, error),
    };
    // No code is sent to a number that could not be bound (checked again under the member row lock when binding).
    if session.member.email_unverified() {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_verify_email_first(),
            StatusCode::CONFLICT,
        )
        .await;
    }
    if session.member.mobile.as_deref() == Some(mobile.as_str()) {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_mobile_linked_here(),
            StatusCode::CONFLICT,
        )
        .await;
    }
    send_then_step(
        &state,
        &headers,
        uid,
        &mobile,
        SmsPurpose::Bind,
        ACCOUNT_MOBILE,
    )
    .await
}

/// After a code is sent, records this step and returns to the account security page; on failure the message stays
/// in the mobile number section.
async fn send_then_step(
    state: &AppState,
    headers: &HeaderMap,
    uid: i64,
    mobile: &str,
    purpose: SmsPurpose,
    back: &str,
) -> Response {
    match send_allowed(state).await {
        Ok(true) => {}
        Ok(false) => {
            return mobile_failure(
                state,
                headers,
                crate::i18n::current().account_error_sms_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    if let Err(error) = state
        .identity
        .send_sms_code(mobile, purpose, &state.viewer.client_ip)
        .await
    {
        return match send_failure(error) {
            Ok((message, status)) => mobile_failure(state, headers, &message, status).await,
            Err(error) => identity_error(state, error),
        };
    }
    let sealed = match state.identity.seal_sms_step(uid, mobile, purpose) {
        Ok(sealed) => sealed,
        Err(error) => return identity_error(state, error),
    };
    let mut response = redirect(back);
    set_step(
        &mut response,
        state.viewer.secure,
        &sealed,
        identity::phone::STEP_TTL,
    );
    response
}

async fn mobile_bind(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<SmsForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return mobile_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let uid = session.member.uid;
    let Some(step) = step(&state, &headers, uid, SmsPurpose::Bind) else {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_code_first(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let attempt = match security::password_attempt(&state, uid).await {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            return mobile_failure(
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
        .bind_mobile(
            &state.viewer.raw_sid,
            &form.current_password,
            &step.mobile,
            &form.code,
        )
        .await;
    match security::password_result(attempt, result).await {
        Ok(()) => {
            let mut response = redirect("/my/account?mobile_bound=true#mobile");
            response.extensions_mut().insert(IdentityChanged);
            set_step(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            mobile_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn mobile_unbind(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<SmsForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return mobile_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let attempt = match security::password_attempt(&state, session.member.uid).await {
        Ok(Some(attempt)) => attempt,
        Ok(None) => {
            return mobile_failure(
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
        .unbind_mobile(&state.viewer.raw_sid, &form.current_password)
        .await;
    match security::password_result(attempt, result).await {
        Ok(()) => {
            let mut response = redirect("/my/account?mobile_unbound=true#mobile");
            response.extensions_mut().insert(IdentityChanged);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            mobile_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn password_send(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<SmsForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (_, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return mobile_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    let Some(mobile) = session.member.mobile.clone() else {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_mobile_first(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let uid = session.member.uid;
    send_then_step(
        &state,
        &headers,
        uid,
        &mobile,
        SmsPurpose::Confirm,
        ACCOUNT_PASSWORD,
    )
    .await
}

async fn password_set(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<SmsForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Faccount");
    }
    let (form, session) = match account_form(&state, form) {
        Ok(pair) => pair,
        Err(message) => {
            return mobile_failure(&state, &headers, message, StatusCode::FORBIDDEN).await;
        }
    };
    if step(&state, &headers, session.member.uid, SmsPurpose::Confirm).is_none() {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_code_first(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    if form.password != form.password2 {
        return mobile_failure(
            &state,
            &headers,
            crate::i18n::current().account_error_password_mismatch(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    match state
        .identity
        .set_password_with_sms(&state.viewer.raw_sid, &form.code, &form.password)
        .await
    {
        Ok(()) => {
            let mut response = security::signed_out(&state, "/login?password_set=true");
            set_step(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            mobile_failure(&state, &headers, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}
