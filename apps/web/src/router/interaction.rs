//! Private collections and progressively enhanced interaction controls.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{content::writes::ContentWriteError, identity, notifications};
use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath},
    http::HeaderMap,
    routing::post,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/favorites", get(favorites_page))
        .route("/my/notifications", get(notifications_page))
        .route("/api/{target}/star", post(favorite))
        .route("/api/comments/{pid}/vote", post(reaction))
        .route("/api/comments/{pid}/downvote", post(downvote))
        .route("/my/notifications/read-all", post(read_all))
        .route("/my/notifications/{id}/read", post(read))
        .route("/my/notifications/{id}/open", post(open))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn(interaction_errors))
}

async fn interaction_errors(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let htmx = posting::hx(request.headers());
    let mut response = next.run(request).await;
    if htmx
        && (response.status().is_client_error() || response.status().is_server_error())
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"text/html"))
    {
        response
            .headers_mut()
            .insert("X-Interaction-Page", HeaderValue::from_static("true"));
        response
            .headers_mut()
            .insert("HX-Retarget", HeaderValue::from_static("body"));
        response
            .headers_mut()
            .insert("HX-Reswap", HeaderValue::from_static("innerHTML"));
    }
    response
}

#[derive(Deserialize)]
struct StateForm {
    _csrf: String,
    request_key: String,
    desired: bool,
    #[serde(default)]
    location: String,
}
csrf_forms!(StateForm);

#[derive(Default, Deserialize)]
struct ListQuery {
    page: Option<usize>,
    #[serde(default)]
    category: String,
}

#[derive(Default, Deserialize)]
struct ReadForm {
    _csrf: String,
    request_key: String,
    #[serde(default)]
    max_id: i64,
    #[serde(default)]
    page: usize,
    /// The category tab at submission time; after marking as read, return to the same tab.
    #[serde(default)]
    category: String,
}
csrf_forms!(ReadForm);

pub(super) struct FavoriteControl {
    pub tid: i64,
    pub active: bool,
    /// The thread page shows the total number of favorites; the personal favorites list does not query counts per row.
    pub count: Option<i64>,
    pub can_change: bool,
    pub location: String,
    pub request_key: String,
}

pub(super) struct ReactionControl {
    pub pid: i64,
    pub count: i64,
    pub active: bool,
    pub available: bool,
    pub own: bool,
    pub can_change: bool,
    pub location: String,
    pub request_key: String,
    /// The partial refresh of a downvote also replaces the first post's like control (the two are mutually exclusive).
    pub oob: bool,
}

pub(super) fn reaction_control(
    state: &AppState,
    pid: i64,
    authorid: Option<i64>,
    available: bool,
    location: String,
    value: Option<&crate::interaction::ReactionState>,
) -> ReactionControl {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let own = member.is_some_and(|member| Some(member.uid) == authorid);
    let active = value.is_some_and(|value| value.active);
    ReactionControl {
        pid,
        count: if available {
            value.map(|value| value.count).unwrap_or(0)
        } else {
            0
        },
        active,
        available,
        own,
        can_change: member.is_some_and(|member| member.can_contribute())
            && (active || available && !own),
        location,
        request_key: identity::random_token(),
        oob: false,
    }
}

/// The thread downvote control: first post only; the author sees the downvote count, others only
/// their own state.
pub(super) struct DownvoteControl {
    pub pid: i64,
    pub active: bool,
    pub can_change: bool,
    pub count: Option<i64>,
    pub location: String,
    pub request_key: String,
    pub oob: bool,
}

/// `available` means the thread can currently be downvoted (not withdrawn); `blocked_by_author` means the author has
/// blocked the reader.
pub(super) fn downvote_control(
    state: &AppState,
    pid: i64,
    authorid: Option<i64>,
    available: bool,
    blocked_by_author: bool,
    location: String,
    value: crate::interaction::DownvoteState,
) -> DownvoteControl {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let own = member.is_some_and(|member| Some(member.uid) == authorid);
    DownvoteControl {
        pid,
        active: value.active,
        can_change: member.is_some_and(|member| member.can_contribute())
            && !own
            && (value.active || available && !blocked_by_author),
        count: value.count,
        location,
        request_key: identity::random_token(),
        oob: false,
    }
}

#[derive(Template)]
#[template(path = "partials/downvote_response.html")]
struct DownvoteTemplate<'a> {
    p: PageMeta<'a>,
    downvote: DownvoteControl,
    reaction: ReactionControl,
}

#[derive(Template)]
#[template(path = "partials/favorite.html")]
struct FavoriteTemplate<'a> {
    p: PageMeta<'a>,
    favorite: FavoriteControl,
}

#[derive(Template)]
#[template(path = "partials/reaction_response.html")]
struct ReactionTemplate<'a> {
    p: PageMeta<'a>,
    reaction: ReactionControl,
    highlights: super::highlights::Section,
    /// Liking the first post also replaces the downvote control (a like removes the member's own downvote).
    downvote: Option<DownvoteControl>,
}

struct FavoriteRow {
    title: String,
    available: bool,
    time: String,
    control: FavoriteControl,
}

#[derive(Template)]
#[template(path = "user/favorites.html", blocks = ["collection"])]
struct FavoritesTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<FavoriteRow>,
    pages: Vec<PageLink>,
}

struct NotificationRow {
    item: notifications::Notification,
    time: String,
    action: &'static str,
}

#[derive(Template)]
#[template(path = "user/notifications.html", blocks = ["collection"])]
struct NotificationsTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<NotificationRow>,
    pages: Vec<PageLink>,
    page: usize,
    max_id: i64,
    category: String,
    can_change: bool,
    fragment: bool,
    request_key: String,
}

fn failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}

#[allow(clippy::result_large_err)]
fn actor(state: &AppState) -> Result<identity::Actor, Response> {
    state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.actor())
        .ok_or_else(|| {
            error_page(
                state,
                StatusCode::UNAUTHORIZED,
                crate::i18n::current().my_error_log_in(),
            )
        })
}

/// Toggle counts for favorites and likes: the per-account limits per minute and per hour are the admin settings
/// `settings.limit.interaction_per_*` (constant windows), shared by both.
///
/// Every toggle writes a request row, and the limit stops request-row flooding. A page has at most 20 floors, so
/// clicking each one would not use half of it.
/// With Redis enabled it uses the Redis sliding log like uploads and private messages, with one count shared across
/// processes. The API additionally has per-token write rate limits.
const TOGGLE_BURST_WINDOW: i64 = 60;
const TOGGLE_HOURLY_WINDOW: i64 = 3600;
/// Marking notifications as read and opening them: 60 per account per minute.
const READ_BURST: (u32, i64) = (60, 60);

pub(super) async fn toggle_limited(state: &AppState, uid: i64) -> Option<Response> {
    let limit = &state.frame.settings.limit;
    let (burst, burst_window) = (
        u32::try_from(limit.interaction_per_minute).unwrap_or(0),
        TOGGLE_BURST_WINDOW,
    );
    let (hourly, hourly_window) = (
        u32::try_from(limit.interaction_per_hour).unwrap_or(0),
        TOGGLE_HOURLY_WINDOW,
    );
    let rules = [
        Rule::new("interaction-toggle", uid, burst, burst_window),
        Rule::new("interaction-toggle-hour", uid, hourly, hourly_window),
    ];
    rejected(state, &rules).await
}

async fn read_limited(state: &AppState, uid: i64) -> Option<Response> {
    let (burst, window) = READ_BURST;
    rejected(state, &[Rule::new("notification-read", uid, burst, window)]).await
}

/// `None` when allowed; over the limit gives the 429 page, and Redis enabled but unavailable gives 503.
async fn rejected(state: &AppState, rules: &[Rule]) -> Option<Response> {
    match state.limit(rules).await {
        Ok(true) => None,
        Ok(false) => Some(error_page(
            state,
            StatusCode::TOO_MANY_REQUESTS,
            crate::i18n::current().my_error_too_often(),
        )),
        Err(unavailable) => Some(unavailable),
    }
}

fn read_gate(state: &AppState, path: &str) -> Option<Response> {
    state.viewer.session.is_none().then(|| {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("return_to", path)
            .finish();
        auth::redirect(&format!("/login?{query}"))
    })
}

/// Unknown categories are treated as "All".
fn known_category(category: &str) -> &str {
    if notifications::category_types(category).is_empty() {
        ""
    } else {
        category
    }
}

/// The first-page URL of one category of the notification list; the empty string is all.
fn category_base(category: &str) -> String {
    if category.is_empty() {
        "/my/notifications".to_owned()
    } else {
        format!("/my/notifications?category={category}")
    }
}

/// base may already carry the category query string, so pick the right separator when appending the page number.
fn list_path(base: &str, page: usize) -> String {
    if page == 1 {
        base.into()
    } else if base.contains('?') {
        format!("{base}&page={page}")
    } else {
        format!("{base}?page={page}")
    }
}

fn pages(base: &str, total: usize, current: usize, page_size: usize) -> Vec<PageLink> {
    page_window(current, total.div_ceil(page_size))
        .map(|number| PageLink {
            number,
            href: list_path(base, number),
            active: number == current,
        })
        .collect()
}

fn fragment(html: Result<String, askama::Error>) -> Response {
    match html {
        Ok(html) => (
            [
                (header::VARY, "HX-Request"),
                (header::CACHE_CONTROL, "private, no-store"),
            ],
            Html(html),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to render interaction fragment");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn favorites_response(state: &AppState, page: usize) -> Response {
    let actor = match actor(state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let result =
        match crate::interaction::favorites(&state.db, actor.uid, page, &state.frame.forum_filters)
            .await
        {
            Ok(result) => result,
            Err(error) => return failure(state, error),
        };
    let path = list_path("/my/favorites", page);
    let mut p = meta(
        state,
        crate::i18n::current().my_favorites_title().into(),
        crate::i18n::current().my_favorites_description().into(),
        &path,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let template = FavoritesTemplate {
        p,
        frame: crate::router::my::frame("favorites"),
        pages: pages(
            "/my/favorites",
            result.total,
            page,
            crate::interaction::PAGE_SIZE,
        ),
        rows: result
            .rows
            .into_iter()
            .map(|row| FavoriteRow {
                title: row.title,
                available: row.available,
                time: row.time,
                control: FavoriteControl {
                    tid: row.tid,
                    active: true,
                    count: None,
                    can_change: state
                        .viewer
                        .session
                        .as_ref()
                        .is_some_and(|s| s.member.can_contribute()),
                    location: path.clone(),
                    request_key: identity::random_token(),
                },
            })
            .collect(),
    };
    render(&template)
}

async fn favorites_page(
    Page(state): Page,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<ListQuery>,
) -> Response {
    if let Some(response) = read_gate(&state, "/my/favorites") {
        return response;
    }
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().my_error_page_number(),
        );
    }
    let path = list_path("/my/favorites", page);
    if uri.to_string() != path {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(path))],
        )
            .into_response();
    }
    favorites_response(&state, page).await
}

async fn favorite(
    Page(state): Page,
    AxumPath(target): AxumPath<String>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let Some(tid) = target
        .strip_prefix("thread-")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|id| *id > 0)
    else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    if let Some(response) = toggle_limited(&state, actor.uid).await {
        return response;
    }
    if let Err(error) = crate::interaction::Interaction::new(state.db.clone())
        .set_favorite(actor, tid, form.desired, &form.request_key)
        .await
    {
        return failure(&state, error);
    }
    let in_collection = form.location.split('?').next() == Some("/my/favorites");
    if in_collection {
        let requested = form
            .location
            .split_once('?')
            .and_then(|(_, query)| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "page")
                    .and_then(|(_, value)| value.parse::<usize>().ok())
            })
            .unwrap_or(1)
            .max(1);
        let count: i64 =
            match crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=?")
                .bind(actor.uid)
                .fetch_one(state.db.executor())
                .await
            {
                Ok(value) => value,
                Err(error) => return database_error(&state, error.into()),
            };
        let location = list_path(
            "/my/favorites",
            requested.min(
                (count as usize)
                    .div_ceil(crate::interaction::PAGE_SIZE)
                    .max(1),
            ),
        );
        return if posting::hx(&headers) {
            ([("HX-Redirect", crate::site::href(location))], "").into_response()
        } else {
            auth::redirect(&location)
        };
    }
    let thread = match content::thread_for_viewer(&state.db, tid as u64, state.readable()).await {
        Ok(Some(thread)) => thread,
        Ok(None) => {
            return if posting::hx(&headers) {
                ([("HX-Redirect", crate::site::href("/my/favorites"))], "").into_response()
            } else {
                auth::redirect("/my/favorites")
            };
        }
        Err(error) => return database_error(&state, error),
    };
    let active = match crate::interaction::favorite_state(&state.db, actor.uid, tid).await {
        Ok(value) => value,
        Err(error) => return database_error(&state, error),
    };
    let candidate = auth::safe_return(&form.location);
    let route = PageRoute::parse(
        candidate
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_start_matches('/'),
        &[],
    );
    let location = if matches!(route, Some(PageRoute::Thread { id, .. }) if id == thread.id) {
        candidate
    } else {
        format!("/thread-{}", thread.id)
    };
    if !posting::hx(&headers) {
        return auth::redirect(&location);
    }
    // The favorite count reads the counter column of the thread details just read.
    let count = thread.favorites;
    render(&FavoriteTemplate {
        p: meta(
            &state,
            crate::i18n::current().home_card_favorites().into(),
            "".into(),
            &location,
            "",
            "",
        ),
        favorite: FavoriteControl {
            tid,
            active,
            count: Some(count),
            can_change: state
                .viewer
                .session
                .as_ref()
                .is_some_and(|s| s.member.can_contribute())
                && (active || !thread.withdrawn),
            location,
            request_key: identity::random_token(),
        },
    })
}

async fn reaction(
    Page(state): Page,
    AxumPath(pid): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if let Some(response) = toggle_limited(&state, actor.uid).await {
        return response;
    }
    if let Err(error) = crate::interaction::Interaction::new(state.db.clone())
        .set_reaction(actor, pid, form.desired, &form.request_key)
        .await
    {
        return failure(&state, error);
    }
    let tid: Option<i64> =
        match crate::db::query_scalar("SELECT tid FROM gonof_forum_post WHERE pid=?")
            .bind(pid)
            .fetch_optional(state.db.executor())
            .await
        {
            Ok(value) => value,
            Err(error) => return database_error(&state, error.into()),
        };
    let thread = match tid {
        Some(tid) => {
            match content::thread_for_viewer(&state.db, tid as u64, state.readable()).await {
                Ok(value) => value,
                Err(error) => return database_error(&state, error),
            }
        }
        None => None,
    };
    let Some(mut thread) = thread else {
        return if posting::hx(&headers) {
            ([("HX-Redirect", crate::site::href("/"))], "").into_response()
        } else {
            auth::redirect("/")
        };
    };
    let flat = form.location.split('?').nth(1).is_some_and(|q| {
        url::form_urlencoded::parse(q.split('#').next().unwrap_or("").as_bytes())
            .any(|(k, v)| k == "view" && v == "flat")
    });
    let location = match content::post_location_for_viewer(
        &state.db,
        thread.id,
        pid,
        flat,
        state.readable(),
        crate::urls::Paging::from_settings(&state.frame.settings),
    )
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().my_error_reply_unavailable(),
            );
        }
        Err(error) => return database_error(&state, error),
    };
    // A no-JS form only needs to redirect back to the floor URL; the full reply below is only for HTMX partial refresh.
    if !posting::hx(&headers) {
        return auth::redirect(&location);
    }
    let page = PageRoute::parse(
        form.location
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_start_matches('/'),
        &[],
    )
    .filter(|r| matches!(r,PageRoute::Thread{id,..} if *id==thread.id))
    .map(|r| r.page())
    .unwrap_or(1);
    let params = posting::serde_urlencoded_query(&form.location);
    let sub_root = params.sub_root;
    let sub_page = params.sub_page.unwrap_or(1).max(1);
    if let Err(error) = content::load_replies(
        &state.db,
        &mut thread,
        page,
        flat,
        sub_root,
        sub_page,
        crate::urls::Paging::from_settings(&state.frame.settings),
    )
    .await
    {
        return database_error(&state, error);
    }
    let (authorid, available) = if thread.first_pid == pid {
        (thread.authorid, !thread.withdrawn)
    } else if let Some(post) = thread.posts.iter().find(|post| post.pid as i64 == pid) {
        (post.authorid, !post.hidden && !post.withdrawn)
    } else {
        (None, false)
    };
    let values = match crate::interaction::reactions(&state.db, Some(actor.uid), &[pid]).await {
        Ok(values) => values,
        Err(error) => return database_error(&state, error),
    };
    let highlights = match super::highlights::load(&state, &thread, flat, true).await {
        Ok(section) => section,
        Err(error) => return database_error(&state, error),
    };
    let downvote = if pid == thread.first_pid {
        match opening_downvote(&state, &thread, actor.uid, location.clone()).await {
            Ok(mut control) => {
                control.oob = true;
                Some(control)
            }
            Err(error) => return database_error(&state, error),
        }
    } else {
        None
    };
    render(&ReactionTemplate {
        downvote,
        p: meta(
            &state,
            crate::i18n::current().my_like_title().into(),
            "".into(),
            &location,
            "",
            "",
        ),
        reaction: reaction_control(
            &state,
            pid,
            authorid,
            available,
            location.clone(),
            values.get(&pid),
        ),
        highlights,
    })
}

/// The first post's downvote control, for partial refresh (the full page is assembled by the thread page itself).
async fn opening_downvote(
    state: &AppState,
    thread: &crate::content::Thread,
    uid: i64,
    location: String,
) -> anyhow::Result<DownvoteControl> {
    let value =
        crate::interaction::downvote_state(&state.db, Some(uid), thread.first_pid, thread.authorid)
            .await?;
    let blocked = match thread.authorid {
        Some(author) => crate::interaction::is_blocked_by(&state.db, author, uid).await?,
        None => false,
    };
    Ok(downvote_control(
        state,
        thread.first_pid,
        thread.authorid,
        !thread.withdrawn && !thread.erased && !thread.blocked,
        blocked,
        location,
        value,
    ))
}

/// Downvotes and likes share the toggle counts. Without JavaScript, 303 back to the first post; HTMX replaces the
/// downvote control and also replaces the first post's like control, since a downvote removes the member's own like.
async fn downvote(
    Page(state): Page,
    AxumPath(pid): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if let Some(response) = toggle_limited(&state, actor.uid).await {
        return response;
    }
    if let Err(error) = crate::interaction::Interaction::new(state.db.clone())
        .set_downvote(actor, pid, form.desired, &form.request_key)
        .await
    {
        return failure(&state, error);
    }
    let tid: Option<i64> =
        match crate::db::query_scalar("SELECT tid FROM gonof_forum_post WHERE pid=? AND first=1")
            .bind(pid)
            .fetch_optional(state.db.executor())
            .await
        {
            Ok(value) => value,
            Err(error) => return database_error(&state, error.into()),
        };
    let thread = match tid {
        Some(tid) => {
            match content::thread_for_viewer(&state.db, tid as u64, state.readable()).await {
                Ok(value) => value,
                Err(error) => return database_error(&state, error),
            }
        }
        None => None,
    };
    let Some(thread) = thread else {
        return if posting::hx(&headers) {
            ([("HX-Redirect", crate::site::href("/"))], "").into_response()
        } else {
            auth::redirect("/")
        };
    };
    let flat = form.location.split('?').nth(1).is_some_and(|q| {
        url::form_urlencoded::parse(q.split('#').next().unwrap_or("").as_bytes())
            .any(|(k, v)| k == "view" && v == "flat")
    });
    let location = format!("{}#floor-1", urls::thread(thread.id, 1, flat));
    if !posting::hx(&headers) {
        return auth::redirect(&location);
    }
    let control = match opening_downvote(&state, &thread, actor.uid, location.clone()).await {
        Ok(control) => control,
        Err(error) => return database_error(&state, error),
    };
    let values = match crate::interaction::reactions(&state.db, Some(actor.uid), &[pid]).await {
        Ok(values) => values,
        Err(error) => return database_error(&state, error),
    };
    let mut reaction = reaction_control(
        &state,
        pid,
        thread.authorid,
        !thread.withdrawn,
        location.clone(),
        values.get(&pid),
    );
    reaction.oob = true;
    render(&DownvoteTemplate {
        p: meta(
            &state,
            crate::i18n::current().my_downvote_title().into(),
            "".into(),
            &location,
            "",
            "",
        ),
        downvote: control,
        reaction,
    })
}

async fn notifications_response(
    state: &AppState,
    page: usize,
    htmx: bool,
    category: &str,
) -> Response {
    let actor = match actor(state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let list = match notifications::list_in_category(
        &state.db,
        actor,
        page,
        notifications::PAGE_SIZE,
        category,
        &state.frame.forum_filters,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return failure(state, error),
    };
    // Pagination links must keep the current category, or paging falls back to all.
    let base = category_base(category);
    let mut p = meta(
        state,
        crate::i18n::current().frame_notifications().into(),
        crate::i18n::current().my_notifications_description().into(),
        &list_path("/my/notifications", page),
        "",
        "",
    );
    p.unread_notifications = list.unread;
    // HTMX only takes the collection block, which does not include the right column, so neither the member summary nor
    // the two thread cards need querying.
    if !htmx && let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let template = NotificationsTemplate {
        p,
        frame: crate::router::my::frame_with_filter("notifications", category),
        page,
        max_id: list.max_id,
        category: category.to_owned(),
        fragment: htmx,
        request_key: identity::random_token(),
        can_change: state
            .viewer
            .session
            .as_ref()
            .is_some_and(|s| s.member.can_contribute()),
        pages: pages(&base, list.total as usize, page, notifications::PAGE_SIZE),
        rows: list
            .items
            .into_iter()
            .map(|item| NotificationRow {
                action: notifications::record::Kind::parse(&item.kind).map_or(
                    crate::i18n::current().my_notice_reply(),
                    notifications::record::Kind::action,
                ),
                time: crate::clock::datetime(item.dateline, &crate::clock::current()),
                item,
            })
            .collect(),
    };
    if htmx {
        let mut response = fragment(template.as_collection().render());
        // Only the list is replaced: the category tabs and the right column stay, and the header link and the card's
        // unread dot follow out of band.
        response.headers_mut().insert(
            "HX-Retarget",
            HeaderValue::from_static("#notification-list"),
        );
        response
            .headers_mut()
            .insert("HX-Reswap", HeaderValue::from_static("outerHTML"));
        response
    } else {
        render(&template)
    }
}

async fn notifications_page(
    Page(state): Page,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<ListQuery>,
) -> Response {
    if let Some(response) = read_gate(&state, "/my/notifications") {
        return response;
    }
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().my_error_page_number(),
        );
    }
    // Unknown categories are treated as "All" and normalized to the URL without that parameter, so a misspelled
    // query string does not stay on the page.
    let category = known_category(&query.category);
    let path = list_path(&category_base(category), page);
    if uri.to_string() != path {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(path))],
        )
            .into_response();
    }
    notifications_response(&state, page, false, category).await
}

async fn read(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<ReadForm>,
) -> Response {
    change_read(state, headers, form, Some(id)).await
}

async fn read_all(
    Page(state): Page,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<ReadForm>,
) -> Response {
    change_read(state, headers, form, None).await
}

async fn change_read(
    state: AppState,
    headers: HeaderMap,
    form: ReadForm,
    id: Option<i64>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if !valid_key(&form.request_key) {
        return error_page(
            &state,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().my_error_request_key(),
        );
    }
    if let Some(response) = read_limited(&state, actor.uid).await {
        return response;
    }
    let category = known_category(&form.category);
    let result = if let Some(id) = id {
        notifications::mark_read(&state.db, actor, id, identity::now()).await
    } else {
        notifications::mark_all_read(&state.db, actor, form.max_id, category, identity::now()).await
    };
    if let Err(error) = result {
        return failure(&state, error);
    }
    let page = form.page.max(1);
    if posting::hx(&headers) {
        notifications_response(&state, page, true, category).await
    } else {
        auth::redirect(&list_path(&category_base(category), page))
    }
}

async fn open(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<ReadForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if !valid_key(&form.request_key) {
        return error_page(
            &state,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().my_error_request_key(),
        );
    }
    if let Some(response) = read_limited(&state, actor.uid).await {
        return response;
    }
    let location = match notifications::open_url_in_view(
        &state.db,
        actor,
        id,
        identity::now(),
        (
            state.frame.reading.flat,
            crate::urls::Paging::from_settings(&state.frame.settings),
        ),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return failure(&state, error),
    };
    if posting::hx(&headers) {
        ([("HX-Redirect", crate::site::href(location))], "").into_response()
    } else {
        auth::redirect(&location)
    }
}

fn valid_key(value: &str) -> bool {
    identity::valid_request_key(value)
}
