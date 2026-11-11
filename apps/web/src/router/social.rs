//! Private subscriptions, blocks and notification settings, with native form fallbacks.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{
    identity,
    interaction::social::{self, Relation},
    notifications::preferences::{self, Kind, Preferences},
};
use axum::{
    extract::{DefaultBodyLimit, Path},
    http::HeaderMap,
    routing::post,
};
use std::collections::HashMap;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/following/threads", get(threads_page))
        .route("/my/following/forums", get(forums_page))
        .route("/my/blocked-users", get(blocks_page))
        .route("/my/following/members", get(members_page))
        .route("/my/followers", get(followers_page))
        .route("/my/following/feed", get(feed_page))
        .route(
            "/my/preferences",
            get(preferences_page).post(save_preferences),
        )
        .route("/api/{target}/follow", post(follow_thread))
        .route("/api/forums/{id}/follow", post(follow_forum))
        .route("/api/users/{id}/block", post(block_member))
        .route("/api/users/{id}/follow", post(follow_member))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

pub(super) struct Control {
    pub target: i64,
    pub action: String,
    pub active: bool,
    pub can_change: bool,
    pub location: String,
    pub request_key: String,
    pub label: &'static str,
    /// Explanation given when it cannot be changed; nothing is shown for an empty string.
    pub notice: &'static str,
}
pub(super) async fn control(
    state: &AppState,
    relation: Relation,
    target: i64,
    location: String,
) -> anyhow::Result<Control> {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let active = if let Some(member) = member {
        social::following(&state.db, member.uid, target, relation).await?
    } else {
        false
    };
    control_known(state, relation, target, location, active).await
}

/// Same as [`control`], with the follow status already read (the thread page takes it from the reader-state row,
/// docs/queries.md, "Thread page").
pub(super) async fn control_known(
    state: &AppState,
    relation: Relation,
    target: i64,
    location: String,
    active: bool,
) -> anyhow::Result<Control> {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let mut notice = "";
    let mut can_change = member.is_some_and(|member| match relation {
        Relation::Block => member.uid != target,
        Relation::Member => member.uid != target && member.can_contribute(),
        _ => member.can_contribute(),
    });
    // Following members: when either side has blocked the other, a new follow is not allowed, and the message does not
    // say which.
    if relation == Relation::Member
        && can_change
        && !active
        && let Some(member) = member
        && (crate::interaction::is_blocked_by(&state.db, target, member.uid).await?
            || crate::interaction::is_blocked_by(&state.db, member.uid, target).await?)
    {
        can_change = false;
        notice = crate::i18n::current().social_follow_unavailable();
    }
    let action = action(relation, target);
    let label = match relation {
        Relation::Block => {
            if active {
                crate::i18n::current().social_unblock()
            } else {
                crate::i18n::current().social_block()
            }
        }
        _ => {
            if active {
                crate::i18n::current().social_unfollow()
            } else {
                crate::i18n::current().social_follow()
            }
        }
    };
    Ok(Control {
        target,
        action,
        active,
        can_change,
        location,
        request_key: identity::random_token(),
        label,
        notice,
    })
}
fn action(relation: Relation, target: i64) -> String {
    match relation {
        Relation::Thread => format!("/api/thread-{target}/follow"),
        Relation::Forum => format!("/api/forums/{target}/follow"),
        Relation::Block => format!("/api/users/{target}/block"),
        Relation::Member => format!("/api/users/{target}/follow"),
    }
}
fn base(relation: Relation) -> &'static str {
    match relation {
        Relation::Thread => "/my/following/threads",
        Relation::Forum => "/my/following/forums",
        Relation::Block => "/my/blocked-users",
        Relation::Member => "/my/following/members",
    }
}
/// Page id in the directory, used to pick the second-level tabs.
fn page_id(relation: Relation) -> &'static str {
    match relation {
        Relation::Thread => "following-threads",
        Relation::Forum => "following-forums",
        Relation::Block => "blocked-users",
        Relation::Member => "following-members",
    }
}
fn title(relation: Relation) -> &'static str {
    match relation {
        Relation::Thread => crate::i18n::current().social_title_threads(),
        Relation::Forum => crate::i18n::current().social_title_forums(),
        Relation::Block => crate::i18n::current().social_title_blocked(),
        Relation::Member => crate::i18n::current().social_title_members(),
    }
}
fn location(relation: Relation, page: usize) -> String {
    if page == 1 {
        base(relation).into()
    } else {
        format!("{}?page={page}", base(relation))
    }
}
fn failure(state: &AppState, error: crate::content::writes::ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}
fn login(path: &str) -> Response {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_to", path)
        .finish();
    auth::redirect(&format!("/login?{query}"))
}
#[derive(Default, Deserialize)]
struct ListQuery {
    page: Option<usize>,
}
struct CollectionRow {
    item: social::RelationRow,
    control: Control,
}
#[derive(Template)]
#[template(path = "social/collection.html")]
struct CollectionTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<CollectionRow>,
    total: i64,
    page: usize,
    previous: String,
    next: String,
    kind: &'static str,
}
async fn collection(
    state: AppState,
    query: ListQuery,
    uri: OriginalUri,
    relation: Relation,
) -> Response {
    let page = query.page.unwrap_or(1);
    let path = location(relation, page);
    let Some(session) = &state.viewer.session else {
        return login(&path);
    };
    if page == 0 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().my_error_page_number(),
        );
    }
    if uri.0.to_string() != path {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(path))],
        )
            .into_response();
    }
    let collection = match social::list(
        &state.db,
        session.member.actor(),
        relation,
        page,
        &state.frame.forum_filters,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return failure(&state, error),
    };
    let mut rows = Vec::new();
    for item in collection.rows {
        let control = Control {
            target: item.target,
            action: action(relation, item.target),
            active: true,
            can_change: relation == Relation::Block || session.member.can_contribute(),
            location: path.clone(),
            request_key: identity::random_token(),
            label: if relation == Relation::Block {
                crate::i18n::current().social_unblock()
            } else {
                crate::i18n::current().social_unfollow()
            },
            notice: "",
        };
        rows.push(CollectionRow { item, control });
    }
    let mut p = meta(
        &state,
        title(relation).into(),
        title(relation).into(),
        &path,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&CollectionTemplate {
        p,
        frame: crate::router::my::frame(page_id(relation)),
        rows,
        total: collection.total,
        page,
        previous: if page > 1 {
            location(relation, page - 1)
        } else {
            String::new()
        },
        next: if page * crate::interaction::PAGE_SIZE < collection.total as usize {
            location(relation, page + 1)
        } else {
            String::new()
        },
        kind: relation.name(),
    })
}
async fn threads_page(
    Page(state): Page,
    Query(query): Query<ListQuery>,
    uri: OriginalUri,
) -> Response {
    collection(state, query, uri, Relation::Thread).await
}
async fn forums_page(
    Page(state): Page,
    Query(query): Query<ListQuery>,
    uri: OriginalUri,
) -> Response {
    collection(state, query, uri, Relation::Forum).await
}
async fn blocks_page(
    Page(state): Page,
    Query(query): Query<ListQuery>,
    uri: OriginalUri,
) -> Response {
    collection(state, query, uri, Relation::Block).await
}
async fn members_page(
    Page(state): Page,
    Query(query): Query<ListQuery>,
    uri: OriginalUri,
) -> Response {
    collection(state, query, uri, Relation::Member).await
}

#[derive(Template)]
#[template(path = "social/followers.html")]
struct FollowersTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<social::RelationRow>,
    total: i64,
    page: usize,
    previous: String,
    next: String,
}
/// Follower list: visible only to the member themselves, with no action buttons; to stop being followed by
/// someone, block them.
async fn followers_page(
    Page(state): Page,
    Query(query): Query<ListQuery>,
    uri: OriginalUri,
) -> Response {
    let page = query.page.unwrap_or(1);
    let path = if page == 1 {
        "/my/followers".to_owned()
    } else {
        format!("/my/followers?page={page}")
    };
    let Some(session) = &state.viewer.session else {
        return login(&path);
    };
    if page == 0 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().my_error_page_number(),
        );
    }
    if uri.0.to_string() != path {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(path))],
        )
            .into_response();
    }
    let list = match social::followers(&state.db, session.member.actor(), page).await {
        Ok(value) => value,
        Err(error) => return failure(&state, error),
    };
    let mut p = meta(
        &state,
        crate::i18n::current().profile_followers().into(),
        crate::i18n::current().social_followers_description().into(),
        &path,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&FollowersTemplate {
        p,
        frame: crate::router::my::frame("followers"),
        rows: list.rows,
        total: list.total,
        page,
        previous: match page {
            1 => String::new(),
            2 => "/my/followers".into(),
            _ => format!("/my/followers?page={}", page - 1),
        },
        next: if page * crate::interaction::PAGE_SIZE < list.total as usize {
            format!("/my/followers?page={}", page + 1)
        } else {
            String::new()
        },
    })
}

#[derive(Default, Deserialize)]
struct FeedQuery {
    #[serde(default)]
    before: String,
}
struct FeedRow {
    item: social::FeedItem,
    time: String,
    category: &'static str,
}
#[derive(Template)]
#[template(path = "social/feed.html")]
struct FeedTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<FeedRow>,
    days: i64,
    first: bool,
    next: String,
}
/// The cursor is written as `publish time-first post PID`; an unreadable cursor is treated as 400 and does not fall
/// back to the first page.
fn feed_cursor(value: &str) -> Option<(i64, i64)> {
    let (dateline, pid) = value.split_once('-')?;
    let dateline = dateline.parse::<i64>().ok().filter(|v| *v >= 0)?;
    let pid = pid.parse::<i64>().ok().filter(|v| *v > 0)?;
    Some((dateline, pid))
}
/// Following feed: no on-site notifications; new threads of followed members are seen here, with no unread badge.
async fn feed_page(Page(state): Page, Query(query): Query<FeedQuery>) -> Response {
    let path = if query.before.is_empty() {
        "/my/following/feed".to_owned()
    } else {
        format!("/my/following/feed?before={}", query.before)
    };
    let Some(session) = &state.viewer.session else {
        return login(&path);
    };
    let before = if query.before.is_empty() {
        None
    } else {
        match feed_cursor(&query.before) {
            Some(cursor) => Some(cursor),
            None => {
                return error_page(
                    &state,
                    StatusCode::BAD_REQUEST,
                    crate::i18n::current().social_error_cursor(),
                );
            }
        }
    };
    let feed = match social::feed(
        &state.db,
        session.member.actor(),
        before,
        identity::now(),
        &state.frame.forum_filters,
        &state.frame.all_forums,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return failure(&state, error),
    };
    let mut p = meta(
        &state,
        crate::i18n::current().my_page_following_feed().into(),
        crate::i18n::current().social_feed_description().into(),
        &path,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    let clock = crate::clock::current();
    render(&FeedTemplate {
        p,
        frame: crate::router::my::frame("following-feed"),
        rows: feed
            .rows
            .into_iter()
            .map(|item| FeedRow {
                time: crate::clock::datetime(item.dateline, &clock),
                category: crate::content::category_label(item.category),
                item,
            })
            .collect(),
        days: social::FEED_DAYS,
        first: before.is_none(),
        next: feed
            .next
            .map(|(dateline, pid)| format!("/my/following/feed?before={dateline}-{pid}"))
            .unwrap_or_default(),
    })
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
async fn change(
    state: AppState,
    id: i64,
    headers: HeaderMap,
    form: StateForm,
    relation: Relation,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login(base(relation));
    };
    // The count is the admin setting `settings.limit.social_per_5min` (a 5-minute window, constant), shared by
    // following, blocking and forum filters.
    match state
        .limit(&[Rule::new(
            "social-write",
            session.member.uid,
            u32::try_from(state.frame.settings.limit.social_per_5min).unwrap_or(0),
            300,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error_page(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                crate::i18n::current().my_error_too_many_actions(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    if let Err(error) = social::set(
        &state.db,
        session.member.actor(),
        id,
        form.desired,
        &form.request_key,
        relation,
    )
    .await
    {
        return failure(&state, error);
    }
    let mut destination = auth::safe_return(&form.location);
    if destination == "/" && form.location.is_empty() {
        destination = base(relation).into();
    }
    if destination.split('?').next() == Some(base(relation)) {
        let requested = destination
            .split_once('?')
            .and_then(|(_, q)| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(key, _)| key == "page")
                    .and_then(|(_, v)| v.parse::<usize>().ok())
            })
            .unwrap_or(1)
            .max(1);
        let first = match social::list(
            &state.db,
            session.member.actor(),
            relation,
            1,
            &state.frame.forum_filters,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => return failure(&state, error),
        };
        destination = location(
            relation,
            requested.min(
                (first.total as usize)
                    .div_ceil(crate::interaction::PAGE_SIZE)
                    .max(1),
            ),
        );
    }
    if posting::hx(&headers) {
        ([("HX-Redirect", crate::site::href(destination))], "").into_response()
    } else {
        auth::redirect(&destination)
    }
}
async fn follow_thread(
    Page(state): Page,
    Path(target): Path<String>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    let Some(id) = target
        .strip_prefix("thread-")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
    else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    change(state, id, headers, form, Relation::Thread).await
}
async fn follow_forum(
    Page(state): Page,
    Path(id): Path<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    change(state, id, headers, form, Relation::Forum).await
}
async fn block_member(
    Page(state): Page,
    Path(id): Path<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    change(state, id, headers, form, Relation::Block).await
}
async fn follow_member(
    Page(state): Page,
    Path(id): Path<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<StateForm>,
) -> Response {
    // Following members shares the toggle count with favorites and likes.
    if let Some(session) = &state.viewer.session
        && let Some(response) = super::interaction::toggle_limited(&state, session.member.uid).await
    {
        return response;
    }
    change(state, id, headers, form, Relation::Member).await
}

struct PreferenceRow {
    field: &'static str,
    label: &'static str,
    site: bool,
    email: bool,
}
#[derive(Template)]
#[template(path = "social/preferences.html")]
struct PreferencesTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<PreferenceRow>,
    email_message: bool,
    email_login: bool,
    version: i64,
    message: String,
    success: bool,
    email_verified: bool,
}
async fn preferences_response(
    state: &AppState,
    draft: Option<Preferences>,
    message: String,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login("/my/preferences");
    };
    let settings = if let Some(draft) = draft {
        draft
    } else {
        match preferences::get(&state.db, session.member.actor()).await {
            Ok(value) => value,
            Err(error) => return failure(state, error),
        }
    };
    let rows = Kind::ALL
        .into_iter()
        .enumerate()
        .map(|(index, kind)| PreferenceRow {
            field: kind.field(),
            label: kind.label(),
            site: settings.site[index],
            email: settings.email[index],
        })
        .collect();
    let mut p = meta(
        state,
        crate::i18n::current().my_page_preferences().into(),
        crate::i18n::current()
            .social_preferences_description()
            .into(),
        "/my/preferences",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&PreferencesTemplate {
        frame: crate::router::my::frame("preferences"),
        p,
        rows,
        email_message: settings.email_message,
        email_login: settings.email_login,
        version: settings.version,
        message,
        success: status.is_success(),
        email_verified: session.member.email_verified,
    });
    *response.status_mut() = status;
    response
}
#[derive(Default, Deserialize)]
struct PreferenceQuery {
    #[serde(default)]
    saved: bool,
}
async fn preferences_page(Page(state): Page, Query(query): Query<PreferenceQuery>) -> Response {
    preferences_response(
        &state,
        None,
        if query.saved {
            crate::i18n::current().social_preferences_saved().into()
        } else {
            String::new()
        },
        StatusCode::OK,
    )
    .await
}
async fn save_preferences(
    Page(state): Page,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login("/my/preferences");
    };
    match state
        .limit(&[Rule::new(
            "notification-settings",
            session.member.uid,
            30,
            300,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error_page(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                crate::i18n::current().social_error_save_too_many(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    let Some(version) = form.get("version").and_then(|v| v.parse::<i64>().ok()) else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().social_error_preferences_version(),
        );
    };
    // Whole-page submission: unchecked checkboxes do not appear in the form, which means off.
    let mut draft = Preferences {
        site: [false; 5],
        email: [false; 5],
        email_message: form.contains_key("email_message"),
        email_login: form.contains_key("email_login"),
        version,
    };
    if form.iter().any(|(key, value)| {
        (key.starts_with("site_") || key.starts_with("email_")) && value != "true"
    }) {
        return preferences_response(
            &state,
            Some(draft),
            crate::i18n::current()
                .social_error_preferences_format()
                .into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    for (index, kind) in Kind::ALL.into_iter().enumerate() {
        for (channel, values) in [("site", &mut draft.site), ("email", &mut draft.email)] {
            if form.contains_key(&format!("{channel}_{}", kind.field())) {
                values[index] = true;
            }
        }
    }
    match preferences::save(&state.db, session.member.actor(), &draft).await {
        Ok(()) => auth::redirect("/my/preferences?saved=true"),
        Err(error) => {
            let (status, message) = posting::write_error(error);
            preferences_response(&state, Some(draft), message, status).await
        }
    }
}
