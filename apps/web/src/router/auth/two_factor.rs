//! Two-factor authentication web pages: the second sign-in step, enabling / disabling / regenerating recovery codes
//! on the account security page, and the one-time display page for new recovery codes. All are native forms, usable
//! without JS.
use super::*;
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, Page, csrf_forms};

/// Display page for new recovery codes; the display Cookie is sent only to this path.
const RECOVERY_PATH: &str = "/my/account/recovery-codes";
const LOGIN_RETURN: &str = "/login?return_to=%2Fmy%2Faccount";
const EXPIRED_LOGIN: &str = "/login?second_factor_expired=true";
/// The second step is additionally rate-limited per IP over a 5-minute window (constant); the count is the admin
/// setting `settings.account.two_factor_ip_per_5min`. Per-account failure counts and lockouts are stored in the
/// database.
const SECOND_FACTOR_IP_WINDOW: i64 = 300;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login/verify", get(verify_page).post(verify))
        .route("/my/account/two-factor/setup", post(setup))
        .route("/my/account/two-factor/enable", post(enable))
        .route("/my/account/two-factor/disable", post(disable))
        .route(RECOVERY_PATH, get(recovery_page).post(regenerate))
}

/// Cookie for awaiting the second step: the same `__Host-` and Secure rules as the session Cookie.
fn pending_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_2fa"
    } else {
        "gonof_2fa"
    }
}

pub(super) fn set_pending(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    let flag = if secure { "; Secure" } else { "" };
    if let Ok(header) = HeaderValue::from_str(&format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
        pending_name(secure),
        max_age.max(0)
    )) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

fn pending(state: &AppState, headers: &HeaderMap) -> Option<String> {
    unique_cookie(headers, pending_name(state.viewer.secure), 2048).map(str::to_owned)
}

/// The display Cookie for new recovery codes is sent only to the display page, so it cannot use the `__Host-` prefix,
/// which requires `Path=/`.
fn reveal_name(secure: bool) -> &'static str {
    if secure {
        "__Secure-gonof_recovery"
    } else {
        "gonof_recovery"
    }
}

fn set_reveal(response: &mut Response, secure: bool, value: &str, max_age: i64) {
    let flag = if secure { "; Secure" } else { "" };
    if let Ok(header) = HeaderValue::from_str(&format!(
        "{}={value}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
        reveal_name(secure),
        // Sent only to the display page, with this request's path prefix.
        crate::site::href(RECOVERY_PATH),
        max_age.max(0)
    )) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

fn locked_message(until: i64) -> String {
    crate::i18n::current().account_error_two_factor_locked(display_time(until))
}

#[derive(Default, Deserialize)]
struct VerifyForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    return_to: String,
}
csrf_forms!(VerifyForm);

#[derive(Template)]
#[template(path = "auth/second_factor.html")]
struct VerifyTemplate<'a> {
    p: PageMeta<'a>,
    return_to: &'a str,
    message: &'a str,
    /// The last submission was a recovery code: when re-rendering, the recovery code section stays expanded so it need
    /// not be clicked again.
    recovery: bool,
}

/// When the submitted content is not 6 digits (a code), it is handled as the recovery code section. This only decides
/// how the page expands; the check is still judged by `Identity::complete_login` from the input's shape.
fn recovery_attempt(code: &str) -> bool {
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    !compact.is_empty() && !(compact.len() == 6 && compact.bytes().all(|b| b.is_ascii_digit()))
}

fn verify_response(
    state: &AppState,
    return_to: &str,
    message: &str,
    recovery: bool,
    status: StatusCode,
) -> Response {
    let mut response = render(&VerifyTemplate {
        p: meta(
            state,
            crate::i18n::current().account_two_factor().into(),
            crate::i18n::current()
                .account_two_factor_description(crate::site::site_name(&state.frame.settings)),
            "/login/verify",
            "",
            "",
        ),
        return_to: &safe_return(return_to),
        message,
        recovery,
    });
    *response.status_mut() = status;
    response
}

fn expired(state: &AppState) -> Response {
    let mut response = redirect(EXPIRED_LOGIN);
    set_pending(&mut response, state.viewer.secure, "", 0);
    response
}

async fn verify_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<ReturnQuery>,
) -> Response {
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    if !pending(&state, &headers).is_some_and(|raw| state.identity.pending_login_valid(&raw)) {
        return expired(&state);
    }
    verify_response(&state, &query.return_to, "", false, StatusCode::OK)
}

async fn verify(Page(state): Page, headers: HeaderMap, form: CheckedForm<VerifyForm>) -> Response {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().common_error_form(),
            );
        }
        CheckedForm::StaleToken(form, reason) => {
            return verify_response(
                &state,
                &form.return_to,
                &crate::i18n::current().account_error_stale_code(reason),
                recovery_attempt(&form.code),
                StatusCode::FORBIDDEN,
            );
        }
    };
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    let Some(raw) = pending(&state, &headers) else {
        return expired(&state);
    };
    match state
        .limit(&[Rule::new(
            "second-factor-ip",
            &state.viewer.client_ip,
            u32::try_from(state.frame.settings.account.two_factor_ip_per_5min).unwrap_or(0),
            SECOND_FACTOR_IP_WINDOW,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return verify_response(
                &state,
                &form.return_to,
                crate::i18n::current().account_error_too_many(),
                recovery_attempt(&form.code),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    let metadata = session_metadata(&state, &headers).with_device(&device_marker(&state, &headers));
    match state
        .identity
        .complete_login(&raw, &form.code, &metadata)
        .await
    {
        Ok(identity::SecondFactorOutcome::Granted {
            grant,
            recovery_remaining,
        }) => {
            // After signing in with a recovery code, go to the account security page first, which shows how many are
            // left and suggests regenerating.
            let destination = if recovery_remaining.is_some() {
                "/my/account?recovery_login=true".to_owned()
            } else {
                Some(safe_return(&form.return_to))
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| "/".into())
            };
            let mut response = grant_response(&state, *grant, &destination);
            set_pending(&mut response, state.viewer.secure, "", 0);
            response
        }
        Ok(identity::SecondFactorOutcome::Rejected) => verify_response(
            &state,
            &form.return_to,
            crate::i18n::current().account_error_wrong_answer(),
            recovery_attempt(&form.code),
            StatusCode::UNAUTHORIZED,
        ),
        Ok(identity::SecondFactorOutcome::Locked { until }) => verify_response(
            &state,
            &form.return_to,
            &locked_message(until),
            recovery_attempt(&form.code),
            StatusCode::TOO_MANY_REQUESTS,
        ),
        Ok(identity::SecondFactorOutcome::Unavailable) => verify_response(
            &state,
            &form.return_to,
            crate::i18n::current().account_error_authenticator_unavailable(),
            recovery_attempt(&form.code),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        Err(IdentityError::Unauthorized) => expired(&state),
        Err(IdentityError::Suspended(message)) => {
            let mut response = account_page(
                &state,
                false,
                &AuthForm::default(),
                &message,
                StatusCode::FORBIDDEN,
            )
            .await;
            set_pending(&mut response, state.viewer.secure, "", 0);
            response
        }
        Err(error) => identity_error(&state, error),
    }
}

#[derive(Default, Deserialize)]
struct TwoFactorForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    token: String,
}
csrf_forms!(TwoFactorForm);

#[derive(Template)]
#[template(path = "settings/two_factor_setup.html")]
struct SetupTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    setup: &'a identity::TotpSetup,
    username: &'a str,
    message: &'a str,
}

impl SetupTemplate<'_> {
    /// The manual-entry sentence: the add-to-app link (an `otpauth:` address, so no site prefix) and the account name
    /// to type in are markup slots of the translation.
    fn manual_entry(&self) -> crate::i18n::Markup {
        use crate::i18n::Markup;
        self.p.t.settings_two_factor_manual(
            Markup::trusted(format!(
                "<a href=\"{}\">{}</a>",
                Markup::text(&crate::site::href(&self.setup.uri)),
                Markup::text(self.p.t.settings_two_factor_add_link())
            )),
            Markup::trusted(format!(
                "<span translate=\"no\">{}</span>",
                Markup::text(self.username)
            )),
        )
    }
}

async fn setup_response(
    state: &AppState,
    setup: &identity::TotpSetup,
    message: &str,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect(LOGIN_RETURN);
    };
    let mut p = meta(
        state,
        crate::i18n::current().settings_two_factor_enable().into(),
        crate::i18n::current()
            .settings_two_factor_description(crate::site::site_name(&state.frame.settings)),
        "/my/account",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&SetupTemplate {
        p,
        frame: crate::router::my::frame("account"),
        setup,
        username: &session.member.username,
        message,
    });
    *response.status_mut() = status;
    response
}

/// Common preliminary checks for form format, CSRF and sign-in.
async fn checked(
    state: &AppState,
    form: CheckedForm<TwoFactorForm>,
) -> Result<(TwoFactorForm, i64), Response> {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return Err(security::two_factor_response(
                state,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            )
            .await);
        }
        CheckedForm::StaleToken(..) => {
            return Err(security::two_factor_response(
                state,
                crate::i18n::current().settings_error_stale_reenter(),
                StatusCode::FORBIDDEN,
            )
            .await);
        }
    };
    let Some(session) = &state.viewer.session else {
        return Err(redirect(LOGIN_RETURN));
    };
    Ok((form, session.member.uid))
}

async fn too_many_changes(state: &AppState) -> Response {
    security::two_factor_response(
        state,
        crate::i18n::current().account_error_too_many(),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await
}

/// Reserves account and IP slots before checking the password; on successful verification they are returned first,
/// and only then does the credentials write transaction start.
async fn checked_password<'a>(
    state: &'a AppState,
    form: CheckedForm<TwoFactorForm>,
) -> Result<(TwoFactorForm, identity::VerifiedPassword<'a>), Response> {
    let (form, uid) = checked(state, form).await?;
    let Some(attempt) = security::password_attempt(state, uid).await? else {
        return Err(too_many_changes(state).await);
    };
    let check = state
        .identity
        .check_current_password(&state.viewer.raw_sid, &form.current_password)
        .await;
    match security::password_result(attempt, check).await {
        Ok(verified) => Ok((form, verified)),
        Err(error) => Err(failure(state, error).await),
    }
}

async fn failure(state: &AppState, error: IdentityError) -> Response {
    match error {
        IdentityError::Unauthorized => security::signed_out(state, "/login?session_ended=true"),
        IdentityError::Validation(message) => {
            security::two_factor_response(state, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        IdentityError::Conflict(message) => {
            security::two_factor_response(state, message, StatusCode::CONFLICT).await
        }
        error => identity_error(state, error),
    }
}

async fn setup(Page(state): Page, form: CheckedForm<TwoFactorForm>) -> Response {
    let (_, verified) = match checked_password(&state, form).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    match state
        .identity
        .begin_verified_two_factor_setup(verified)
        .await
    {
        Ok(setup) => setup_response(&state, &setup, "", StatusCode::OK).await,
        Err(error) => failure(&state, error).await,
    }
}

async fn enable(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<TwoFactorForm>,
) -> Response {
    let (form, uid) = match checked(&state, form).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    match security::password_changes_blocked(&state, uid).await {
        Ok(true) => return too_many_changes(&state).await,
        Ok(false) => {}
        Err(unavailable) => return unavailable,
    }
    let metadata = session_metadata(&state, &headers);
    match state
        .identity
        .enable_two_factor(&state.viewer.raw_sid, &form.token, &form.code, &metadata)
        .await
    {
        Ok(enabled) => {
            // Credentials version +1: the current device switches to a new session and other devices are signed out.
            let mut response = grant_response(
                &state,
                enabled.grant,
                &format!("{RECOVERY_PATH}?enabled=true"),
            );
            set_reveal(
                &mut response,
                state.viewer.secure,
                &enabled.reveal,
                identity::RECOVERY_REVEAL_TTL,
            );
            response
        }
        Err(IdentityError::Validation(message)) => {
            // Wrong code while the setup has not expired yet: try again with the same QR code.
            let resumed = match state.viewer.session.as_ref() {
                Some(session) => {
                    state
                        .identity
                        .resume_two_factor_setup(&session.member, &form.token)
                        .await
                }
                None => None,
            };
            match resumed {
                Some(Ok(setup)) => {
                    setup_response(&state, &setup, message, StatusCode::UNPROCESSABLE_ENTITY).await
                }
                Some(Err(error)) => identity_error(&state, error),
                None => {
                    security::two_factor_response(&state, message, StatusCode::UNPROCESSABLE_ENTITY)
                        .await
                }
            }
        }
        Err(error) => failure(&state, error).await,
    }
}

async fn disable(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<TwoFactorForm>,
) -> Response {
    let (form, verified) = match checked_password(&state, form).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    let metadata = session_metadata(&state, &headers);
    match state
        .identity
        .disable_two_factor_verified(verified, &form.code, &metadata)
        .await
    {
        Ok(identity::TwoFactorDisable::Disabled) => {
            redirect("/my/account?two_factor_disabled=true")
        }
        Ok(identity::TwoFactorDisable::Rejected) => {
            security::two_factor_response(
                &state,
                crate::i18n::current().account_error_wrong_answer(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Ok(identity::TwoFactorDisable::Locked { until }) => {
            security::two_factor_response(
                &state,
                &locked_message(until),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await
        }
        Ok(identity::TwoFactorDisable::Unavailable) => {
            security::two_factor_response(
                &state,
                crate::i18n::current().account_error_authenticator_unavailable(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(error) => failure(&state, error).await,
    }
}

async fn regenerate(
    Page(state): Page,
    headers: HeaderMap,
    form: CheckedForm<TwoFactorForm>,
) -> Response {
    let (_, verified) = match checked_password(&state, form).await {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    let metadata = session_metadata(&state, &headers);
    match state
        .identity
        .regenerate_recovery_codes_verified(verified, &metadata)
        .await
    {
        Ok(reveal) => {
            let mut response = redirect(&format!("{RECOVERY_PATH}?regenerated=true"));
            set_reveal(
                &mut response,
                state.viewer.secure,
                &reveal,
                identity::RECOVERY_REVEAL_TTL,
            );
            response
        }
        Err(error) => failure(&state, error).await,
    }
}

#[derive(Default, Deserialize)]
struct RecoveryQuery {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    regenerated: bool,
}

#[derive(Template)]
#[template(path = "settings/recovery_codes.html")]
struct RecoveryTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    codes: Vec<String>,
    notice: &'a str,
}

/// New recovery codes are shown only once: the display Cookie is cleared as soon as it is read, so they do not appear
/// after a refresh.
async fn recovery_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<RecoveryQuery>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect(LOGIN_RETURN);
    };
    let codes = unique_cookie(&headers, reveal_name(state.viewer.secure), 2048)
        .and_then(|raw| {
            state
                .identity
                .revealed_recovery_codes(raw, session.member.uid)
        })
        .unwrap_or_default();
    let notice = match (query.enabled, query.regenerated, codes.is_empty()) {
        (_, _, true) => "",
        (true, _, false) => crate::i18n::current().settings_notice_two_factor_enabled(),
        (_, true, false) => crate::i18n::current().settings_notice_recovery_regenerated(),
        _ => "",
    };
    let mut p = meta(
        &state,
        crate::i18n::current().settings_recovery_codes().into(),
        crate::i18n::current()
            .settings_recovery_description(crate::site::site_name(&state.frame.settings)),
        RECOVERY_PATH,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    let mut response = render(&RecoveryTemplate {
        p,
        frame: crate::router::my::frame("account"),
        codes,
        notice,
    });
    set_reveal(&mut response, state.viewer.secure, "", 0);
    response
}
