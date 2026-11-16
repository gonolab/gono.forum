//! User search page `/search/users`: alongside thread search `/search`, lists members by username prefix.
//! Matching, escaping and visibility scope: see [`crate::content::members::search`].
use super::extract::Page;
use super::*;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/search/users", get(member_search_page))
}

#[derive(Default, Deserialize)]
struct MemberSearchQuery {
    #[serde(default)]
    q: String,
    page: Option<usize>,
}

/// The "Threads / Members" scope tabs shared by the two search pages.
pub(super) struct SearchScope {
    pub(super) threads_href: String,
    pub(super) members_href: String,
    pub(super) members: bool,
}

impl SearchScope {
    pub(super) fn new(q: &str, members: bool) -> Self {
        Self {
            threads_href: urls::search(q, Sort::Latest, 1),
            members_href: urls::member_search(q, 1),
            members,
        }
    }
}

struct MemberRow {
    username: String,
    uid: i64,
    avatar: AvatarView,
    joined: String,
    /// The sign-up time as a timestamp, for the `datetime` of the shown date.
    joined_at: i64,
    bio: String,
}

#[derive(Template)]
#[template(path = "search/users.html")]
struct MemberSearchTemplate<'a> {
    p: PageMeta<'a>,
    q: String,
    scope: SearchScope,
    rows: Vec<MemberRow>,
    previous: String,
    next: String,
}

async fn member_search_page(
    Page(state): Page,
    query: Result<Query<MemberSearchQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().search_error_query(),
        );
    };
    let q = query.q.trim();
    if q.len() > 300 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().home_error_search_too_long(),
        );
    }
    let page = query.page.unwrap_or(1);
    if page == 0 || page > content::members::SEARCH_MAX_PAGE {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().home_error_search_page(),
        );
    }
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    let found = match content::members::search(&state.db, q, page, viewer_uid).await {
        Ok(found) => found,
        Err(error) => return database_error(&state, error),
    };
    // Same as the other lists: beyond the last page is a 404, and page 1 of an empty set shows the empty state.
    if page > 1 && found.members.is_empty() {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_no_such_page(),
        );
    }
    let clock = crate::clock::current();
    let rows = found
        .members
        .into_iter()
        .map(|member| MemberRow {
            avatar: avatar_view(
                Some(member.uid),
                member.avatar_aid.unwrap_or(0),
                member.avatar,
                &member.username,
            ),
            joined: crate::clock::date(member.regdate, &clock),
            joined_at: member.regdate,
            uid: member.uid,
            username: member.username,
            bio: member.bio,
        })
        .collect();
    let title = if q.is_empty() {
        crate::i18n::current().search_members().to_owned()
    } else if page > 1 {
        crate::i18n::current()
            .common_paged_title(crate::i18n::current().search_members_heading(q), page)
    } else {
        crate::i18n::current().search_members_heading(q)
    };
    let mut p = meta(
        &state,
        title,
        crate::i18n::current().search_members_description().into(),
        &urls::member_search(q, page),
        "",
        q,
    );
    if let Some(error) = fill_sidebar(&state, &mut p).await {
        return error;
    }
    render(&MemberSearchTemplate {
        p,
        q: q.to_owned(),
        scope: SearchScope::new(q, true),
        rows,
        previous: if page > 1 {
            urls::member_search(q, page - 1)
        } else {
            String::new()
        },
        next: if found.has_next {
            urls::member_search(q, page + 1)
        } else {
            String::new()
        },
    })
}

/// The right column is the same as on the thread search page: member card, popular and latest threads.
pub(super) async fn fill_sidebar(state: &AppState, p: &mut PageMeta<'_>) -> Option<Response> {
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    let forums = state
        .frame
        .listable_forums(&state.frame.forum_filters.timeline_excluded());
    let blocked = match viewer_uid {
        Some(uid) => match content::blocked_authors(&state.db, uid).await {
            Ok(blocked) => blocked,
            Err(error) => return Some(database_error(state, error)),
        },
        None => Vec::new(),
    };
    let mut overview = match content::layered_sidebar_for_viewer(
        &state.db,
        &state.global,
        &state.timed_cache(),
        content::ListViewer {
            uid: viewer_uid,
            forums: &forums,
            blocked_uids: &blocked,
        },
        content::SidebarParams::from_settings(&state.frame.settings),
        &content::ListScope::All,
        &state.frame.statistics_fids(),
    )
    .await
    {
        Ok(overview) => overview,
        Err(error) => return Some(database_error(state, error)),
    };
    p.sidebar = Some(SidebarTopics {
        hot: std::mem::take(&mut overview.hot),
        latest: std::mem::take(&mut overview.latest),
    });
    if let Some(session) = &state.viewer.session {
        match state.member_summary(session.member.actor()).await {
            Ok(summary) => p.member_summary = Some(summary),
            Err(error) => return Some(database_error(state, error)),
        }
    }
    None
}
