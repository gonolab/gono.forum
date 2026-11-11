//! Member API token settings: generating one requires the current password, and the token's plaintext is shown only
//! once. A successful generation does not render the plaintext directly: it goes into a one-time encrypted cookie sent
//! only to this page, with a 303 back to GET this page, and is cleared once read; refreshing does not resubmit the
//! generation form (see `identity::SecretReveal`).
use super::*;
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, CsrfForm, Page, csrf_forms};

const KEY_ACCOUNT_LIMIT: (u32, i64) = (5, 300);
const KEY_IP_LIMIT: (u32, i64) = (30, 300);
const LOGIN_RETURN: &str = "/login?return_to=%2Fmy%2Fapi";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/api", get(api_page))
        .route("/my/api/enable", post(generate))
        .route("/my/api/rotate", post(generate))
        .route("/my/api/disable", post(disable))
}

#[derive(Default, Deserialize)]
struct ApiForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    current_password: String,
}
csrf_forms!(ApiForm);

#[derive(Default, Deserialize)]
struct ApiQuery {
    #[serde(default)]
    disabled: bool,
}

struct KeyView {
    hint: String,
    created: String,
    expires: String,
    last_used: String,
    last_used_ip: String,
    expired: bool,
}

#[derive(Template)]
#[template(path = "settings/api.html")]
struct ApiTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    endpoint: String,
    key: Option<KeyView>,
    secret: &'a str,
    message: &'a str,
    success: bool,
    can_enable: bool,
    site_enabled: bool,
    ttl_days: i64,
}

async fn api_response(
    state: &AppState,
    secret: &str,
    message: &str,
    success: bool,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect(LOGIN_RETURN);
    };
    let key = match state.identity.api_key(session.member.uid).await {
        Ok(key) => key,
        Err(error) => return identity_error(state, error),
    };
    let now = identity::now();
    // The plaintext in the reveal cookie must still be the currently valid token: if it was regenerated or disabled
    // since, it is no longer shown.
    let secret = match &key {
        Some(key) if !secret.is_empty() && key.active_at(now) && secret.starts_with(&key.hint) => {
            secret
        }
        _ => "",
    };
    let (message, success) = if secret.is_empty() {
        (message, success)
    } else {
        (crate::i18n::current().settings_api_created_notice(), true)
    };
    let key = key.map(|key| KeyView {
        expired: !key.active_at(now),
        hint: key.hint,
        created: display_time(key.dateline),
        expires: display_time(key.expires_at),
        last_used: if key.last_used_at > 0 {
            display_time(key.last_used_at)
        } else {
            crate::i18n::current().settings_api_never_used().into()
        },
        last_used_ip: key.last_used_ip,
    });
    let mut p = meta(
        state,
        crate::i18n::current().settings_api_title().into(),
        crate::i18n::current()
            .settings_api_description(crate::site::site_name(&state.frame.settings)),
        "/my/api",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&ApiTemplate {
        frame: crate::router::my::frame("api"),
        p,
        endpoint: format!("{}{}", state.site_url(), crate::router::api::PREFIX),
        key,
        secret,
        message,
        success,
        can_enable: session.member.can_contribute(),
        site_enabled: state.frame.settings.api.enabled,
        ttl_days: state.frame.settings.api.key_ttl_days,
    });
    *response.status_mut() = status;
    response
}

/// The one GET after generation takes the plaintext from the reveal cookie, shows it and clears the cookie: it does
/// not appear again on refresh.
async fn api_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<ApiQuery>,
) -> Response {
    let (message, success) = if query.disabled {
        (crate::i18n::current().settings_api_disabled_notice(), true)
    } else {
        ("", false)
    };
    let secret = state
        .viewer
        .session
        .as_ref()
        .and_then(|session| super::API_REVEAL.read(&state, &headers, session.member.uid))
        .unwrap_or_default();
    let mut response = api_response(&state, &secret, message, success, StatusCode::OK).await;
    super::API_REVEAL.set(&mut response, state.viewer.secure, "", 0);
    response
}

/// Enabling and regenerating share one transaction: the old token is revoked under lock; the new token's plaintext goes
/// into the reveal cookie, and a 303 back to this page shows it once.
async fn generate(Page(state): Page, form: CheckedForm<ApiForm>) -> Response {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return api_response(
                &state,
                "",
                crate::i18n::current().common_error_form(),
                false,
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(..) => {
            return api_response(
                &state,
                "",
                crate::i18n::current().account_error_stale_password(),
                false,
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    let Some(session) = &state.viewer.session else {
        return redirect(LOGIN_RETURN);
    };
    if !state.frame.settings.api.enabled {
        return api_response(
            &state,
            "",
            crate::i18n::current().settings_error_api_site_disabled(),
            false,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    if !session.member.can_contribute() {
        return api_response(
            &state,
            "",
            crate::login::Participation::current()
                .message(crate::i18n::current().settings_error_api_need_email()),
            false,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    match state
        .limit(&[
            Rule::new(
                "api-key-account",
                session.member.uid,
                KEY_ACCOUNT_LIMIT.0,
                KEY_ACCOUNT_LIMIT.1,
            ),
            Rule::new(
                "api-key-ip",
                &state.viewer.client_ip,
                KEY_IP_LIMIT.0,
                KEY_IP_LIMIT.1,
            ),
        ])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return api_response(
                &state,
                "",
                crate::i18n::current().settings_error_too_frequent(),
                false,
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .create_api_key(
            &state.viewer.raw_sid,
            &form.current_password,
            state.frame.settings.api.key_ttl_days.saturating_mul(86_400),
        )
        .await
    {
        Ok(grant) => {
            super::API_REVEAL.redirect(&state, session.member.uid, &grant.secret, "/my/api")
        }
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            api_response(&state, "", message, false, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        // The API was switched off while the password was being checked.
        Err(IdentityError::Suspended(message)) => {
            api_response(&state, "", &message, false, StatusCode::FORBIDDEN).await
        }
        Err(IdentityError::Forbidden) => {
            api_response(
                &state,
                "",
                crate::login::Participation::current()
                    .message(crate::i18n::current().settings_error_api_need_email()),
                false,
                StatusCode::FORBIDDEN,
            )
            .await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn disable(Page(state): Page, CsrfForm(_): CsrfForm<ActionForm>) -> Response {
    if state.viewer.session.is_none() {
        return redirect(LOGIN_RETURN);
    }
    match state.identity.revoke_api_key(&state.viewer.raw_sid).await {
        Ok(_) => redirect("/my/api?disabled=true"),
        Err(IdentityError::Unauthorized) => {
            security::signed_out(&state, "/login?session_ended=true")
        }
        Err(error) => identity_error(&state, error),
    }
}
