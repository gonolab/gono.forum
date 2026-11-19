//! Member JSON API: accepts only `Authorization: Bearer` tokens, reads no Cookie and needs no CSRF.
//!
//! Writes reuse the [`ContentWriter`] and [`Interaction`] transactions directly, so quotas, the email threshold, freeze,
//! read-only mode, idempotency and notification rules match the web pages; this module handles only authentication,
//! rate limiting, JSON encoding and decoding, and the error format.
use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{any, get, patch, post, put},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{AppState, auth, posting};
use crate::rate_limit::Rule;
use crate::{
    activities,
    content::{
        self,
        edits::{ContentEditor, EditRequest},
        writes::{
            ContentWriteError, ContentWriter, NewReply, NewThread, Published, ThreadVisibility,
        },
    },
    db,
    identity::{self, ApiKey, IdentityError, Member},
    interaction::{self, Interaction},
    markdown,
};

/// Path prefix of the JSON API; `auth::context` does not build a Cookie identity for this prefix.
pub(super) const PREFIX: &str = "/api/v1";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Hard limit on JSON request bodies (the upper bound 1024 KiB of `settings.api.max_body_kib`). The limit set in the
/// admin area is checked per request in [`ApiJson`] (docs/api.md, "Request bodies"); this is only the outer
/// backstop.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Member API settings read during authentication, put into the request extensions for the subsequent [`ApiJson`] so
/// they need not be fetched again.
#[derive(Clone)]
struct ApiLimits {
    max_body_bytes: usize,
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    let api = Router::new()
        .route("/me", get(me))
        .route("/forums", get(forums))
        .route("/credits", get(credits))
        .route("/check-in", post(check_in))
        .route("/threads", post(create_thread))
        .route("/threads/{tid}", patch(edit_thread))
        .route("/threads/{tid}/activity", get(activity))
        .route("/threads/{tid}/accept", post(accept))
        .route("/threads/{tid}/vote", post(vote))
        .route("/threads/{tid}/enter", post(enter))
        .route("/threads/{tid}/replies", post(reply_thread))
        .route("/posts/{pid}", patch(edit_post))
        .route("/posts/{pid}/replies", post(reply_post))
        .route(
            "/threads/{tid}/favorite",
            put(favorite_on).delete(favorite_off),
        )
        .route("/posts/{pid}/like", put(like_on).delete(like_off))
        // The prefix itself (`/api/v1`) is caught by the nested router's root path and does not fall through to the
        // outer HTML 404.
        .route("/", any(not_found))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(axum::middleware::from_fn(no_store))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));
    // matchit falls back to the nested wildcard segment only when no other branch matches: `/api/v1/comments` would
    // first be caught by the HTML endpoint `/api/{thread}/comments`. The three paths shaped like
    // `/api/{…}/comments|star|follow` are registered explicitly as JSON 404s; static segments take precedence over
    // parameter segments, so they no longer leak back to HTML. The tip endpoint `/api/{thread}/tip` likewise.
    let mut router = Router::new().nest(PREFIX, api);
    for name in ["comments", "star", "follow", "tip"] {
        router = router.route(
            &format!("{PREFIX}/{name}"),
            any(not_found).layer(axum::middleware::from_fn(no_store)),
        );
    }
    router
}

/// Uniform JSON error, the same as `/data/v1`'s (docs/api.md, "Responses"): `{"code","error"}`, the stable code and
/// the text for people.
pub(super) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<i64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    fn unauthorized(message: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_token", message)
    }

    fn not_found(message: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation", message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    fn rate_limited(retry_after: i64) -> Self {
        Self {
            retry_after: Some(retry_after),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                crate::i18n::current().api_error_rate(),
            )
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        tracing::error!(%error, "API request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            crate::i18n::current().api_error_internal(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = json_response(
            self.status,
            json!({"code": self.code, "error": self.message}),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"gonof\""),
            );
        }
        if let Some(seconds) = self.retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(seconds.max(1).unsigned_abs()),
            );
        }
        response
    }
}

impl From<ContentWriteError> for ApiError {
    fn from(error: ContentWriteError) -> Self {
        // The read-only switch is locked again inside the transaction; losing the race to read-only also returns 403,
        // not 500.
        if let ContentWriteError::Internal(inner) = &error
            && let Some(IdentityError::Suspended(message)) = inner.downcast_ref::<IdentityError>()
        {
            return Self::new(StatusCode::FORBIDDEN, "read_only", message.clone());
        }
        let (status, message) = posting::write_error(error);
        let code = match status {
            StatusCode::UNPROCESSABLE_ENTITY => "validation",
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::CONFLICT => "conflict",
            StatusCode::TOO_MANY_REQUESTS => "quota",
            _ => "internal",
        };
        Self::new(status, code, message)
    }
}

impl From<IdentityError> for ApiError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::Validation(message) => Self::validation(message),
            IdentityError::Unauthorized => Self::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                crate::i18n::current().api_error_unauthorized(),
            ),
            IdentityError::Suspended(message) => {
                Self::new(StatusCode::FORBIDDEN, "read_only", message)
            }
            IdentityError::Forbidden => Self::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                crate::i18n::current().api_error_forbidden(),
            ),
            IdentityError::Restricted(message) => {
                Self::new(StatusCode::FORBIDDEN, "forbidden", message)
            }
            IdentityError::NotFound => {
                Self::not_found(crate::i18n::current().api_error_not_found())
            }
            IdentityError::Conflict(message) => {
                Self::new(StatusCode::CONFLICT, "conflict", message)
            }
            IdentityError::Internal(error) => Self::internal(error),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        // Redis enabled but unavailable (rate limiting, cache): like the web pages, return 503 with Retry-After, not
        // 500.
        if error.downcast_ref::<crate::cache::Unavailable>().is_some() {
            let detail = crate::debug::visible_error(&error);
            tracing::error!(error = %detail, "cache unavailable");
            return Self {
                retry_after: Some(3),
                ..Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    crate::i18n::current().api_error_unavailable(),
                )
            };
        }
        Self::internal(error)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error.into())
    }
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        value.to_string(),
    )
        .into_response()
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn not_found() -> ApiError {
    ApiError::not_found(crate::i18n::current().api_error_endpoint())
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        crate::i18n::current().api_error_method(),
    )
}

/// A member who has passed token authentication, the site switch and rate limit checks.
///
/// Reads the member and token only once; `lock_actor`/`author_locked` inside write transactions lock the account again
/// and recheck the credentials version, freeze, email, quotas and whether the token has been revoked; this extractor
/// does not replace that layer.
pub(super) struct ApiActor {
    pub member: Member,
    pub key: ApiKey,
    /// Site URL used for returned absolute URLs: the forum URL, or derived from this request's `Host` when left empty
    /// (the API does not go through the page frame).
    pub site_url: String,
    /// Floor pagination (returned floor URLs are computed from it).
    pub paging: crate::urls::Paging,
    /// Site time zone (`settings.time.offset_minutes`): "today" in read-only endpoints is computed from it.
    pub offset_minutes: i64,
}

impl ApiActor {
    /// Carries the token ID: once the write transaction locks the member row it rechecks against this, so writes queued
    /// before a revocation commits will not commit afterwards.
    fn actor(&self) -> identity::Actor {
        identity::Actor {
            api_key: Some(self.key.id),
            ..self.member.actor()
        }
    }
}

/// The `Authorization` scheme name is case-insensitive (RFC 9110 §11.1); whitespace around the token is ignored.
fn bearer_secret(value: &str) -> Option<&str> {
    let (scheme, secret) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| secret.trim())
}

impl FromRequestParts<Arc<AppState>> for ApiActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // The member API does not go through the page frame: settings come from the process cache; write decisions
        // still read the database with locks through each write transaction's write gate.
        let settings = state.global.settings(&state.db).await?.settings.clone();
        // Outside the page frame, the site time zone must likewise be written into this request: the
        // `YYYY-MM-DDTHH:MM` form of deadlines is parsed with it, otherwise it would fall back to the default
        // (UTC+8), and the API would not follow a change of the site time zone.
        crate::clock::set(crate::clock::TimeFormat::from(&*settings));
        let config = &settings.api;
        let failures = u32::try_from(config.auth_failures_per_5min).unwrap_or(0);
        let client_ip =
            auth::client_ip_parts(&parts.headers, &parts.extensions, state.config.listen);
        let failure = Rule::new(
            "api-auth-failure",
            &client_ip,
            failures,
            AUTH_FAILURE_WINDOW,
        );
        let secret = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_secret)
            .filter(|secret| identity::valid_api_secret(secret));
        let Some(secret) = secret else {
            // Malformed input does not query the database or use up the failure quota; a full bucket still returns 429
            // first.
            if state.limits.blocked(std::slice::from_ref(&failure)).await? {
                return Err(ApiError::rate_limited(AUTH_FAILURE_WINDOW));
            }
            return Err(ApiError::unauthorized(
                crate::i18n::current().api_error_bearer(),
            ));
        };
        let Some(attempt) = state.limits.reserve_failure(vec![failure]).await? else {
            return Err(ApiError::rate_limited(AUTH_FAILURE_WINDOW));
        };
        let (key, member) = match state.identity.authenticate_api_key(secret).await {
            Ok(None) => {
                return Err(ApiError::unauthorized(
                    crate::i18n::current().api_error_token(),
                ));
            }
            Ok(Some(authenticated)) => authenticated,
            Err(error) => {
                attempt.release().await?;
                return Err(error.into());
            }
        };
        attempt.release().await?;
        if !member.active() || member.frozen {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "account_restricted",
                crate::i18n::current().api_error_account(),
            ));
        }
        if !settings.api.enabled {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "api_disabled",
                crate::i18n::current().api_error_disabled(),
            ));
        }
        let write = !parts.method.is_safe();
        if write && settings.site.read_only {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "read_only",
                settings.site.read_only_notice(crate::i18n::current()),
            ));
        }
        let per_minute = |value: i64| u32::try_from(value).unwrap_or(0);
        let mut rules = vec![Rule::new(
            "api-requests",
            key.id,
            per_minute(config.requests_per_minute),
            60,
        )];
        if write {
            rules.push(Rule::new(
                "api-writes",
                key.id,
                per_minute(config.writes_per_minute),
                60,
            ));
        }
        if !state.limits.allow(&rules).await? {
            return Err(ApiError::rate_limited(60));
        }
        parts.extensions.insert(ApiLimits {
            max_body_bytes: usize::try_from(config.max_body_kib)
                .unwrap_or(0)
                .saturating_mul(1024),
        });
        if let Err(error) = state
            .identity
            .touch_api_key(key.id, &client_ip, identity::now())
            .await
        {
            tracing::warn!(%error, "failed to record API token use time");
        }
        let site_url = crate::runtime::site_origin(
            &settings,
            auth::request_origin(
                &parts.headers,
                &parts.uri,
                &parts.extensions,
                state.config.listen,
            )
            .as_deref(),
        )
        .map_err(|error| ApiError::from(anyhow::Error::new(error)))?;
        Ok(Self {
            member,
            key,
            site_url,
            paging: crate::urls::Paging::from_settings(&settings),
            offset_minutes: settings.time.offset_minutes,
        })
    }
}

/// Window for rate limiting authentication failures, kept constant; the count is the admin setting
/// `settings.api.auth_failures_per_5min`.
const AUTH_FAILURE_WINDOW: i64 = 300;

/// JSON request body: non-JSON types, oversize bodies and parse failures are all returned in the uniform error format.
pub(super) struct ApiJson<T>(pub T);

impl<T: DeserializeOwned> FromRequest<Arc<AppState>> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(
        request: Request,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                crate::i18n::current().api_error_content_type(),
            ));
        }
        // The limit is an admin setting (`settings.api.max_body_kib`), checked per request; it was already read during
        // authentication, and is fetched again only for unauthenticated requests (which do not happen).
        let limit = match request.extensions().get::<ApiLimits>() {
            Some(limits) => limits.max_body_bytes,
            None => {
                let settings = state.global.settings(&state.db).await?.settings.clone();
                usize::try_from(settings.api.max_body_kib)
                    .unwrap_or(0)
                    .saturating_mul(1024)
            }
        }
        .min(MAX_BODY_BYTES);
        let bytes = axum::body::to_bytes(request.into_body(), limit)
            .await
            .map_err(|error| {
                let too_large = std::error::Error::source(&error)
                    .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
                    || error.into_inner().is::<http_body_util::LengthLimitError>();
                if too_large {
                    ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "payload_too_large",
                        crate::i18n::current().api_error_body_size(),
                    )
                } else {
                    ApiError::bad_request(crate::i18n::current().api_error_body())
                }
            })?;
        serde_json::from_slice(&bytes).map(Self).map_err(|error| {
            ApiError::bad_request(crate::i18n::current().api_error_json(error.to_string()))
        })
    }
}

/// The digest of `Idempotency-Key` is used as the request key; when not provided a random one is generated, with no
/// replay protection.
fn request_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let Some(value) = headers.get(IDEMPOTENCY_HEADER) else {
        return Ok(identity::random_token());
    };
    let invalid = || ApiError::bad_request(crate::i18n::current().api_error_idempotency());
    let value = value.to_str().map_err(|_| invalid())?;
    if value.is_empty()
        || value.len() > 200
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(invalid());
    }
    Ok(identity::token_hash(value))
}

fn positive_id(raw: &str, missing: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::not_found(missing))
}

fn writer(state: &AppState, actor: &ApiActor) -> ContentWriter {
    ContentWriter::new(state.db.clone(), state.config.content.clone())
        .with_codes(state.config.identity.two_factor.clone())
        .with_source("api")
        .with_site_url(actor.site_url.clone())
}

fn editor(state: &AppState, actor: &ApiActor) -> ContentEditor {
    ContentEditor::new(
        state.db.clone(),
        state.config.content.clone(),
        state.config.media.orphan_seconds,
    )
    .with_source("api")
    .with_site_url(actor.site_url.clone())
    .with_global(state.global.clone())
}

async fn location(
    state: &AppState,
    actor: &ApiActor,
    tid: i64,
    pid: i64,
) -> Result<String, ApiError> {
    // The member API does not go through the web page frame: the forums readable by the member are read from the forum
    // table once (the thread row has no copy of its forum's visibility).
    let forums = content::readable_forums(&state.db, Some(actor.member.uid)).await?;
    let path = content::post_location_for_viewer(
        &state.db,
        u64::try_from(tid).unwrap_or_default(),
        pid,
        false,
        content::Readable {
            uid: Some(actor.member.uid),
            forums: &forums,
        },
        actor.paging,
    )
    .await?
    .unwrap_or_else(|| format!("/thread-{tid}"));
    Ok(format!("{}{path}", actor.site_url))
}

fn attachments(body: &str) -> Result<Vec<i64>, ApiError> {
    markdown::references(body)
        .map(|refs| refs.attachments.into_iter().collect())
        .map_err(|error| ApiError::validation(error.to_string()))
}

async fn me(actor: ApiActor) -> Result<Response, ApiError> {
    let member = &actor.member;
    Ok(json_response(
        StatusCode::OK,
        json!({
            "uid": member.uid,
            "username": member.username,
            "email_verified": member.email_verified,
            "can_post": member.can_contribute(),
            "profile_url": format!("{}{}", actor.site_url, crate::urls::space(member.uid)),
            "key": {
                "hint": actor.key.hint,
                "created_at": actor.key.dateline,
                "expires_at": actor.key.expires_at,
                "last_used_at": actor.key.last_used_at,
            },
        }),
    ))
}

/// Forums readable by a signed-in member, taken from the member copy of the process forum cache
/// (docs/settings-and-caching.md, "The process cache"), in unchanged `displayorder, fid` order.
async fn forums(
    State(state): State<Arc<AppState>>,
    _actor: ApiActor,
) -> Result<Response, ApiError> {
    let ttl = state.global.settings(&state.db).await?.settings.cache_ttl();
    let all = state.global.forums(&state.db, true, ttl).await?;
    // Multi-domain sites: list only forums within this site's scope; posting still ignores the scope.
    let (inside, _) = crate::site::partition_forums(&all);
    let forums: Vec<Value> = inside
        .iter()
        .map(|forum| {
            json!({
                "fid": forum.fid,
                "slug": forum.slug,
                "name": forum.name,
                "description": forum.description,
                // null for top-level forums (fields only added, order unchanged, existing callers not broken).
                "parent_fid": forum.parent_fid,
            })
        })
        .collect();
    Ok(json_response(StatusCode::OK, json!({"forums": forums})))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ForumRef {
    Id(i64),
    Slug(String),
}

/// Deadlines accept UTC Unix seconds, or the same `YYYY-MM-DDTHH:MM` as the web form (site time zone).
#[derive(Deserialize)]
#[serde(untagged)]
enum DeadlineInput {
    Seconds(i64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadInput {
    forum: ForumRef,
    title: String,
    body: String,
    /// 0 ordinary, 1 bounty, 2 red packet, 3 lottery, 4 poll, 5 news, matching the categories of the web form.
    #[serde(default)]
    category: i64,
    #[serde(default)]
    budget: i64,
    #[serde(default)]
    shares: i64,
    #[serde(default)]
    distribution: Option<String>,
    #[serde(default)]
    deadline: Option<DeadlineInput>,
    #[serde(default)]
    max_choices: Option<i64>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    source_name: String,
    #[serde(default)]
    source_author: String,
    /// Lottery: `reply` (default, valid replies enter automatically) or `click` (one-click entry).
    #[serde(default)]
    entry_mode: Option<String>,
    /// Participation requirements for one-click entry, each of which must be confirmed when entering.
    #[serde(default)]
    requirements: Vec<String>,
    /// Entries are reviewed by the author before the draw.
    #[serde(default)]
    review: bool,
    /// Allows the author to draw early.
    #[serde(default)]
    early_draw: bool,
    /// Lottery prizes; when not given, `budget` and `shares` describe a single credits prize.
    #[serde(default)]
    prizes: Vec<activities::PrizeSpec>,
    /// `public` (default) or `members`; accepted as a string and then validated, so unknown values return 422 rather
    /// than 400.
    #[serde(default)]
    visibility: Option<String>,
}

/// Only `public` and `members` are accepted; other values are a field validation failure (422), not a JSON parse
/// failure.
fn thread_visibility(value: Option<&str>) -> Result<Option<ThreadVisibility>, ApiError> {
    value
        .map(|value| {
            ThreadVisibility::parse(value)
                .ok_or_else(|| ApiError::validation(crate::i18n::current().api_error_visibility()))
        })
        .transpose()
}

impl ThreadInput {
    /// Defaults match the web form: distribution `equal`, 1 selectable option. Required fields per category, budget
    /// range, divisibility of shares and option count are all judged by `ActivitySpec::validate` in the publish
    /// transaction.
    fn activity(&self) -> activities::ActivitySpec {
        activities::ActivitySpec {
            kind: self.category,
            budget: self.budget,
            shares: self.shares,
            distribution: self.distribution.clone().unwrap_or_else(|| "equal".into()),
            deadline: match &self.deadline {
                None => 0,
                Some(DeadlineInput::Seconds(seconds)) => *seconds,
                Some(DeadlineInput::Text(text)) => posting::parse_deadline(text),
            },
            max_choices: self.max_choices.unwrap_or(1),
            options: self
                .options
                .iter()
                .map(|option| option.trim())
                .filter(|option| !option.is_empty())
                .map(str::to_owned)
                .collect(),
            source_url: self.source_url.trim().to_owned(),
            source_name: self.source_name.trim().to_owned(),
            source_author: self.source_author.trim().to_owned(),
            entry_mode: self.entry_mode.clone().unwrap_or_else(|| "reply".into()),
            requirements: self
                .requirements
                .iter()
                .map(|item| item.trim().to_owned())
                .collect(),
            review: self.review,
            early_draw: self.early_draw,
            prizes: self.prizes.clone(),
        }
    }
}

/// The posting target is looked up by ID or slug among the forums readable by the signed-in member, taken from the
/// member copy of the process forum cache (docs/settings-and-caching.md, "The process cache"), the same source as the
/// web posting page's dropdown. This is only a pre-check outside the transaction: after locking the forum path, the
/// publish transaction still rechecks readability against the database.
///
/// A retry of a thread that was already published looks its key up before anything else in the transaction, and only
/// replays (or refuses a different request); it needs no board it can find now. So when the reference finds no board,
/// a key already recorded supplies one: a forum ID as sent (the number the original digest has), and for a slug —
/// renamed or taken away since — the board of the post the record points to.
async fn resolve_forum(
    state: &AppState,
    actor: &ApiActor,
    headers: &HeaderMap,
    key: &str,
    forum: &ForumRef,
) -> Result<i64, ApiError> {
    let ttl = state.global.settings(&state.db).await?.settings.cache_ttl();
    let forums = state.global.forums(&state.db, true, ttl).await?;
    let found = match forum {
        ForumRef::Id(fid) => forums.iter().find(|f| f.fid == *fid),
        ForumRef::Slug(slug) => forums.iter().find(|f| f.slug == slug.trim()),
    };
    if let Some(found) = found {
        return Ok(found.fid);
    }
    // Without the header the key is random and has no record.
    let recorded = if headers.contains_key(IDEMPOTENCY_HEADER) {
        db::query_scalar::<i64>(
            "SELECT p.fid FROM gonof_forum_post_request r JOIN gonof_forum_post p ON p.pid = r.pid
             WHERE r.uid = ? AND r.request_key = ?",
        )
        .bind(actor.member.uid)
        .bind(key)
        .fetch_optional(state.db.executor())
        .await?
    } else {
        None
    };
    recorded
        .map(|recorded| match forum {
            ForumRef::Id(fid) => *fid,
            ForumRef::Slug(_) => recorded,
        })
        .ok_or_else(|| ApiError::not_found(crate::i18n::current().api_error_forum()))
}

async fn create_thread(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ThreadInput>,
) -> Result<Response, ApiError> {
    let key = request_key(&headers)?;
    let fid = resolve_forum(&state, &actor, &headers, &key, &input.forum).await?;
    let visibility = thread_visibility(input.visibility.as_deref())?.unwrap_or_default();
    let attachment_ids = attachments(&input.body)?;
    // Category and activity fields are validated outside the transaction first, giving per-field validation messages;
    // the publish transaction validates once more against the time after locking (an invalid category rolls back the
    // whole publish), and freezes the budget.
    let activity = input.activity();
    let deadline_text = match &input.deadline {
        Some(DeadlineInput::Text(text)) => Some(text.clone()),
        _ => None,
    };
    let credit = state
        .global
        .settings(&state.db)
        .await?
        .settings
        .site
        .credit_word(crate::i18n::current());
    let checked = activity
        .validate(identity::now(), &credit)
        .map_err(
            |error| match error.downcast_ref::<activities::ActivityFailure>() {
                Some(failure) => ApiError::validation(failure.0.clone()),
                None => ApiError::internal(error),
            },
        );
    let thread = NewThread {
        activity,
        fid,
        title: input.title,
        body: input.body,
        attachment_ids,
        request_key: key,
        visibility,
    };
    let published = match checked {
        Ok(()) => publish(&state, &actor, thread, deadline_text.as_deref()).await?,
        // A retry of a publication that succeeded replays with its original result even after its activity has
        // ended (docs/api.md, "Idempotency keys"): the transaction looks the key up before anything else. A new
        // request fails inside the transaction as well, and gets the message of this check.
        Err(invalid) => match publish(&state, &actor, thread, deadline_text.as_deref()).await {
            Ok(published) => published,
            Err(_) => return Err(invalid),
        },
    };
    // A new thread points to the thread page itself, without the first post's floor anchor.
    let url = format!("{}/thread-{}", actor.site_url, published.tid);
    Ok(json_response(
        // When the same request key was already written, the original result is replayed with 200 instead of 201;
        // decided by the write transaction under the lock.
        if published.replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        json!({
            "tid": published.tid,
            "pid": published.pid,
            "position": published.position,
            "url": url,
        }),
    ))
}

/// A deadline sent as text is a local time in the site's time zone; the publish transaction keeps that text, not the
/// moment it was read as, in the request digest (`ContentWriter::publish_actor_typed`).
async fn publish(
    state: &AppState,
    actor: &ApiActor,
    thread: NewThread,
    deadline_text: Option<&str>,
) -> Result<Published, ContentWriteError> {
    let writer = writer(state, actor);
    match deadline_text {
        Some(text) => {
            writer
                .publish_actor_typed(actor.actor(), thread, text)
                .await
        }
        None => writer.publish_actor(actor.actor(), thread).await,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyInput {
    body: String,
}

async fn reply_thread(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ReplyInput>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    submit_reply(&state, &actor, &headers, tid, None, input.body).await
}

async fn reply_post(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(pid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ReplyInput>,
) -> Result<Response, ApiError> {
    let pid = positive_id(&pid, crate::i18n::current().api_error_reply())?;
    // Discovery reads only locate the target; permission, visibility and the two-level relationship are validated by
    // the reply transaction under the lock.
    let row = db::query("SELECT tid, root_pid, first FROM gonof_forum_post WHERE pid = ?")
        .bind(pid)
        .fetch_optional(state.db.executor())
        .await?
        .ok_or_else(|| ApiError::not_found(crate::i18n::current().api_error_reply()))?;
    if row.try_get::<i64, _>("first")? == 1 {
        return Err(ApiError::validation(
            crate::i18n::current().api_error_nested_first(),
        ));
    }
    let tid: i64 = row.try_get("tid")?;
    let root = row.try_get::<Option<i64>, _>("root_pid")?.unwrap_or(pid);
    submit_reply(&state, &actor, &headers, tid, Some((root, pid)), input.body).await
}

async fn submit_reply(
    state: &AppState,
    actor: &ApiActor,
    headers: &HeaderMap,
    tid: i64,
    target: Option<(i64, i64)>,
    body: String,
) -> Result<Response, ApiError> {
    let attachment_ids = attachments(&body)?;
    let key = request_key(headers)?;
    let published = writer(state, actor)
        .reply_actor(
            actor.actor(),
            NewReply {
                tid,
                root_pid: target.map(|(root, _)| root),
                reply_to_pid: target.map(|(_, reply_to)| reply_to),
                body,
                attachment_ids,
                request_key: key,
            },
        )
        .await?;
    let url = location(state, actor, published.tid, published.pid).await?;
    let mut value = json!({
        "tid": published.tid,
        "pid": published.pid,
        "position": published.position,
        "url": url,
    });
    if let Some((root, _)) = target {
        value["root_pid"] = json!(root);
    }
    Ok(json_response(
        // When the same request key was already written, the original result is replayed with 200 instead of 201;
        // decided by the write transaction under the lock.
        if published.replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        value,
    ))
}

/// The activity transaction rejects all rules on author, status, deadline, options, balance and so on with
/// `ActivityFailure`, mapped to 422 `activity_rejected`; read-only mode found by the check-in transaction is
/// 403 `read_only`; other errors are database failures.
fn activity_error(error: anyhow::Error) -> ApiError {
    if let Some(IdentityError::Suspended(message)) = error.downcast_ref::<IdentityError>() {
        return ApiError::new(StatusCode::FORBIDDEN, "read_only", message.clone());
    }
    match error.downcast_ref::<activities::ActivityFailure>() {
        Some(failure) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "activity_rejected",
            failure.0.clone(),
        ),
        None => ApiError::internal(error),
    }
}

/// The activity card includes the member's own vote, claim and accepted-answer status, from the same source as the web
/// card, and does not go into the shared cache.
async fn activity(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    let card = activities::card(
        &state.db,
        tid,
        Some(activities::CardViewer::of(&actor.member)),
    )
    .await?
    .ok_or_else(|| ApiError::not_found(crate::i18n::current().api_error_activity()))?;
    Ok(json_response(
        StatusCode::OK,
        serde_json::to_value(card).map_err(anyhow::Error::from)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptInput {
    pid: i64,
}

async fn accept(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<AcceptInput>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    let key = request_key(&headers)?;
    activities::accept_live(&state.db, actor.actor(), tid, input.pid, &key)
        .await
        .map_err(activity_error)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"tid": tid, "accepted_pid": input.pid}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoteInput {
    choices: Vec<i64>,
}

async fn vote(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<VoteInput>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    let key = request_key(&headers)?;
    activities::vote_live(&state.db, actor.actor(), tid, input.choices.clone(), &key)
        .await
        .map_err(activity_error)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"tid": tid, "choices": input.choices}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterInput {
    /// IDs of the participation requirements confirmed one by one (from 1, see the activity card's `requirements`);
    /// omitted when there are no requirements.
    #[serde(default)]
    confirm: Vec<i64>,
}

/// One-click entry. Idempotent per member and thread, without Idempotency-Key: returns `entered=false` when already
/// entered.
async fn enter(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    ApiJson(input): ApiJson<EnterInput>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    let entered = activities::enter_live(&state.db, actor.actor(), tid, &input.confirm)
        .await
        .map_err(activity_error)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"tid": tid, "entered": entered}),
    ))
}

async fn check_in(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
) -> Result<Response, ApiError> {
    // Check-in is idempotent per member and natural day in the site time zone, without Idempotency-Key: a repeated
    // check-in on the same day returns credited=false.
    let credited = activities::check_in_live(&state.db, actor.actor())
        .await
        .map_err(activity_error)?;
    Ok(json_response(StatusCode::OK, json!({"credited": credited})))
}

async fn credits(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
) -> Result<Response, ApiError> {
    let page = activities::credit_page(
        &state.db,
        actor.member.uid,
        0,
        identity::now(),
        actor.offset_minutes,
    )
    .await?;
    Ok(json_response(
        StatusCode::OK,
        json!({
            "available": page.available,
            "frozen": page.frozen,
            "checked_in": page.checked_in,
        }),
    ))
}

/// Current version of the edit target: used only to fill in the default version; author identity, time window,
/// visibility and version conflicts are all judged by the edit transaction under the lock, and this read grants no
/// permission.
struct CurrentPost {
    pid: i64,
    first: bool,
    version: i64,
    thread_version: i64,
}

async fn current_post(
    state: &AppState,
    condition: &str,
    id: i64,
) -> Result<Option<CurrentPost>, ApiError> {
    let row = db::query(&format!(
        "SELECT p.pid, p.first, p.version, t.version AS thread_version
         FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid WHERE {condition}"
    ))
    .bind(id)
    .fetch_optional(state.db.executor())
    .await?;
    row.map(|row| -> anyhow::Result<CurrentPost> {
        Ok(CurrentPost {
            pid: row.try_get("pid")?,
            first: row.try_get::<i64, _>("first")? == 1,
            version: row.try_get("version")?,
            thread_version: row.try_get("thread_version")?,
        })
    })
    .transpose()
    .map_err(ApiError::from)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadEditInput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    expected_version: Option<i64>,
    #[serde(default)]
    visibility: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostEditInput {
    body: String,
    #[serde(default)]
    expected_version: Option<i64>,
}

/// Omitted fields keep their current values; when `expected_version` is omitted the current first post version is
/// submitted, and as with web editing the transaction compares versions and returns 409 on mismatch.
async fn edit_thread(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ThreadEditInput>,
) -> Result<Response, ApiError> {
    let tid = positive_id(&tid, crate::i18n::current().api_error_thread())?;
    let visibility = thread_visibility(input.visibility.as_deref())?;
    if input.title.is_none() && input.body.is_none() && visibility.is_none() {
        return Err(ApiError::validation(
            crate::i18n::current().api_error_edit_empty(),
        ));
    }
    let current = current_post(&state, "p.first_tid = ?", tid)
        .await?
        .ok_or_else(|| ApiError::not_found(crate::i18n::current().api_error_thread()))?;
    submit_edit(
        &state,
        &actor,
        &headers,
        current,
        input.title,
        input.body,
        visibility,
        input.expected_version,
    )
    .await
}

async fn edit_post(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(pid): Path<String>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<PostEditInput>,
) -> Result<Response, ApiError> {
    let pid = positive_id(&pid, crate::i18n::current().api_error_reply())?;
    let current = current_post(&state, "p.pid = ?", pid)
        .await?
        .ok_or_else(|| ApiError::not_found(crate::i18n::current().api_error_reply()))?;
    if current.first {
        return Err(ApiError::validation(
            crate::i18n::current().api_error_edit_first(),
        ));
    }
    submit_edit(
        &state,
        &actor,
        &headers,
        current,
        None,
        Some(input.body),
        None,
        input.expected_version,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn submit_edit(
    state: &AppState,
    actor: &ApiActor,
    headers: &HeaderMap,
    current: CurrentPost,
    title: Option<String>,
    body: Option<String>,
    visibility: Option<ThreadVisibility>,
    expected_version: Option<i64>,
) -> Result<Response, ApiError> {
    let key = request_key(headers)?;
    // The version comes from the edit transaction (or, for a replay, from the request record): a read after the commit
    // could return another edit's version for a body this client has never seen.
    let edited = editor(state, actor)
        .edit_versioned(
            actor.actor(),
            EditRequest {
                pid: current.pid,
                expected_post_version: expected_version.unwrap_or(current.version),
                expected_thread_version: current.thread_version,
                title,
                body,
                request_key: key,
                reason: String::new(),
                visibility,
            },
        )
        .await?;
    let published = edited.published;
    let url = if current.first {
        format!("{}/thread-{}", actor.site_url, published.tid)
    } else {
        location(state, actor, published.tid, published.pid).await?
    };
    Ok(json_response(
        StatusCode::OK,
        json!({
            "tid": published.tid,
            "pid": published.pid,
            "position": published.position,
            "version": edited.version,
            "url": url,
        }),
    ))
}

async fn favorite_on(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_favorite(&state, &actor, &headers, &tid, true).await
}

async fn favorite_off(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(tid): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_favorite(&state, &actor, &headers, &tid, false).await
}

async fn set_favorite(
    state: &AppState,
    actor: &ApiActor,
    headers: &HeaderMap,
    tid: &str,
    desired: bool,
) -> Result<Response, ApiError> {
    let tid = positive_id(tid, crate::i18n::current().api_error_thread())?;
    let key = request_key(headers)?;
    Interaction::new(state.db.clone())
        .set_favorite(actor.actor(), tid, desired, &key)
        .await?;
    let favorited = interaction::favorite_state(&state.db, actor.member.uid, tid).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"tid": tid, "favorited": favorited}),
    ))
}

async fn like_on(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(pid): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_like(&state, &actor, &headers, &pid, true).await
}

async fn like_off(
    State(state): State<Arc<AppState>>,
    actor: ApiActor,
    Path(pid): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_like(&state, &actor, &headers, &pid, false).await
}

async fn set_like(
    state: &AppState,
    actor: &ApiActor,
    headers: &HeaderMap,
    pid: &str,
    desired: bool,
) -> Result<Response, ApiError> {
    let pid = positive_id(pid, crate::i18n::current().api_error_reply())?;
    let key = request_key(headers)?;
    Interaction::new(state.db.clone())
        .set_reaction(actor.actor(), pid, desired, &key)
        .await?;
    let values = interaction::reactions(&state.db, Some(actor.member.uid), &[pid]).await?;
    let value = values.get(&pid).copied().unwrap_or_default();
    Ok(json_response(
        StatusCode::OK,
        json!({"pid": pid, "liked": value.active, "count": value.count}),
    ))
}
