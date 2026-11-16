//! Plain HTML account forms and the primary-database identity context shared by every page.
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use askama::Template;
use axum::{
    Router,
    extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::{
    AppState, PageMeta, database_error, error_page,
    extract::{CheckedForm, CsrfForm, Page, csrf_forms},
    meta, render,
};
use crate::identity::{self, AuthSession, IdentityError, Invite, SessionGrant};
use crate::rate_limit::Rule;

mod api;
mod external;
mod profile;
pub(super) use external::callback_url;
mod registration_source;
mod security;
mod sms;
mod two_factor;

/// Content security policy. `frame-src` only opens the YouTube no-cookie domain, for the click-to-load player in
/// post bodies; no other external source is allowed, so no external images, scripts or styles are loaded. The two
/// versions differ only in the style nonce: the middleware gives every page the one with a nonce, and
/// `FALLBACK_CSP` is the router-level fallback.
pub(super) const FALLBACK_CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-src 'self' https://www.youtube-nocookie.com; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'";

pub(super) fn page_csp(nonce: &str) -> String {
    FALLBACK_CSP.replacen(
        "style-src 'self'",
        &format!("style-src 'self' 'nonce-{nonce}'"),
        1,
    )
}

#[derive(Clone, Default)]
pub(super) struct Viewer {
    pub session: Option<AuthSession>,
    pub csrf: String,
    pub csp_nonce: String,
    pub published: String,
    pub return_to: String,
    pub unread_notifications: i64,
    pub sensitive: bool,
    raw_sid: String,
    pub(super) client_ip: String,
    /// Whether this connection uses HTTPS; decides the cookie Secure flag and the `__Host-` prefix.
    /// Unrelated to the site URL: the URL is an editable site identity, the protocol is a deployment fact.
    pub(super) secure: bool,
    /// The origin derived from this request (`http(s)://Host`); the page frame uses it as the site URL when the
    /// forum URL is empty. `None` when no usable `Host` was sent. See [`request_origin`].
    pub(super) origin: Option<String>,
    /// The sealed skin preview cookie of a signed-in member (docs/skins.md, "Preview"); the page frame opens it and
    /// applies it only to the administrator who started it. Empty without one.
    pub(super) skin_preview: String,
}

#[derive(Clone)]
struct IdentityChanged;

pub(super) fn routes(upload_limit: usize) -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", get(login_page).post(login))
        .route("/register", get(register_page).post(register))
        .route("/logout", post(logout))
        .route("/verify-email", get(verify_page).post(verify))
        .route("/verify-email/resend", post(resend))
        .route("/my/invites", get(invites_page).post(create_invite))
        .route("/my/invites/{id}/revoke", post(revoke_invite))
        .merge(security::routes())
        .merge(two_factor::routes())
        .merge(profile::routes(upload_limit))
        .merge(api::routes())
        .merge(sms::routes())
        .merge(external::routes())
        .layer(DefaultBodyLimit::max(16 * 1024))
}

/// The `__Host-` prefix requires the cookie to carry Secure, so the name and the flag must come from the same
/// decision; otherwise the browser silently drops a `__Host-` cookie without Secure.
fn cookie_name(secure: bool, session: bool) -> &'static str {
    match (secure, session) {
        (true, true) => "__Host-gonof_session",
        (true, false) => "__Host-gonof_csrf",
        (false, true) => "gonof_session",
        (false, false) => "gonof_csrf",
    }
}

/// Reveal cookie for token plaintext shown only once (feed URLs, API tokens), see [`identity::SecretReveal`].
/// It is only sent to the reveal page, so it cannot use the `__Host-` prefix (which requires `Path=/`); under HTTPS
/// it uses `__Secure-`.
pub(super) struct RevealCookie {
    plain: &'static str,
    secure: &'static str,
    path: &'static str,
    pub(super) kind: identity::SecretReveal,
}

pub(super) const FEED_REVEAL: RevealCookie = RevealCookie {
    plain: "gonof_feed_reveal",
    secure: "__Secure-gonof_feed_reveal",
    path: "/my/feed",
    kind: identity::SecretReveal::FeedToken,
};

pub(super) const API_REVEAL: RevealCookie = RevealCookie {
    plain: "gonof_api_reveal",
    secure: "__Secure-gonof_api_reveal",
    path: "/my/api",
    kind: identity::SecretReveal::ApiKey,
};

impl RevealCookie {
    fn name(&self, secure: bool) -> &'static str {
        if secure { self.secure } else { self.plain }
    }

    /// Writes the cookie (clears it when `max_age` is 0).
    pub(super) fn set(&self, response: &mut Response, secure: bool, value: &str, max_age: i64) {
        let flag = if secure { "; Secure" } else { "" };
        if let Ok(header) = HeaderValue::from_str(&format!(
            "{}={value}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
            self.name(secure),
            // A cookie sent only to the reveal page carries this request's path prefix.
            crate::site::href(self.path),
            max_age.max(0)
        )) {
            response.headers_mut().append(header::SET_COOKIE, header);
        }
    }

    /// Generation succeeded: the plaintext goes into the reveal cookie and a 303 goes back to the reveal page. A
    /// sealing failure (only possible when the cookie key configuration is broken) is handled as a server error.
    pub(super) fn redirect(
        &self,
        state: &AppState,
        uid: i64,
        secret: &str,
        location: &str,
    ) -> Response {
        match state.identity.seal_secret_reveal(self.kind, uid, secret) {
            Ok(sealed) => {
                let mut response = redirect(location);
                self.set(
                    &mut response,
                    state.viewer.secure,
                    &sealed,
                    identity::SECRET_REVEAL_TTL,
                );
                response
            }
            Err(error) => {
                tracing::error!(%error, "failed to seal the one-time reveal token");
                error_page(
                    state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    crate::i18n::current().account_error_unavailable(),
                )
            }
        }
    }

    /// Reads the member's own unexpired plaintext; `None` when absent, forged or belonging to someone else.
    pub(super) fn read(&self, state: &AppState, headers: &HeaderMap, uid: i64) -> Option<String> {
        unique_cookie(headers, self.name(state.viewer.secure), 2048)
            .and_then(|raw| state.identity.revealed_secret(self.kind, raw, uid))
    }
}

fn unique_cookie<'a>(headers: &'a HeaderMap, name: &str, max_length: usize) -> Option<&'a str> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        // Do not skip unparsable Cookie headers, so a second value with the same name is not dropped and the
        // remaining one mistaken for unique.
        for part in header.to_str().ok()?.split(';') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            if key != name {
                continue;
            }
            if found.is_some() || value.is_empty() || value.len() > max_length {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

pub(super) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = unique_cookie(headers, name, 64)?;
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_owned())
}

fn session_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    // Identity verifies the AEAD version and ciphertext; the HTTP layer first limits the size and rejects
    // ambiguous duplicate names.
    unique_cookie(headers, name, 2048).map(str::to_owned)
}

/// `secure` is given by the caller, not read from `state`: the middleware holds the router's shared `AppState`,
/// whose `viewer` is not attached yet, so reading it there would always give false.
fn set_cookie(response: &mut Response, secure: bool, session: bool, value: &str, max_age: i64) {
    let flag = if secure { "; Secure" } else { "" };
    let value = format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
        cookie_name(secure, session),
        max_age.max(0)
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).expect("only internally generated cookies are written"),
    );
}

fn client_ip(request: &Request, listen: SocketAddr) -> String {
    client_ip_parts(request.headers(), request.extensions(), listen)
}

/// Same as [`client_ip`], for extractors that only hold the request headers and extensions.
pub(super) fn client_ip_parts(
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
    listen: SocketAddr,
) -> String {
    let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<SocketAddr>>() else {
        return "local".into();
    };
    // The local nginx overwrites X-Real-IP. Trust it only across a loopback-only
    // listener and loopback peer; public clients cannot choose their rate-limit key.
    if peer.ip().is_loopback() {
        if listen.ip().is_loopback() {
            match real_ip(headers) {
                Ok(ip) => return ip.to_string(),
                Err(problem) => warn_shared_client_ip(problem),
            }
        } else if headers.contains_key("x-real-ip") {
            warn_shared_client_ip("the listen address is not loopback, X-Real-IP is not trusted");
        }
    }
    peer.ip().to_string()
}

/// The `X-Real-IP` written by the reverse proxy: there must be exactly one, and it must be an IP without a port.
fn real_ip(headers: &HeaderMap) -> Result<IpAddr, &'static str> {
    let mut values = headers.get_all("x-real-ip").iter();
    let value = values
        .next()
        .ok_or(crate::i18n::current().account_error_no_real_ip())?;
    if values.next().is_some() {
        return Err(crate::i18n::current().account_error_real_ip_repeated());
    }
    value
        .to_str()
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(crate::i18n::current().account_error_real_ip_invalid())
}

/// Minimum interval in seconds between two "client address unavailable" warnings in the same process.
const SHARED_CLIENT_IP_WARNING_SECONDS: i64 = 3600;
static LAST_SHARED_CLIENT_IP_WARNING: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(i64::MIN);

/// Warns once when a request forwarded by a loopback peer has no usable client address.
///
/// In that case only the peer address can be counted, so everyone coming through the reverse proxy counts as
/// 127.0.0.1: per-IP rate limits (login, registration, password recovery, API authentication failures, etc.) share
/// one bucket site-wide, and once one person uses up the quota everyone gets 429. This is a deployment problem that
/// code cannot fix, so it should be reported as early as possible; logging every occurrence would flood the log, so
/// each process logs once on first occurrence and then at most once per hour. Accessing a development instance
/// directly on this machine without a reverse proxy also triggers it, which is expected.
fn warn_shared_client_ip(problem: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    let now = identity::now();
    let last = LAST_SHARED_CLIENT_IP_WARNING.load(Relaxed);
    if !shared_client_ip_warning_due(last, now) {
        return;
    }
    if LAST_SHARED_CLIENT_IP_WARNING
        .compare_exchange(last, now, Relaxed, Relaxed)
        .is_ok()
    {
        tracing::warn!(
            problem,
            "requests from the reverse proxy carry no client address, so per-IP rate limits share \
             one loopback bucket site-wide; make the application listen only on loopback and set \
             proxy_set_header X-Real-IP $remote_addr in the nginx reverse proxy"
        );
    }
}

fn shared_client_ip_warning_due(last: i64, now: i64) -> bool {
    now.saturating_sub(last) >= SHARED_CLIENT_IP_WARNING_SECONDS
}

/// Whether the client's leg of the connection is HTTPS.
///
/// **The application never terminates TLS itself**; connections to it are always plain HTTP, so the only evidence of
/// HTTPS is the `X-Forwarded-Proto` written by a trusted proxy. The trust rule matches [`client_ip_parts`]: the header
/// is honored only when both the listener and the peer are loopback, otherwise a public client could declare "I am
/// HTTPS" itself.
///
/// Without evidence this is false, not a "safer" true. True would make a plain-HTTP deployment send Secure cookies
/// that the browser never sends back, so login fails outright; and since the `__Host-` prefix requires Secure, the
/// name and the flag flip together, so even existing sessions cannot be read.
///
/// Hence a deployment requirement: **the reverse proxy must forward `X-Forwarded-Proto`** (the BaoTa panel's default
/// configuration does). Not forwarding it tells the application the site is plain HTTP, and the session cookie loses
/// Secure.
pub(super) fn secure_transport(
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
    listen: SocketAddr,
) -> bool {
    let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<SocketAddr>>() else {
        return false;
    };
    if !(listen.ip().is_loopback() && peer.ip().is_loopback()) {
        return false;
    }
    let mut values = headers.get_all("x-forwarded-proto").iter();
    match (values.next(), values.next()) {
        // Several headers with the same name mean more than one hop is writing it and it is unclear which one is
        // trusted, so treat it as plain HTTP.
        (Some(value), None) => value.as_bytes().eq_ignore_ascii_case(b"https"),
        _ => false,
    }
}

/// The site origin derived from this request: the protocol from [`secure_transport`] (the `X-Forwarded-Proto`
/// forwarded by the reverse proxy), host and port from the `Host` header (`:authority` when HTTP/2 has no `Host`),
/// then normalized by the same rules as the forum URL.
///
/// When the forum URL is empty, canonical, share metadata, the sitemap and robots all use it, so the reverse proxy
/// must pass the original `Host` through. `Host` comes from the request itself; normalization rejects forms with a
/// path, credentials or odd characters, so the result can only be "the host name the client claims", which is
/// exactly what leaving it empty means; to pin the URL, fill in the forum URL in the admin area.
pub(super) fn request_origin(
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    extensions: &axum::http::Extensions,
    listen: SocketAddr,
) -> Option<String> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            uri.authority()
                .map(|authority| authority.as_str().to_owned())
        })?;
    let scheme = if secure_transport(headers, extensions, listen) {
        "https"
    } else {
        "http"
    };
    crate::config::normalize_site_url(&format!("{scheme}://{}", host.trim())).ok()
}

/// Request origin for entry points that bypass the page frame (robots, sitemap, feeds), see [`request_origin`].
pub(super) struct RequestOrigin(pub Option<String>);

impl axum::extract::FromRequestParts<Arc<AppState>> for RequestOrigin {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(request_origin(
            &parts.headers,
            &parts.uri,
            &parts.extensions,
            state.config.listen,
        )))
    }
}

pub(super) async fn context(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    // Skin stylesheets are the same for everyone and carry their own cache headers (router/skins.rs).
    if path.starts_with("/skin/") {
        return next.run(request).await;
    }
    // Images whose visibility depends on the viewer: forum icons (member-only forums), post attachments and list
    // thumbnails.
    let gated_image = path.starts_with("/forum-icon/")
        || path.starts_with("/media/")
        || path.starts_with("/thumb/");
    let custom_stylesheet = path == super::custom_style::STYLESHEET;
    let token_page = crate::urls::token_page(path);
    // The JSON API only accepts Bearer tokens: no cookie parsing, no CSRF cookie issued, no write to the online
    // table.
    let json_api = path
        .strip_prefix(super::api::PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
    // Brand images are public resources: looking up the session, writing the online table and issuing a CSRF
    // cookie would all be wasted, and the `private, no-store` rewritten at the end would make the browser download
    // them every time. Same for avatars: whether one can be shown depends only on the owner's current binding and
    // account status, not on the viewer, and it adds its own image cache headers with an ETag.
    // Feeds likewise only have the anonymous view and add their own `public, no-cache` with an ETag.
    if path.starts_with("/static/")
        || path.starts_with("/brand/")
        || path.starts_with("/avatar/")
        || json_api
        || crate::urls::feed_path(path)
        || crate::urls::private_feed_path(path)
        // The sitemap, like robots, only has the anonymous view: no session lookup, no cookies.
        || matches!(path, "/static" | "/healthz" | "/robots.txt" | "/sitemap.xml")
    {
        return next.run(request).await;
    }
    // The protocol must be decided before cookies are read: the cookie name itself depends on it, and reading and
    // writing must use the same decision.
    let secure = secure_transport(request.headers(), request.extensions(), state.config.listen);
    let raw_sid = session_cookie(request.headers(), cookie_name(secure, true)).unwrap_or_default();
    let session = if raw_sid.is_empty() {
        None
    } else {
        match state.identity.session(&raw_sid).await {
            Ok(session) => session,
            Err(error) => return token_page_response(identity_error(&state, error), token_page),
        }
    };
    let unread_notifications = session
        .as_ref()
        .map_or(0, |session| session.member.unread_notifications);
    let anonymous_csrf = cookie(request.headers(), cookie_name(secure, false));
    let csrf = session.as_ref().map_or_else(
        || {
            anonymous_csrf
                .clone()
                .unwrap_or_else(identity::random_token)
        },
        |session| session.csrf_token.clone(),
    );
    if let Some(session) = &session {
        state
            .identity
            .touch_online(session, &client_ip(&request, state.config.listen))
            .await;
    }
    let csp_nonce = request
        .headers()
        .get("x-csp-nonce")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .unwrap_or_else(identity::random_token);
    let consume_publication = request.method() == axum::http::Method::GET
        && !["/api/", "/media/", "/static/"]
            .iter()
            .any(|prefix| request.uri().path().starts_with(prefix));
    let landing_channel = if session.is_none() {
        registration_source::landing(&request)
    } else {
        None
    };
    let skin_preview = if session.is_some() {
        auxiliary_cookie(request.headers(), super::skins::PREVIEW_COOKIE).unwrap_or_default()
    } else {
        String::new()
    };
    let viewer = Viewer {
        sensitive: crate::urls::sensitive_page(path),
        csp_nonce,
        unread_notifications,
        published: if consume_publication {
            auxiliary_cookie(request.headers(), "gonof_published").unwrap_or_default()
        } else {
            String::new()
        },
        return_to: auxiliary_cookie(request.headers(), "gonof_return")
            .map(|v| safe_return(&v))
            .unwrap_or_default(),
        raw_sid: session
            .as_ref()
            .map_or_else(|| raw_sid.clone(), |session| session.session_token.clone()),
        session,
        csrf,
        client_ip: client_ip(&request, state.config.listen),
        secure,
        origin: request_origin(
            request.headers(),
            request.uri(),
            request.extensions(),
            state.config.listen,
        ),
        skin_preview,
    };
    request.extensions_mut().insert(viewer.clone());
    // The admin area always requires login first: a visitor opening any /manage URL goes to the login page and
    // returns to the original page (with query string) after logging in, the same way for every page. This only
    // covers the "not logged in" case; the moderator and administrator gates remain on each page, and POSTs still go
    // through their own CSRF and permission checks.
    let guest_manage = viewer.session.is_none()
        && matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && (request.uri().path() == "/manage" || request.uri().path().starts_with("/manage/"));
    let mut response = if guest_manage {
        let target = request
            .uri()
            .path_and_query()
            .map_or("/manage", |value| value.as_str());
        redirect(&format!(
            "/login?return_to={}",
            url::form_urlencoded::byte_serialize(target.as_bytes()).collect::<String>()
        ))
    } else {
        next.run(request).await
    };
    if let Some(channel) = landing_channel
        && (response.status().is_success() || response.status().is_redirection())
    {
        registration_source::write(&mut response, &viewer, Some(&channel));
    }
    let csp = page_csp(&viewer.csp_nonce);
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).expect("hex nonce"),
    );
    if !viewer.published.is_empty()
        && !response.status().is_redirection()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"text/html"))
        && !response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value.as_bytes().starts_with(b"gonof_published="))
    {
        auxiliary_set_cookie(&mut response, "gonof_published", "", 0);
    }
    if response.extensions().get::<IdentityChanged>().is_none() {
        if let Some(session) = &viewer.session {
            if let Some(renewed) = &session.renewed_cookie {
                set_cookie(
                    &mut response,
                    secure,
                    true,
                    renewed,
                    session.expires_at - identity::now(),
                );
            }
            if session.member.is_admin {
                response.extensions_mut().insert(crate::debug::Authorized);
            }
        } else {
            if !viewer.raw_sid.is_empty() {
                set_cookie(&mut response, secure, true, "", 0);
            }
            if anonymous_csrf.is_none() {
                set_cookie(&mut response, secure, false, &viewer.csrf, 3600);
            }
        }
    }
    // Forum icons and post attachments depend on the viewer, so they must go through the session; the image cache
    // headers they set themselves (5 minutes + ETag) are kept so the browser can keep a copy and get a 304 when
    // revalidating. Error pages and all other responses are still changed to no-store.
    let revalidated = gated_image
        && response
            .headers()
            .get(header::CACHE_CONTROL)
            .is_some_and(|value| value == crate::media::IMAGE_CACHE_CONTROL);
    // The member's own custom stylesheet: the URL carries uid and version, and long caching is allowed when the
    // version matches.
    let custom_style = custom_stylesheet
        && response
            .headers()
            .get(header::CACHE_CONTROL)
            .is_some_and(|value| value == super::custom_style::STYLESHEET_CACHE);
    let revalidated = revalidated || custom_style;
    if !revalidated {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response
        .headers_mut()
        .entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("same-origin"));
    token_page_response(response, token_page)
}

pub(super) fn token_page_response(mut response: Response, token_page: bool) -> Response {
    if token_page {
        // The identity/notification queries can fail before routing; the error page must not leak the URL token
        // to resource requests either.
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin"),
        );
    }
    response
}

/// Builds this request's page state: puts in the viewer and takes one page frame snapshot.
///
/// The clone itself only copies shared handles and the `Viewer`; the page frame (site settings, forums, brand
/// assets, navigation) is fetched all at once in `SiteFrame::load` rather than queried field by field.
pub(super) async fn page_state(state: &AppState, viewer: Viewer) -> Result<AppState, Response> {
    let mut state = state.clone();
    state.viewer = viewer;
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    state.frame = std::sync::Arc::new(
        super::frame::SiteFrame::load(&state, viewer_uid)
            .await
            .map_err(|error| identity_error(&state, error))?,
    );
    Ok(state)
}

pub(super) fn identity_error(state: &AppState, error: IdentityError) -> Response {
    let (status, message) = match error {
        IdentityError::Validation(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
        IdentityError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().account_error_login_failed(),
        ),
        IdentityError::Forbidden => (
            StatusCode::FORBIDDEN,
            crate::i18n::current().account_error_forbidden(),
        ),
        IdentityError::NotFound => (
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_error_not_found(),
        ),
        IdentityError::Conflict(message) => (StatusCode::CONFLICT, message),
        IdentityError::Suspended(message) | IdentityError::Restricted(message) => {
            return error_page(state, StatusCode::FORBIDDEN, &message);
        }
        IdentityError::Internal(error) => return database_error(state, error),
    };
    error_page(state, status, message)
}

pub(super) fn redirect(path: &str) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, crate::site::href(path))],
    )
        .into_response()
}

/// Whether the source URL and this request's Host are the same host.
///
/// **Only host name and port are compared, not the protocol.** The application runs behind a reverse proxy and
/// locally always sees plain HTTP, so comparing protocols would judge every HTTPS request cross-site; same-site
/// http→https downgrades are handled by nginx redirects and HSTS, not by the same-origin check. Default ports do not
/// appear in the Origin sent by browsers, so they are dropped here too, so that `https://a.com:443` and `a.com` are
/// not judged as two hosts.
fn same_host(source: &str, host: &str) -> bool {
    let Ok(url) = url::Url::parse(source) else {
        return false;
    };
    let Some(source_host) = url.host_str() else {
        return false;
    };
    let authority = match url.port() {
        Some(port) => format!("{source_host}:{port}"),
        None => source_host.to_owned(),
    };
    authority.eq_ignore_ascii_case(host)
}

/// The reason the check failed; `None` when it passes.
///
/// The three rejection reasons must be kept apart and told to the visitor: if only "The form has expired" were
/// returned, a reverse proxy rewriting Host would leave no clue on the page; that is why every form on the test site
/// got 403, and tracking it down meant digging through code and the nginx configuration. The Origin, Referer and Host
/// shown here are all public information carried by the request itself.
/// The reason the same-origin check failed; `None` when it passes. It does not touch `AppState`, so it can be
/// tested directly against request headers.
pub(super) fn origin_failure(headers: &HeaderMap) -> Option<String> {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
    {
        return Some(crate::i18n::current().account_error_cross_site().into());
    }
    // Compare against the request's own Host, not the site URL: the URL is an editable site identity, and a wrong
    // value would make every form unsubmittable, including the one used to change it back.
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let source = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())?;
    // A source is present but we cannot tell who we are, so same origin cannot be proven: reject.
    match host {
        Some(host) if same_host(source, host) => None,
        Some(host) => Some(crate::i18n::current().account_error_origin_mismatch(source, host)),
        None => Some(crate::i18n::current().account_error_no_host(source)),
    }
}

pub(super) fn csrf_failure(state: &AppState, headers: &HeaderMap, value: &str) -> Option<String> {
    origin_failure(headers).or_else(|| token_failure(state, value))
}

/// Compares only the token, ignoring the source. `None` when it passes.
///
/// Kept separate from [`origin_failure`] for uploads whose token is in the multipart body: the source only needs the
/// request headers and can be checked before reading the body; the token can only be checked once that field is read.
pub(super) fn token_failure(state: &AppState, value: &str) -> Option<String> {
    let matched = if let Some(session) = &state.viewer.session {
        identity::check_csrf(value, session)
    } else {
        value.len() == 64 && constant_eq(value.as_bytes(), state.viewer.csrf.as_bytes())
    };
    (!matched).then(|| {
        crate::i18n::current()
            .account_error_token_mismatch()
            .to_owned()
    })
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

pub(super) fn csrf_error(state: &AppState, reason: &str) -> Response {
    error_page(
        state,
        StatusCode::FORBIDDEN,
        &crate::i18n::current().account_error_stale_refresh(reason),
    )
}

#[derive(Default, Deserialize)]
pub(super) struct AuthForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password2: String,
    #[serde(default)]
    invite: String,
    #[serde(default)]
    return_to: String,
    /// The registration form's checkbox for agreeing to the terms of use and the privacy notice; `1` when checked.
    #[serde(default)]
    accept_terms: String,
}
csrf_forms!(AuthForm);

impl AuthForm {
    fn return_query(&self) -> String {
        if self.return_to.is_empty() {
            return String::new();
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("return_to", &safe_return(&self.return_to));
        format!("?{}", query.finish())
    }
}
#[derive(Default, Deserialize)]
struct ReturnQuery {
    #[serde(default)]
    return_to: String,
    #[serde(default)]
    password_changed: bool,
    #[serde(default)]
    password_reset: bool,
    #[serde(default)]
    session_ended: bool,
    #[serde(default)]
    logged_out_all: bool,
    #[serde(default)]
    email_changed: bool,
    #[serde(default)]
    second_factor_expired: bool,
    #[serde(default)]
    password_set: bool,
}
#[derive(Template)]
#[template(path = "auth/account.html")]
struct AccountTemplate<'a> {
    p: PageMeta<'a>,
    register: bool,
    invitation_required: bool,
    registration_closed: bool,
    /// This request's login page shows SMS login.
    sms_login: bool,
    /// Third-party logins shown on this request: enabled and with credentials configured.
    providers: Vec<crate::oauth::Provider>,
    form: &'a AuthForm,
    message: &'a str,
    success: bool,
}

async fn account_page(
    state: &AppState,
    registration: bool,
    form: &AuthForm,
    message: &str,
    status: StatusCode,
) -> Response {
    let lang = crate::i18n::current();
    let site = crate::site::site_name(&state.frame.settings);
    let (title, description) = if registration {
        (
            lang.frame_register(),
            lang.account_register_description(site),
        )
    } else {
        (lang.frame_log_in(), lang.account_login_description(site))
    };
    let path = if registration { "/register" } else { "/login" };
    let mut response = render(&AccountTemplate {
        p: meta(state, title.into(), description, path, "", ""),
        register: registration,
        invitation_required: state.frame.settings.signup.mode == "invite",
        registration_closed: state.frame.settings.signup.mode == "closed",
        sms_login: crate::login::sms_enabled(&state.frame.settings),
        providers: external::shown(state).await,
        form,
        message,
        success: status == StatusCode::OK && !message.is_empty(),
    });
    *response.status_mut() = status;
    response
}

async fn login_page(Page(state): Page, Query(query): Query<ReturnQuery>) -> Response {
    let mut form = AuthForm {
        return_to: safe_return(&query.return_to),
        ..Default::default()
    };
    if form.return_to.is_empty() {
        form.return_to = state.viewer.return_to.clone();
    }
    if state.viewer.session.is_some() {
        return redirect(if form.return_to.is_empty() {
            "/"
        } else {
            &form.return_to
        });
    }
    let message = if query.logged_out_all {
        crate::i18n::current().account_notice_logged_out_all()
    } else if query.email_changed {
        crate::i18n::current().account_notice_email_changed()
    } else if query.password_changed {
        crate::i18n::current().account_notice_password_changed()
    } else if query.password_reset {
        crate::i18n::current().account_notice_password_reset()
    } else if query.session_ended {
        crate::i18n::current().account_notice_session_ended()
    } else if query.second_factor_expired {
        crate::i18n::current().account_notice_second_factor_expired()
    } else if query.password_set {
        crate::i18n::current().account_notice_password_set()
    } else {
        ""
    };
    let mut response = account_page(&state, false, &form, message, StatusCode::OK).await;
    if !form.return_to.is_empty() {
        auxiliary_set_cookie(&mut response, "gonof_return", &form.return_to, 3600);
    }
    response
}

async fn register_page(Page(state): Page, Query(query): Query<ReturnQuery>) -> Response {
    let mut form = AuthForm {
        return_to: safe_return(&query.return_to),
        ..Default::default()
    };
    if form.return_to.is_empty() {
        form.return_to = state.viewer.return_to.clone();
    }
    if state.viewer.session.is_some() {
        return redirect(if form.return_to.is_empty() {
            "/"
        } else {
            &form.return_to
        });
    }
    let mut response = account_page(&state, true, &form, "", StatusCode::OK).await;
    if !form.return_to.is_empty() {
        auxiliary_set_cookie(&mut response, "gonof_return", &form.return_to, 3600);
    }
    response
}

async fn form_failure(
    state: &AppState,
    registration: bool,
    form: &AuthForm,
    error: IdentityError,
) -> Response {
    let (status, message) = match error {
        IdentityError::Validation(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
        IdentityError::Conflict(message) => (StatusCode::CONFLICT, message),
        IdentityError::Suspended(message) => {
            return account_page(state, registration, form, &message, StatusCode::FORBIDDEN).await;
        }
        IdentityError::Unauthorized | IdentityError::Forbidden => (
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().account_error_login_failed(),
        ),
        error => return identity_error(state, error),
    };
    account_page(state, registration, form, message, status).await
}

fn grant_response(state: &AppState, grant: SessionGrant, path: &str) -> Response {
    let mut response = redirect(path);
    response.extensions_mut().insert(IdentityChanged);
    set_cookie(
        &mut response,
        state.viewer.secure,
        true,
        &grant.session_token,
        grant.expires_at - identity::now(),
    );
    set_cookie(&mut response, state.viewer.secure, false, "", 0);
    if let Some(marker) = &grant.device_marker {
        set_device_marker(&mut response, state.viewer.secure, marker);
    }
    response
}

/// Device marker cookie: only used to detect a "new device login" and send fewer alerts; it is not a credential.
/// Logging out does not clear it.
fn device_marker_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gonof_device"
    } else {
        "gonof_device"
    }
}

fn device_marker(state: &AppState, headers: &HeaderMap) -> String {
    unique_cookie(headers, device_marker_name(state.viewer.secure), 2048)
        .unwrap_or_default()
        .to_owned()
}

fn set_device_marker(response: &mut Response, secure: bool, marker: &str) {
    let flag = if secure { "; Secure" } else { "" };
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{}={marker}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{flag}",
        device_marker_name(secure),
        identity::DEVICE_MARKER_MAX_AGE
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

async fn login(Page(state): Page, headers: HeaderMap, form: CheckedForm<AuthForm>) -> Response {
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
            return account_page(
                &state,
                false,
                &form,
                &crate::i18n::current().account_error_stale_password_reason(reason),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    // Check the IP first, then count the account: a request stopped by the IP limit does not write an
    // attacker-chosen key into the account bucket.
    // The counts are the admin settings `settings.account.login_*_per_5min`; the window is 5 minutes (a constant).
    let account = &state.frame.settings.account;
    let attempts = |value: i64| u32::try_from(value).unwrap_or(0);
    let rules = [
        Rule::new(
            "login-ip",
            &state.viewer.client_ip,
            attempts(account.login_ip_per_5min),
            300,
        ),
        Rule::new(
            "login-name",
            identity::identifier_limit_key(&form.username),
            attempts(account.login_name_per_5min),
            300,
        ),
    ];
    match state.limit(&rules).await {
        Ok(true) => {}
        Ok(false) => {
            return account_page(
                &state,
                false,
                &form,
                crate::i18n::current().account_error_login_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    let metadata = session_metadata(&state, &headers).with_device(&device_marker(&state, &headers));
    match state
        .identity
        .begin_login(&form.username, &form.password, &metadata)
        .await
    {
        Ok(identity::LoginOutcome::Session(grant)) => {
            let destination = safe_return(&form.return_to);
            grant_response(
                &state,
                *grant,
                if destination.is_empty() {
                    "/"
                } else {
                    &destination
                },
            )
        }
        // Two-factor authentication is on: even with the right password no session is issued; go to the second
        // step.
        Ok(identity::LoginOutcome::SecondFactor(pending)) => {
            let destination = safe_return(&form.return_to);
            let path = if destination.is_empty() {
                "/login/verify".to_owned()
            } else {
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                query.append_pair("return_to", &destination);
                format!("/login/verify?{}", query.finish())
            };
            let mut response = redirect(&path);
            two_factor::set_pending(
                &mut response,
                state.viewer.secure,
                &pending.token,
                identity::SECOND_FACTOR_PENDING_TTL,
            );
            response
        }
        Err(error) => form_failure(&state, false, &form, error).await,
    }
}

async fn register(Page(state): Page, headers: HeaderMap, form: CheckedForm<AuthForm>) -> Response {
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
            return account_page(
                &state,
                true,
                &form,
                &crate::i18n::current().account_error_stale_password_reason(reason),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    if state.viewer.session.is_some() {
        return redirect("/");
    }
    if form.password != form.password2 {
        return account_page(
            &state,
            true,
            &form,
            crate::i18n::current().account_error_password_mismatch(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    // Without the consent checkbox the registration transaction is not entered: no password hashing, no invite
    // code consumed, no registration attempt counted.
    if form.accept_terms != "1" {
        return account_page(
            &state,
            true,
            &form,
            crate::i18n::current().account_error_consent(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    let consent = match policy_consent(&state).await {
        Ok(consent) => consent,
        Err(error) => return identity_error(&state, error),
    };
    // The count is the admin setting `settings.signup.ip_hourly_limit`; the window is 1 hour (a constant).
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
            return account_page(
                &state,
                true,
                &form,
                crate::i18n::current().account_error_register_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    let channel = registration_source::read(&headers, &state.viewer);
    let metadata = session_metadata(&state, &headers)
        .with_device(&device_marker(&state, &headers))
        .with_registration_source(state.viewer.origin.as_deref(), channel.as_deref());
    match state
        .identity
        .register_with_consent(
            &form.username,
            &form.email,
            &form.password,
            &form.invite,
            &metadata,
            Some(consent),
        )
        .await
    {
        Ok(grant) => {
            let mut response = grant_response(&state, grant, "/verify-email");
            if channel.is_some() {
                registration_source::write(&mut response, &state.viewer, None);
            }
            auxiliary_set_cookie(
                &mut response,
                "gonof_return",
                &safe_return(&form.return_to),
                3600,
            );
            response
        }
        Err(error) => form_failure(&state, true, &form, error).await,
    }
}

/// The current versions of the two policy documents at registration, written into the registration log as the
/// consent record.
async fn policy_consent(state: &AppState) -> Result<identity::PolicyConsent, IdentityError> {
    let ttl = state.cache_ttl();
    let terms = state.global.policy(&state.db, "policy.terms", ttl).await?;
    let privacy = state
        .global
        .policy(&state.db, "policy.privacy", ttl)
        .await?;
    Ok(identity::PolicyConsent {
        terms_version: terms.updated_at,
        privacy_version: privacy.updated_at,
    })
}

fn session_metadata(state: &AppState, headers: &HeaderMap) -> identity::SessionMetadata {
    identity::SessionMetadata::new(
        headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        &state.viewer.client_ip,
    )
}

#[derive(Default, Deserialize)]
struct ActionForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    request_key: String,
    #[serde(default)]
    token: String,
}
csrf_forms!(ActionForm);

async fn logout(
    Page(state): Page,
    headers: HeaderMap,
    CsrfForm(_): CsrfForm<ActionForm>,
) -> Response {
    if let Some(raw) = cookie(&headers, "gonof_privacy")
        && let Err(error) =
            crate::privacy::exit_access(&state.db, &raw, crate::identity::now()).await
    {
        return identity_error(&state, error);
    }
    let mut response = redirect("/");
    super::privacy::cookie(&mut response, &state, "", 0);
    response.extensions_mut().insert(IdentityChanged);
    set_cookie(&mut response, state.viewer.secure, true, "", 0);
    set_cookie(&mut response, state.viewer.secure, false, "", 0);
    super::appeals::clear_cookie(&mut response, &state);
    // A skin preview belongs to the session that started it (docs/skins.md, "Preview").
    if !state.viewer.skin_preview.is_empty() {
        auxiliary_set_cookie(&mut response, super::skins::PREVIEW_COOKIE, "", 0);
    }
    response
}

#[derive(Default, Deserialize)]
struct VerifyQuery {
    #[serde(default)]
    return_to: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    sent: bool,
    #[serde(default)]
    verified: bool,
}

#[derive(Template)]
#[template(path = "auth/verify.html")]
struct VerifyTemplate<'a> {
    p: PageMeta<'a>,
    token: &'a str,
    valid: bool,
    message: &'a str,
    email: String,
}

fn verification_page(
    state: &AppState,
    token: &str,
    valid: bool,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut response = render(&VerifyTemplate {
        p: meta(
            state,
            crate::i18n::current().frame_verify_email().into(),
            crate::i18n::current()
                .account_verify_description(crate::site::site_name(&state.frame.settings)),
            "/verify-email",
            "",
            "",
        ),
        token,
        valid,
        message,
        email: state
            .viewer
            .session
            .as_ref()
            .and_then(|session| session.member.email.clone())
            .unwrap_or_default(),
    });
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        // Keep the form's Origin while omitting the verification token from Referer.
        HeaderValue::from_static("strict-origin"),
    );
    response
}

async fn verify_page(
    Page(mut state): Page,
    query: Result<Query<VerifyQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return verification_page(
            &state,
            "",
            false,
            crate::i18n::current().account_error_verify_params(),
            StatusCode::BAD_REQUEST,
        );
    };
    if query.token.len() > 64 {
        return verification_page(
            &state,
            "",
            false,
            crate::i18n::current().account_error_verify_invalid(),
            StatusCode::BAD_REQUEST,
        );
    }
    let return_to = safe_return(&query.return_to);
    if !return_to.is_empty() {
        state.viewer.return_to = return_to.clone();
    }
    if query.token.is_empty() {
        if state
            .viewer
            .session
            .as_ref()
            .is_some_and(|s| s.member.can_contribute())
            && !state.viewer.return_to.is_empty()
        {
            let mut response = redirect(&state.viewer.return_to);
            auxiliary_set_cookie(&mut response, "gonof_return", "", 0);
            return response;
        }
        let message = if query.verified {
            crate::i18n::current().account_verify_done()
        } else if query.sent {
            crate::i18n::current().account_verify_sent()
        } else if state
            .viewer
            .session
            .as_ref()
            .is_some_and(|session| session.member.is_admin && session.member.email.is_none())
        {
            crate::i18n::current().account_verify_admin()
        } else if state
            .viewer
            .session
            .as_ref()
            .is_some_and(|session| session.member.email_verified)
        {
            crate::i18n::current().account_verify_already()
        } else {
            crate::i18n::current().account_verify_open_link()
        };
        let mut response = verification_page(&state, "", false, message, StatusCode::OK);
        if !return_to.is_empty() {
            auxiliary_set_cookie(&mut response, "gonof_return", &return_to, 3600);
        }
        return response;
    }
    match state.identity.verification_status(&query.token).await {
        Ok(true) => verification_page(
            &state,
            &query.token,
            true,
            crate::i18n::current().account_verify_confirm_prompt(),
            StatusCode::OK,
        ),
        Ok(false) => verification_page(
            &state,
            "",
            false,
            crate::i18n::current().account_error_verify_resend(),
            StatusCode::BAD_REQUEST,
        ),
        Err(error) => identity_error(&state, error),
    }
}

async fn verify(Page(state): Page, CsrfForm(form): CsrfForm<ActionForm>) -> Response {
    match state.identity.verify_email(&form.token).await {
        Ok(()) => redirect("/verify-email?verified=true"),
        Err(error) => identity_error(&state, error),
    }
}

async fn resend(Page(state): Page, CsrfForm(_): CsrfForm<ActionForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login");
    };
    match state
        .limit(&[Rule::new("resend", session.member.uid, 1, 60)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return verification_page(
                &state,
                "",
                false,
                crate::i18n::current().account_error_resend_wait(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .resend_verification_for_session(&state.viewer.raw_sid)
        .await
    {
        Ok(()) => redirect("/verify-email?sent=true"),
        Err(error) => identity_error(&state, error),
    }
}

#[derive(Default, Deserialize)]
struct InvitesQuery {
    page: Option<usize>,
}

struct InviteRow {
    id: i64,
    code: String,
    status: &'static str,
    expires: String,
    consumer: String,
    revocable: bool,
}

#[derive(Template)]
#[template(path = "auth/invites.html")]
struct InvitesTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<InviteRow>,
    request_key: String,
    message: &'a str,
    daily_remaining: usize,
    active_remaining: usize,
    can_generate: bool,
    /// Explanation shown when the invite code level threshold is not reached; empty when reached or unlimited.
    level_hint: String,
    previous: String,
    next: String,
    ttl: String,
}

fn display_time(timestamp: i64) -> String {
    let text = crate::clock::datetime(timestamp, &crate::clock::current());
    if text.is_empty() {
        crate::i18n::current().account_time_unavailable().into()
    } else {
        text
    }
}

fn display_duration(seconds: i64) -> String {
    let lang = crate::i18n::current();
    if seconds % 86400 == 0 {
        lang.account_duration_days(seconds / 86400)
    } else if seconds % 3600 == 0 {
        lang.account_duration_hours(seconds / 3600)
    } else if seconds % 60 == 0 {
        lang.account_duration_minutes(seconds / 60)
    } else {
        lang.account_duration_seconds(seconds)
    }
}

fn invite_row(invite: Invite, now: i64) -> InviteRow {
    let usable = invite.status == "available" && invite.endtime > now;
    let status = match invite.status.as_str() {
        "used" => crate::i18n::current().account_invite_used(),
        "revoked" => crate::i18n::current().account_invite_revoked(),
        "frozen" => crate::i18n::current().account_invite_frozen(),
        _ if invite.endtime <= now => crate::i18n::current().account_invite_expired(),
        _ => crate::i18n::current().account_invite_available(),
    };
    InviteRow {
        id: invite.id,
        code: if usable { invite.code } else { String::new() },
        status,
        expires: display_time(invite.endtime),
        consumer: invite
            .fuid
            .map(|id| crate::i18n::current().account_invite_consumer(id.to_string()))
            .unwrap_or_default(),
        revocable: invite.fuid.is_none()
            && matches!(invite.status.as_str(), "available" | "frozen"),
    }
}

async fn invites_response(
    state: &AppState,
    page: usize,
    message: &str,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login");
    };
    if !session.member.can_contribute() {
        return error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::login::Participation::current()
                .message(crate::i18n::current().account_error_invites_need_email()),
        );
    }
    let invites = match state.identity.invites(session.member.uid, page).await {
        Ok(invites) => invites,
        Err(IdentityError::NotFound) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().common_error_no_such_page(),
            );
        }
        Err(error) => return identity_error(state, error),
    };
    let now = invites.timestamp;
    let invite = &state.frame.settings.invite;
    let daily_remaining = invite
        .daily_limit
        .saturating_sub(invites.today_generated)
        .max(0) as usize;
    let active_remaining = invite.active_limit.saturating_sub(invites.active).max(0) as usize;
    let growth_rules = &state.frame.settings.growth;
    let level_hint = if growth_rules.invite_level > 1 && !session.member.is_staff() {
        let growth = match crate::growth::growth(&state.db, session.member.uid).await {
            Ok(growth) => growth,
            Err(error) => return identity_error(state, error.into()),
        };
        let level = growth_rules.level(growth);
        if (level.number as i64) < growth_rules.invite_level {
            crate::i18n::current().account_invites_level(
                growth_rules.level_label(growth_rules.invite_level),
                level.number.to_string(),
                level.name,
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let next = if invites.has_next {
        format!("/my/invites?page={}", page + 1)
    } else {
        String::new()
    };
    let previous = if page > 1 {
        format!("/my/invites?page={}", page - 1)
    } else {
        String::new()
    };
    let mut p = meta(
        state,
        crate::i18n::current().account_invites_title().into(),
        crate::i18n::current()
            .account_invites_description(crate::site::site_name(&state.frame.settings)),
        "/my/invites",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&InvitesTemplate {
        p,
        frame: crate::router::my::frame("invites"),
        rows: invites
            .rows
            .into_iter()
            .map(|invite| invite_row(invite, now))
            .collect(),
        request_key: identity::random_token(),
        message,
        daily_remaining,
        active_remaining,
        can_generate: daily_remaining > 0 && active_remaining > 0 && level_hint.is_empty(),
        level_hint,
        previous,
        next,
        ttl: display_duration(invite.ttl_days.saturating_mul(86_400)),
    });
    *response.status_mut() = status;
    response
}

async fn invites_page(
    Page(state): Page,
    query: Result<Query<InvitesQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().account_error_invites_page(),
        );
    };
    invites_response(&state, query.page.unwrap_or(1), "", StatusCode::OK).await
}

async fn create_invite(Page(state): Page, CsrfForm(form): CsrfForm<ActionForm>) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login");
    }
    match state
        .identity
        .generate_invite_for_session(&state.viewer.raw_sid, &form.request_key)
        .await
    {
        Ok(_) => redirect("/my/invites"),
        Err(IdentityError::Conflict(message) | IdentityError::Validation(message)) => {
            invites_response(&state, 1, message, StatusCode::CONFLICT).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn revoke_invite(
    Page(state): Page,
    Path(id): Path<i64>,
    CsrfForm(_): CsrfForm<ActionForm>,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login");
    }
    match state
        .identity
        .revoke_invite_for_session(&state.viewer.raw_sid, id)
        .await
    {
        Ok(()) => redirect("/my/invites"),
        Err(error) => identity_error(&state, error),
    }
}

// Only local absolute paths can become authentication return locations.
/// On-site return URL: rejects external sites, protocol-relative URLs, backslashes, control characters and encoded
/// slashes.
///
/// The result is written verbatim into the Location header, whose value may only be visible ASCII; non-ASCII
/// characters are percent-encoded byte by byte as UTF-8 (which is also what browsers do when requesting such URLs),
/// otherwise building the response after a successful login would fail and turn into a 500.
pub(super) fn safe_return(value: &str) -> String {
    if value.starts_with('/')
        && !value.starts_with("//")
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value.len() <= 1000
        && !value.to_ascii_lowercase().contains("%2f")
        && !value.to_ascii_lowercase().contains("%5c")
    {
        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if byte.is_ascii() {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        encoded
    } else {
        String::new()
    }
}
fn auxiliary_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| {
            url::form_urlencoded::parse(format!("v={value}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        })
}
pub(super) fn auxiliary_set_cookie(response: &mut Response, name: &str, value: &str, seconds: i64) {
    let value: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    if let Ok(header) = HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}"
    )) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

#[cfg(test)]
mod client_ip_tests {
    use super::{
        ConnectInfo, HeaderValue, Request, SocketAddr, client_ip, real_ip,
        shared_client_ip_warning_due,
    };
    use crate::rate_limit::{Local, Rule};
    use axum::body::Body;

    fn request(peer: Option<&str>, values: &[&str]) -> Request {
        let mut request = Request::new(Body::empty());
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        for value in values {
            request
                .headers_mut()
                .append("x-real-ip", HeaderValue::from_str(value).unwrap());
        }
        request
            .headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("192.0.2.222"));
        request
    }

    #[test]
    fn client_ip_loopback_proxy_accepts_single_ipv4_and_ipv6() {
        for (listen, peer, value, expected) in [
            (
                "127.0.0.1:58833",
                "127.0.0.1:43210",
                "192.0.2.10",
                "192.0.2.10",
            ),
            (
                "[::1]:58833",
                "[::1]:43210",
                "2001:db8:0:0::10",
                "2001:db8::10",
            ),
        ] {
            assert_eq!(
                client_ip(&request(Some(peer), &[value]), listen.parse().unwrap()),
                expected
            );
        }
    }

    #[test]
    fn client_ip_public_peer_cannot_spoof_proxy_headers() {
        for peer in ["198.51.100.20:43210", "[2001:db8::20]:43210"] {
            let socket = peer.parse::<SocketAddr>().unwrap();
            assert_eq!(
                client_ip(
                    &request(Some(peer), &["192.0.2.10"]),
                    "127.0.0.1:58833".parse().unwrap(),
                ),
                socket.ip().to_string()
            );
        }
    }

    #[test]
    fn client_ip_public_listener_does_not_trust_loopback_headers() {
        for listen in ["0.0.0.0:58833", "[::]:58833", "192.0.2.1:58833"] {
            assert_eq!(
                client_ip(
                    &request(Some("127.0.0.1:43210"), &["192.0.2.10"]),
                    listen.parse().unwrap(),
                ),
                "127.0.0.1"
            );
        }
    }

    #[test]
    fn client_ip_rejects_missing_invalid_or_ambiguous_proxy_headers() {
        for values in [
            vec![],
            vec![""],
            vec!["not-an-ip"],
            vec!["192.0.2.10:43210"],
            vec!["[2001:db8::10]"],
            vec!["192.0.2.10, 192.0.2.11"],
            vec!["192.0.2.10", "192.0.2.11"],
            vec!["192.0.2.10", "192.0.2.10"],
        ] {
            assert_eq!(
                client_ip(
                    &request(Some("127.0.0.1:43210"), &values),
                    "127.0.0.1:58833".parse().unwrap(),
                ),
                "127.0.0.1"
            );
        }
        let mut non_ascii = request(Some("127.0.0.1:43210"), &[]);
        non_ascii
            .headers_mut()
            .insert("x-real-ip", HeaderValue::from_bytes(&[0x80]).unwrap());
        assert_eq!(
            client_ip(&non_ascii, "127.0.0.1:58833".parse().unwrap()),
            "127.0.0.1"
        );
    }

    #[test]
    fn unusable_proxy_headers_are_named_for_the_deployment_warning() {
        let headers = |values: &[&str]| request(Some("127.0.0.1:43210"), values).headers().clone();
        assert_eq!(real_ip(&headers(&[])), Err("The request has no X-Real-IP"));
        assert_eq!(
            real_ip(&headers(&["192.0.2.10", "192.0.2.11"])),
            Err("X-Real-IP appears more than once")
        );
        for value in [
            "",
            "not-an-ip",
            "192.0.2.10:43210",
            "192.0.2.10, 192.0.2.11",
        ] {
            assert_eq!(
                real_ip(&headers(&[value])),
                Err("X-Real-IP is not an IP address")
            );
        }
        assert_eq!(
            real_ip(&headers(&["192.0.2.10"])),
            Ok("192.0.2.10".parse().unwrap())
        );
    }

    #[test]
    fn shared_client_ip_warning_fires_once_then_hourly() {
        assert!(shared_client_ip_warning_due(i64::MIN, 0));
        assert!(shared_client_ip_warning_due(i64::MIN, i64::MAX));
        assert!(!shared_client_ip_warning_due(1_000, 1_000));
        assert!(!shared_client_ip_warning_due(1_000, 4_599));
        assert!(shared_client_ip_warning_due(1_000, 4_600));
    }

    #[test]
    fn client_ip_without_connection_info_never_trusts_proxy_headers() {
        assert_eq!(
            client_ip(
                &request(None, &["192.0.2.10"]),
                "127.0.0.1:58833".parse().unwrap(),
            ),
            "local"
        );
    }

    #[test]
    fn client_ip_proxy_clients_have_independent_appeal_limits() {
        let limits = Local::default();
        let keys: Vec<_> = ["192.0.2.10", "192.0.2.11"]
            .iter()
            .map(|ip| {
                client_ip(
                    &request(Some("127.0.0.1:43210"), &[ip]),
                    "127.0.0.1:58833".parse().unwrap(),
                )
            })
            .collect();
        for key in keys {
            for _ in 0..10 {
                assert!(limits.allow(&Rule::new("appeal-mail-ip", &key, 10, 3600)));
            }
            assert!(!limits.allow(&Rule::new("appeal-mail-ip", &key, 10, 3600)));
        }
    }
}

#[cfg(test)]
mod return_tests {
    use super::safe_return;

    #[test]
    fn authentication_returns_keep_local_targets_and_reject_external_or_header_paths() {
        for path in [
            "/new/ai",
            "/thread-1-2?view=flat&sub_root=42#floor-51",
            // Already percent-encoded non-ASCII is kept: this decodes to the Chinese for "coins".
            "/search?q=%E9%87%91%E5%B8%81",
        ] {
            assert_eq!(safe_return(path), path);
        }
        for path in [
            "https://other.invalid",
            "//other.invalid",
            "/\\other.invalid",
            "/%2fother.invalid",
            "/%5cother.invalid",
            "/new\r\nLocation: https://other.invalid",
        ] {
            assert!(safe_return(path).is_empty());
        }
        assert!(safe_return(&format!("/{}", "a".repeat(1000))).is_empty());
    }

    #[test]
    fn non_ascii_returns_are_percent_encoded_for_the_location_header() {
        // Chinese for "coins" and "Chinese" in the query and "floor" in the fragment.
        let encoded = safe_return("/search?q=\u{91d1}\u{5e01}+\u{4e2d}\u{6587} x#\u{697c}");
        assert_eq!(
            encoded,
            "/search?q=%E9%87%91%E5%B8%81+%E4%B8%AD%E6%96%87 x#%E6%A5%BC"
        );
        assert!(axum::http::HeaderValue::from_str(&encoded).is_ok());
        // Encoding only ever produces non-ASCII bytes; it never makes up %2F or %5C.
        assert_eq!(safe_return("/ü/ok"), "/%C3%BC/ok");
        assert!(safe_return("//\u{91d1}\u{5e01}").is_empty());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::{origin_failure, same_host, secure_transport};
    use axum::extract::ConnectInfo;
    use axum::http::{Extensions, HeaderMap, HeaderValue};
    use std::net::SocketAddr;

    fn peer(address: &str) -> Extensions {
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(address.parse::<SocketAddr>().unwrap()));
        extensions
    }

    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-proto", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn origin_matches_by_host_and_port_not_by_scheme() {
        // Behind the reverse proxy the application only sees plain HTTP; comparing protocols would judge every
        // HTTPS request cross-site.
        assert!(same_host("https://example.com", "example.com"));
        assert!(same_host("http://example.com", "example.com"));
        // Browsers do not write the default port in Origin; when written explicitly it is still the same host.
        assert!(same_host("http://127.0.0.1:58833", "127.0.0.1:58833"));
        assert!(same_host("https://EXAMPLE.com", "example.com"));
        // A different port is a different origin.
        assert!(!same_host("http://example.com:8080", "example.com"));
        assert!(!same_host("http://example.com", "example.com:8080"));
        // A different host, however similar the prefix or suffix.
        assert!(!same_host("https://evil.test", "example.com"));
        assert!(!same_host("https://example.com.evil.test", "example.com"));
        assert!(!same_host("https://evil.test/example.com", "example.com"));
        // Anything whose host cannot be parsed is never same-origin.
        assert!(!same_host("null", "example.com"));
        assert!(!same_host("", "example.com"));
        assert!(!same_host("example.com", "example.com"));
    }

    #[test]
    fn rejected_requests_say_which_check_turned_them_away() {
        let headers = |pairs: &[(&str, &str)]| {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                let name = axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap();
                headers.insert(name, HeaderValue::from_str(value).unwrap());
            }
            headers
        };
        // Same origin passes: nothing to say.
        assert!(
            origin_failure(&headers(&[
                ("origin", "http://192.0.2.10"),
                ("host", "192.0.2.10")
            ]))
            .is_none()
        );
        // A request with no source at all (non-browser, or same-origin navigation) is left to the token check.
        assert!(origin_failure(&headers(&[("host", "192.0.2.10")])).is_none());

        // This is what turned every form on the test site into a 403: the BaoTa panel wrote
        // `proxy_set_header Host 127.0.0.1`, so the Host the application sees never equals the browser's Origin.
        // The error must show both values, otherwise the page gives no clue at all.
        let proxy = origin_failure(&headers(&[
            ("origin", "http://192.0.2.10"),
            ("host", "127.0.0.1"),
        ]))
        .expect("a proxy that rewrites Host must be judged cross-origin");
        assert!(
            proxy.contains("http://192.0.2.10") && proxy.contains("127.0.0.1"),
            "{proxy}"
        );
        assert!(proxy.contains("reverse proxy"), "{proxy}");

        // A real cross-site submission must likewise say which check stopped it, and not be confused with the one
        // above.
        let cross = origin_failure(&headers(&[
            ("origin", "https://evil.test"),
            ("host", "192.0.2.10"),
            ("sec-fetch-site", "cross-site"),
        ]))
        .expect("a cross-site submission must be rejected");
        assert!(cross.contains("Sec-Fetch-Site"), "{cross}");

        // Referer also counts as a source; without Host same origin cannot be proven.
        let no_host = origin_failure(&headers(&[(
            "referer",
            "http://192.0.2.10/manage/settings",
        )]))
        .expect("no Host header must not pass");
        assert!(no_host.contains("has no Host header"), "{no_host}");
    }

    #[test]
    fn https_is_only_believed_from_a_loopback_proxy() {
        let listen: SocketAddr = "127.0.0.1:58833".parse().unwrap();
        let public: SocketAddr = "0.0.0.0:58833".parse().unwrap();

        // The reverse proxy hop: listener and peer are both loopback, so the forwarded header is trusted.
        assert!(secure_transport(
            &forwarded(&["https"]),
            &peer("127.0.0.1:41000"),
            listen
        ));
        assert!(secure_transport(
            &forwarded(&["HTTPS"]),
            &peer("127.0.0.1:41000"),
            listen
        ));
        assert!(!secure_transport(
            &forwarded(&["http"]),
            &peer("127.0.0.1:41000"),
            listen
        ));
        // No forwarded header means no evidence of HTTPS: the application never terminates TLS itself.
        assert!(!secure_transport(
            &HeaderMap::new(),
            &peer("127.0.0.1:41000"),
            listen
        ));
        // Several hops are writing it; it is unclear which one is trusted.
        assert!(!secure_transport(
            &forwarded(&["https", "http"]),
            &peer("127.0.0.1:41000"),
            listen
        ));
        // A protocol declared by a public peer cannot be trusted, otherwise anyone could make their cookies carry
        // Secure.
        assert!(!secure_transport(
            &forwarded(&["https"]),
            &peer("203.0.113.9:41000"),
            listen
        ));
        // When listening on a public interface, even the forwarded header of a loopback peer is ignored.
        assert!(!secure_transport(
            &forwarded(&["https"]),
            &peer("127.0.0.1:41000"),
            public
        ));
        // Only in-process calls have no connection info.
        assert!(!secure_transport(
            &forwarded(&["https"]),
            &Extensions::new(),
            listen
        ));
    }
}

#[cfg(test)]
mod csp_tests {
    use super::{FALLBACK_CSP, page_csp};

    /// Apart from `frame-src 'self' https://www.youtube-nocookie.com`, the directives are the self-only baseline
    /// word for word, the two versions differ only in the style nonce, and no other external source appears.
    #[test]
    fn csp_only_adds_the_youtube_nocookie_frame_source() {
        const BEFORE: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'";
        let directives =
            |csp: &str| -> Vec<String> { csp.split(';').map(|d| d.trim().to_owned()).collect() };
        let mut expected = directives(BEFORE);
        expected.insert(
            5,
            "frame-src 'self' https://www.youtube-nocookie.com".into(),
        );
        assert_eq!(directives(FALLBACK_CSP), expected);
        let nonce = "0123456789abcdef";
        let page = page_csp(nonce);
        assert_eq!(
            page,
            FALLBACK_CSP.replace(
                "style-src 'self'",
                "style-src 'self' 'nonce-0123456789abcdef'"
            )
        );
        for csp in [FALLBACK_CSP, page.as_str()] {
            let origins: Vec<_> = csp
                .split_whitespace()
                .filter(|token| token.contains("://"))
                .collect();
            assert_eq!(origins, ["https://www.youtube-nocookie.com;"], "{csp}");
            assert_eq!(csp.matches("frame-src").count(), 1);
        }
    }
}
