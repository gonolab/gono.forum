use super::*;
use crate::{
    identity::IdentityError,
    mail::{MailInput, MailSettings},
};
use axum::{extract::DefaultBodyLimit, routing::post};

use super::extract::{CsrfForm, Page, csrf_forms};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/mail", get(page).post(save))
        .route("/manage/mail/switch", post(switch))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

#[derive(Template)]
#[template(path = "manage/mail.html")]
struct MailTemplate<'a> {
    p: PageMeta<'a>,
    /// Email sending switch (`settings.email.enabled`), taken from the page frame's copy of the site settings.
    enabled: bool,
    settings: MailSettings,
    message: String,
    failed: bool,
}

/// Permissions and conflicts get explicit messages; everything else is an internal error: the original text (redacted)
/// goes to the log and the page shows only generic text.
fn failure(state: &AppState, error: anyhow::Error) -> Response {
    if let Some(identity) = error.downcast_ref::<IdentityError>() {
        let (status, message) = match identity {
            IdentityError::Forbidden => (
                StatusCode::FORBIDDEN,
                crate::i18n::current().mail_settings_error_forbidden(),
            ),
            IdentityError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                crate::i18n::current().mail_settings_error_session(),
            ),
            IdentityError::Conflict(message) => (StatusCode::CONFLICT, *message),
            _ => {
                tracing::error!(error = %crate::debug::visible_error(&error), "mail settings failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    crate::i18n::current().mail_settings_error_unavailable(),
                )
            }
        };
        return error_page(state, status, message);
    }
    tracing::error!(error = %crate::debug::visible_error(&error), "mail settings failed");
    error_page(
        state,
        StatusCode::INTERNAL_SERVER_ERROR,
        crate::i18n::current().mail_settings_error_retry(),
    )
}

fn view(state: &AppState, settings: MailSettings, message: String, status: StatusCode) -> Response {
    let mut response = render(&MailTemplate {
        p: meta(
            state,
            crate::i18n::current().mail_settings_title().into(),
            crate::i18n::current().mail_settings_description().into(),
            "/manage/mail",
            "",
            "",
        ),
        enabled: state.frame.settings.email.enabled,
        settings,
        message,
        failed: !status.is_success(),
    });
    *response.status_mut() = status;
    response
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    saved: String,
}

async fn page(Page(state): Page, Query(query): Query<PageQuery>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/mail");
    };
    let notice = match query.saved.as_str() {
        "on" => crate::i18n::current().mail_settings_switched_on(),
        "off" => crate::i18n::current().mail_settings_switched_off(),
        _ => "",
    };
    match state.mail.settings(session.member.actor()).await {
        Ok(settings) => view(&state, settings, notice.into(), StatusCode::OK),
        Err(error) => failure(&state, error),
    }
}

#[derive(Deserialize)]
struct SwitchForm {
    _csrf: String,
    enabled: String,
}
csrf_forms!(SwitchForm);

/// Email sending switch (`settings.email.enabled`), which only stops sending: when off,
/// delivery pauses and events are still queued as usual; rules such as email verification are unchanged. Saved through
/// the global settings save (exclusive lock, audit, clearing the process cache and broadcasting).
async fn switch(Page(state): Page, CsrfForm(form): CsrfForm<SwitchForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/mail");
    };
    let on = form.enabled == "on";
    match crate::runtime::update(
        &state.db,
        &state.global,
        session.member.actor(),
        "mail.switch",
        |settings| settings.email.enabled = on,
    )
    .await
    {
        Ok(_) => auth::redirect(if on {
            "/manage/mail?saved=on"
        } else {
            "/manage/mail?saved=off"
        }),
        Err(error) => auth::identity_error(&state, error),
    }
}

#[derive(Deserialize)]
struct MailForm {
    _csrf: String,
    revision: i64,
    transport: String,
    host: String,
    port: u16,
    security: String,
    username: String,
    from_address: String,
    from_name: String,
    incoming_host: String,
    incoming_port: u16,
    test_to: String,
    #[serde(default)]
    password: String,
    operation: String,
}
csrf_forms!(MailForm);

async fn save(Page(state): Page, CsrfForm(form): CsrfForm<MailForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return error_page(
            &state,
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().mail_settings_error_sign_in(),
        );
    };
    let actor = session.member.actor();
    let current = match state.mail.settings(actor).await {
        Ok(s) => s,
        Err(error) => return failure(&state, error),
    };
    let submitted = MailSettings {
        revision: form.revision,
        transport: form.transport,
        host: form.host,
        port: form.port,
        security: form.security,
        username: form.username,
        from_address: form.from_address,
        from_name: form.from_name,
        incoming_host: form.incoming_host,
        incoming_port: form.incoming_port,
        test_to: form.test_to,
        has_password: current.has_password,
    };
    let input = MailInput {
        settings: submitted.clone(),
        password: form.password,
    };
    let result = match form.operation.as_str() {
        "save" => state
            .mail
            .save_settings(actor, input)
            .await
            .map(|settings| (settings, crate::i18n::current().mail_settings_saved())),
        "test" => state.mail.test_settings(actor, input).await.map(|()| {
            (
                submitted.clone(),
                crate::i18n::current().mail_settings_tested(),
            )
        }),
        _ => {
            return view(
                &state,
                submitted,
                crate::i18n::current()
                    .mail_settings_error_operation()
                    .into(),
                StatusCode::BAD_REQUEST,
            );
        }
    };
    match result {
        Ok((settings, message)) => view(&state, settings, message.into(), StatusCode::OK),
        Err(error) if error.is::<IdentityError>() || error.is::<sqlx::Error>() => {
            failure(&state, error)
        }
        // What remains are messages written by `mail/settings.rs` and `mail/transport.rs` themselves (format checks,
        // authorization code, SMTP test results; the SMTP server's original text has already been removed there), so
        // they can stay in the form.
        Err(error) => view(
            &state,
            submitted,
            error.to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    }
}
