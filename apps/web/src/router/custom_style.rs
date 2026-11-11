//! `/my/style`: editing, saving and resetting the personal style; `/my/style.css`: the member's own stylesheet.
//! Rules and validation are in [`crate::custom_css`].
use super::extract::{CsrfForm, Page};
use super::*;
use crate::custom_css;
use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use std::collections::HashMap;

const PATH: &str = "/my/style";
pub(super) const STYLESHEET: &str = "/my/style.css";
/// The address carries the version, so the address changes when the content does and browsers can cache it long-term.
/// The context middleware recognizes this string and does not rewrite it to no-store.
pub(super) const STYLESHEET_CACHE: &str = "private, max-age=31536000, immutable";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PATH, get(page).post(save))
        .route("/my/style/reset", post(reset))
        // 16 KiB of style can approach three times that once form-encoded.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .route(STYLESHEET, get(stylesheet))
}

/// Whether the personal style applies on this page: not applied on the editing page, the admin area, account
/// security, login and two-factor, or sensitive pages. Error pages drop it separately in `error_page`.
pub(super) fn applies(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    !(path == PATH
        || path.starts_with("/my/style/")
        || path == "/manage"
        || path.starts_with("/manage/")
        || path == "/my/account"
        || path.starts_with("/my/account/")
        || path == "/login"
        || path.starts_with("/login/")
        || matches!(
            path,
            "/register"
                | "/logout"
                | "/forgot-password"
                | "/reset-password"
                | "/verify-email"
                | "/confirm-email-change"
        )
        || crate::urls::sensitive_page(path))
}

/// The `<link>` address on the member's own pages; empty when no style was ever saved or this page does not apply it.
pub(super) fn href(state: &AppState, path: &str) -> String {
    let Some(session) = &state.viewer.session else {
        return String::new();
    };
    let version = state.frame.reading.css_version;
    if version <= 0 || !applies(path) {
        return String::new();
    }
    format!("{STYLESHEET}?u={}&v={version}", session.member.uid)
}

#[derive(Template)]
#[template(path = "settings/style.html")]
struct StyleTemplate<'a> {
    p: PageMeta<'a>,
    frame: super::my::Frame,
    css: String,
    version: i64,
    has_css: bool,
    message: String,
    success: bool,
    max_kib: usize,
}

fn login() -> Response {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_to", PATH)
        .finish();
    auth::redirect(&format!("/login?{query}"))
}

/// `draft` is the rejected submission, filled back as-is so the member does not have to rewrite it.
async fn respond(
    state: &AppState,
    draft: Option<(String, i64)>,
    message: String,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    let stored = match custom_css::load(&state.db, session.member.uid).await {
        Ok(stored) => stored,
        Err(error) => return database_error(state, error),
    };
    let has_css = stored.css.is_some();
    let (css, version) = draft.unwrap_or((stored.css.unwrap_or_default(), stored.version));
    let mut p = meta(
        state,
        crate::i18n::current().settings_style_title().into(),
        crate::i18n::current().settings_style_description().into(),
        PATH,
        "",
        "",
    );
    if let Some(error) = super::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&StyleTemplate {
        frame: super::my::frame("style"),
        p,
        css,
        version,
        has_css,
        message,
        success: status.is_success(),
        max_kib: custom_css::MAX_BYTES / 1024,
    });
    *response.status_mut() = status;
    response
}

#[derive(Default, Deserialize)]
struct PageQuery {
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    reset: bool,
}

async fn page(Page(state): Page, Query(query): Query<PageQuery>) -> Response {
    let message = if query.saved {
        crate::i18n::current().settings_style_saved()
    } else if query.reset {
        crate::i18n::current().settings_style_cleared()
    } else {
        ""
    };
    respond(&state, None, message.into(), StatusCode::OK).await
}

/// `None` lets it through; over the limit gives a 429 page, and Redis enabled but unavailable gives 503.
async fn limited(state: &AppState, uid: i64) -> Option<Response> {
    match state
        .limit(&[crate::rate_limit::Rule::new("custom-css", uid, 30, 300)])
        .await
    {
        Ok(true) => None,
        Ok(false) => Some(error_page(
            state,
            StatusCode::TOO_MANY_REQUESTS,
            crate::i18n::current().settings_error_save_too_many(),
        )),
        Err(unavailable) => Some(unavailable),
    }
}

fn version(form: &HashMap<String, String>) -> Option<i64> {
    form.get("version")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
}

async fn save(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    if let Some(response) = limited(&state, session.member.uid).await {
        return response;
    }
    let css = form.get("css").cloned().unwrap_or_default();
    let Some(version) = version(&form) else {
        return respond(
            &state,
            Some((css, 0)),
            crate::i18n::current().settings_error_style_version().into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    // Browsers submit line breaks as CRLF; normalize to LF before computing size and line numbers.
    let css = css.replace("\r\n", "\n");
    if let Err(rejection) = custom_css::validate(&css) {
        return respond(
            &state,
            Some((css, version)),
            rejection.to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    match custom_css::save(
        &state.db,
        &state.cache,
        session.member.actor(),
        Some(&css),
        version,
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("{PATH}?saved=true")),
        Err(error) => {
            let (status, message) = posting::write_error(error);
            respond(&state, Some((css, version)), message, status).await
        }
    }
}

async fn reset(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    if let Some(response) = limited(&state, session.member.uid).await {
        return response;
    }
    let Some(version) = version(&form) else {
        return respond(
            &state,
            None,
            crate::i18n::current().settings_error_style_version().into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    match custom_css::save(
        &state.db,
        &state.cache,
        session.member.actor(),
        None,
        version,
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("{PATH}?reset=true")),
        Err(error) => {
            let (status, message) = posting::write_error(error);
            respond(&state, None, message, status).await
        }
    }
}

#[derive(Default, Deserialize)]
struct StylesheetQuery {
    #[serde(default)]
    u: i64,
    #[serde(default)]
    v: i64,
}

/// Only for the session's own member: the `u` in the address must be the uid in the session (so switching accounts in
/// the same browser never hits another account's cached copy), otherwise 404. Long-term caching is only allowed when
/// the version matches; an old version's address gets the current style, but uncached.
async fn stylesheet(
    State(state): State<Arc<AppState>>,
    viewer: Option<axum::Extension<auth::Viewer>>,
    Query(query): Query<StylesheetQuery>,
) -> Response {
    let Some(session) = viewer.and_then(|axum::Extension(viewer)| viewer.session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if query.u != session.member.uid {
        return StatusCode::NOT_FOUND.into_response();
    }
    let stored = match custom_css::load(&state.db, session.member.uid).await {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!(%error, "failed to read the personal style");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let cache = if query.v == stored.version && stored.version > 0 {
        STYLESHEET_CACHE
    } else {
        "private, no-store"
    };
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, cache),
        ],
        stored.css.unwrap_or_default(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_skips_editor_admin_security_and_sensitive_pages() {
        for path in [
            "/",
            "/thread-1",
            "/daily",
            "/space/3",
            "/my",
            "/my/profile",
            "/my/reading",
            "/my/notifications",
            "/search?q=a",
        ] {
            assert!(applies(path), "{path}");
        }
        for path in [
            "/my/style",
            "/manage",
            "/manage/users",
            "/my/account",
            "/my/account/two-factor",
            "/login",
            "/login/verify",
            "/register",
            "/reset-password",
            "/confirm-email-change",
            "/my/data",
            "/my/api",
            "/my/feed",
            "/appeals",
        ] {
            assert!(!applies(path), "{path}");
        }
    }
}
