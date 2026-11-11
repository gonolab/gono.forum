//! Pages for hiding and blocking forums: the buttons at the top of forum pages, the block notice page, and the
//! `/my/forum-filters` management list.
//! The data layer is `interaction::forum_filter`.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{
    identity,
    interaction::forum_filter::{self, FilterMode},
    organization::ForumRecord,
};
use axum::{
    extract::{DefaultBodyLimit, Path},
    http::HeaderMap,
    routing::post,
};

const LIST: &str = "/my/forum-filters";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(LIST, get(filters_page))
        .route("/api/forums/{id}/filter", post(change))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

/// The button for one setting level; each button is an independent native form with its own request key.
pub(super) struct Button {
    pub mode: &'static str,
    pub label: &'static str,
    pub request_key: String,
}

fn button(mode: &'static str, label: &'static str) -> Button {
    Button {
        mode,
        label,
        request_key: identity::random_token(),
    }
}

pub(super) struct Control {
    pub action: String,
    pub location: String,
    pub note: String,
    pub buttons: Vec<Button>,
}

/// The available actions for the level the member set directly.
fn buttons_for(direct: Option<FilterMode>) -> Vec<Button> {
    match direct {
        None => vec![
            button("hide", crate::i18n::current().social_filter_hide()),
            button("block", crate::i18n::current().social_filter_block()),
        ],
        Some(FilterMode::Hide) => vec![
            button("none", crate::i18n::current().social_filter_unhide()),
            button("block", crate::i18n::current().social_filter_block()),
        ],
        Some(FilterMode::Block) => vec![
            button("hide", crate::i18n::current().social_filter_hide_instead()),
            button("none", crate::i18n::current().social_unblock()),
        ],
    }
}

fn action(fid: i64) -> String {
    format!("/api/forums/{fid}/filter")
}

fn forum_name(state: &AppState, fid: i64) -> String {
    state
        .frame
        .forums
        .iter()
        .find(|f| f.fid == fid)
        .map_or_else(
            || crate::i18n::current().social_filter_parent().into(),
            |f| f.name.clone(),
        )
}

/// The hide and block buttons next to "Follow" at the top of a forum page. Blocked forums go to [`forum_blocked_page`]
/// and never reach here.
pub(super) fn control(state: &AppState, forum: &ForumRecord, location: String) -> Control {
    let filters = &state.frame.forum_filters;
    let note = match (
        filters.inherited_from(forum.fid),
        filters.effective(forum.fid),
    ) {
        (Some(parent), Some(FilterMode::Hide)) => {
            crate::i18n::current().social_filter_inherited_hide(forum_name(state, parent))
        }
        (Some(parent), Some(FilterMode::Block)) => {
            crate::i18n::current().social_filter_inherited_block(forum_name(state, parent))
        }
        (None, Some(FilterMode::Hide)) => crate::i18n::current().social_filter_hidden_note().into(),
        _ => String::new(),
    };
    Control {
        action: action(forum.fid),
        location,
        note,
        buttons: buttons_for(filters.direct(forum.fid)),
    }
}

#[derive(Template)]
#[template(path = "social/forum_blocked.html")]
struct BlockedTemplate<'a> {
    p: PageMeta<'a>,
    message: String,
    filter: Control,
}

/// A blocked forum (including one blocked along with its parent forum) itself, its threads and its posting entry all
/// only show this notice and the unblock entry.
/// Only the member can see this page, so it uses 200 rather than 404: the forum has not disappeared.
pub(super) fn forum_blocked_page(
    state: &AppState,
    forum: &ForumRecord,
    location: &str,
) -> Response {
    let (target, message, label) = match state.frame.forum_filters.inherited_from(forum.fid) {
        Some(parent) => (
            parent,
            crate::i18n::current()
                .social_filter_blocked_parent(forum_name(state, parent), &forum.name),
            crate::i18n::current().social_filter_unblock_parent(),
        ),
        None => (
            forum.fid,
            crate::i18n::current().social_filter_blocked_forum(&forum.name),
            crate::i18n::current().social_unblock(),
        ),
    };
    let p = meta(
        state,
        crate::i18n::current().social_filter_blocked_title(&forum.name),
        state.frame.settings.site.description.clone(),
        location,
        "",
        "",
    );
    render(&BlockedTemplate {
        p,
        message,
        filter: Control {
            action: action(target),
            location: location.into(),
            note: String::new(),
            buttons: vec![button("none", label)],
        },
    })
}

struct Row {
    fid: i64,
    name: String,
    href: String,
    mode_label: &'static str,
    time: String,
    children: String,
    filter: Control,
}

#[derive(Template)]
#[template(path = "social/forum_filters.html")]
struct ListTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<Row>,
}

fn login(path: &str) -> Response {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_to", path)
        .finish();
    auth::redirect(&format!("/login?{query}"))
}

fn failure(state: &AppState, error: crate::content::writes::ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}

async fn filters_page(Page(state): Page) -> Response {
    let Some(session) = &state.viewer.session else {
        return login(LIST);
    };
    let filters = match forum_filter::list(&state.db, session.member.actor()).await {
        Ok(rows) => rows,
        Err(error) => return failure(&state, error),
    };
    let rows = filters
        .into_iter()
        .map(|row| {
            // Forums later disabled or made private are still listed, offering only unblocking. The setting is
            // per account and ignores the scope of multi-domain sites.
            let readable = state.frame.all_forums.iter().any(|f| f.fid == row.fid);
            let children = state
                .frame
                .all_forums
                .iter()
                .filter(|f| f.parent_fid == Some(row.fid))
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(crate::i18n::current().common_list_separator());
            Row {
                fid: row.fid,
                name: if readable {
                    row.name
                } else {
                    crate::i18n::current().social_filter_unavailable().into()
                },
                href: if readable {
                    format!("/{}", row.slug)
                } else {
                    String::new()
                },
                mode_label: row.mode.label(),
                time: crate::moderation::display_time(row.dateline),
                children,
                filter: Control {
                    action: action(row.fid),
                    location: LIST.into(),
                    note: String::new(),
                    buttons: if readable {
                        buttons_for(Some(row.mode))
                    } else {
                        vec![button(
                            "none",
                            crate::i18n::current().social_filter_remove(),
                        )]
                    },
                },
            }
        })
        .collect();
    let mut p = meta(
        &state,
        crate::i18n::current().my_page_forum_filters().into(),
        crate::i18n::current().social_filter_description().into(),
        LIST,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&ListTemplate {
        p,
        frame: crate::router::my::frame("forum-filters"),
        rows,
    })
}

#[derive(Deserialize)]
struct FilterForm {
    _csrf: String,
    request_key: String,
    mode: String,
    #[serde(default)]
    location: String,
}
csrf_forms!(FilterForm);

async fn change(
    Page(state): Page,
    Path(id): Path<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<FilterForm>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login(LIST);
    };
    let desired = match form.mode.as_str() {
        "none" => None,
        value => match FilterMode::parse(value) {
            Some(mode) => Some(mode),
            None => {
                return error_page(
                    &state,
                    StatusCode::BAD_REQUEST,
                    crate::i18n::current().social_error_filter_option(),
                );
            }
        },
    };
    // Shares the same limit as following and blocking members.
    // The count is the admin setting `settings.limit.social_per_5min` (window of 5 minutes, a constant), shared by
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
    if let Err(error) = forum_filter::set(
        &state.db,
        &state.cache,
        session.member.actor(),
        id,
        desired,
        &form.request_key,
    )
    .await
    {
        return failure(&state, error);
    }
    let destination = if form.location.is_empty() {
        LIST.into()
    } else {
        auth::safe_return(&form.location)
    };
    if posting::hx(&headers) {
        ([("HX-Redirect", crate::site::href(destination))], "").into_response()
    } else {
        auth::redirect(&destination)
    }
}
