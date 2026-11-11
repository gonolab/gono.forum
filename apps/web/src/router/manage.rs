use super::{
    AppState, PageMeta, auth, error_page,
    extract::{CsrfForm, Page, csrf_forms},
    meta, posting, render,
};
use crate::{
    content::edits::{ChangeRequest, ContentAction, ContentEditor},
    identity::{self, Actor, RoleAssignment},
    manage as governance,
};
use askama::Template;
use axum::{
    Router,
    extract::{Path, Query},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub(super) mod catalog;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/content", get(content_page))
        .route("/manage/content/{pid}", get(detail_page))
        .route("/manage/posts/{pid}/actions", post(change))
        .route("/manage/logs", get(logs_page))
        .route("/manage/moderators", get(moderators_page).post(assign))
}

#[allow(clippy::result_large_err)]
fn actor(state: &AppState) -> Result<Actor, Response> {
    state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.actor())
        .ok_or_else(|| auth::redirect("/login?return_to=/manage/content"))
}
fn failure(state: &AppState, error: identity::IdentityError) -> Response {
    match error {
        identity::IdentityError::Forbidden => error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().forum_content_error_forbidden(),
        ),
        identity::IdentityError::Unauthorized => error_page(
            state,
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().forum_content_error_session(),
        ),
        other => auth::identity_error(state, other),
    }
}

#[derive(Template)]
#[template(path = "manage/content.html")]
struct ContentTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::ManagedPost>,
    forums: Vec<governance::ManagedForum>,
    filter: governance::ContentQuery,
    total: i64,
    previous: String,
    next: String,
}
async fn content_page(
    Page(state): Page,
    Query(filter): Query<governance::ContentQuery>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let page = match governance::content(&state.db, actor, &filter).await {
        Ok(page) => page,
        Err(error) => return failure(&state, error),
    };
    let forums = match governance::forums(&state.db, actor).await {
        Ok(forums) => forums,
        Err(error) => return failure(&state, error),
    };
    let href = |page: i64| {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        if !filter.kind.is_empty() {
            params.append_pair("kind", &filter.kind);
        }
        if let Some(fid) = filter.fid {
            params.append_pair("fid", &fid.to_string());
        }
        if !filter.state.is_empty() {
            params.append_pair("state", &filter.state);
        }
        if !filter.q.is_empty() {
            params.append_pair("q", &filter.q);
        }
        params.append_pair("page", &page.to_string());
        format!("/manage/content?{}", params.finish())
    };
    let previous = if filter.page() > 1 {
        href(filter.page() - 1)
    } else {
        String::new()
    };
    let next = if filter.page() * governance::PAGE_SIZE < page.total {
        href(filter.page() + 1)
    } else {
        String::new()
    };
    render(&ContentTemplate {
        p: meta(
            &state,
            crate::i18n::current().forum_content_title().into(),
            crate::i18n::current().forum_content_description().into(),
            "/manage/content",
            "",
            "",
        ),
        rows: page.rows,
        forums,
        filter,
        total: page.total,
        previous,
        next,
    })
}

#[derive(Template)]
#[template(path = "manage/detail.html")]
struct DetailTemplate<'a> {
    p: PageMeta<'a>,
    post: governance::ManagedPost,
    revisions: Vec<governance::Revision>,
    revisions_total: i64,
    revisions_page: i64,
    previous: String,
    next: String,
    request_key: String,
    message: String,
    reason: String,
    selected_action: String,
    public_href: String,
    /// ID of this post's pending restore request.
    restorations: Vec<i64>,
    /// Can edit on others' behalf, restore withdrawals, pin and highlight (administrators, global moderators,
    /// moderators).
    full: bool,
    /// Can lock and unlock (reviewers cannot).
    can_lock: bool,
}
async fn detail_render(
    state: &AppState,
    pid: i64,
    revisions_page: i64,
    status: StatusCode,
    message: String,
    reason: String,
    selected_action: String,
) -> Response {
    let actor = match actor(state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let (post, revisions) = match governance::post(&state.db, actor, pid, revisions_page).await {
        Ok(post) => post,
        Err(error) => return failure(state, error),
    };
    let public_href = match crate::content::thread(&state.db, post.tid as u64).await {
        Ok(Some(thread)) => thread
            .posts
            .iter()
            .find(|item| item.pid == pid as u64)
            .map(|item| super::post_location(state, &thread, item, false))
            .unwrap_or_else(|| format!("/thread-{}", post.tid)),
        _ => format!("/thread-{}", post.tid),
    };
    let restorations = match crate::moderation::pending_for_post(&state.db, pid).await {
        Ok(ids) => ids,
        Err(error) => return super::database_error(state, anyhow::Error::from(error)),
    };
    let path = format!("/manage/content/{pid}");
    let previous = if revisions_page > 1 {
        format!("{path}?revisions_page={}#revisions", revisions_page - 1)
    } else {
        String::new()
    };
    let next = if revisions_page * governance::PAGE_SIZE < revisions.total {
        format!("{path}?revisions_page={}#revisions", revisions_page + 1)
    } else {
        String::new()
    };
    let role = state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.adminid)
        .unwrap_or(0);
    let full = crate::content::edits::staff_can_edit(role);
    let can_lock = crate::content::edits::staff_action_allowed(
        role,
        crate::content::edits::ContentAction::Lock,
    );
    let mut response = render(&DetailTemplate {
        full,
        can_lock,
        p: meta(
            state,
            crate::i18n::current().forum_detail_title().into(),
            crate::i18n::current().forum_detail_description().into(),
            &path,
            "",
            "",
        ),
        post,
        revisions: revisions.rows,
        revisions_total: revisions.total,
        revisions_page,
        previous,
        next,
        request_key: identity::random_token(),
        message,
        reason,
        selected_action,
        public_href,
        restorations,
    });
    *response.status_mut() = status;
    response
}
impl DetailTemplate<'_> {
    /// The links to this post's pending restoration requests, joined as a list in this language.
    fn restoration_links(&self) -> crate::i18n::Markup {
        let base = crate::i18n::Markup::text(&crate::site::base().to_string());
        let links: Vec<String> = self
            .restorations
            .iter()
            .map(|id| format!("<a href=\"{base}/manage/restorations/{id}\">#{id}</a>"))
            .collect();
        let separator = crate::i18n::Markup::text(self.p.t.common_list_separator());
        crate::i18n::Markup::trusted(links.join(separator.as_str()))
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct DetailQuery {
    revisions_page: Option<i64>,
}
async fn detail_page(
    Page(state): Page,
    Path(pid): Path<i64>,
    Query(query): Query<DetailQuery>,
) -> Response {
    detail_render(
        &state,
        pid,
        query.revisions_page.unwrap_or(1),
        StatusCode::OK,
        String::new(),
        String::new(),
        String::new(),
    )
    .await
}
#[derive(Deserialize)]
struct ActionForm {
    _csrf: String,
    expected_post_version: i64,
    expected_thread_version: i64,
    request_key: String,
    reason: String,
    action: ContentAction,
}
csrf_forms!(ActionForm);
async fn change(
    Page(state): Page,
    Path(pid): Path<i64>,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if let Err(error) = governance::staff(&state.db, actor).await {
        return failure(&state, error);
    }
    if matches!(form.action, ContentAction::Withdraw) {
        return error_page(
            &state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().forum_content_error_withdraw(),
        );
    }
    let reason = form.reason.clone();
    let selected_action = serde_json::to_value(form.action)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    if !(1..=500).contains(&reason.trim().chars().count()) {
        return detail_render(
            &state,
            pid,
            1,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().forum_content_error_reason().into(),
            reason,
            selected_action,
        )
        .await;
    }
    let request = ChangeRequest {
        pid,
        expected_post_version: form.expected_post_version,
        expected_thread_version: form.expected_thread_version,
        request_key: form.request_key,
        reason: form.reason,
        action: form.action,
    };
    match ContentEditor::new(
        state.db.clone(),
        state.config.content.clone(),
        state.config.media.orphan_seconds,
    )
    .with_global(state.global.clone())
    .change(actor, request)
    .await
    {
        Ok(_) => auth::redirect(&format!("/manage/content/{pid}")),
        Err(error) => {
            let (status, message) = posting::write_error(error);
            detail_render(&state, pid, 1, status, message, reason, selected_action).await
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct MemberQuery {
    q: String,
    page: Option<i64>,
    uid: Option<i64>,
}
#[derive(Template)]
#[template(path = "manage/logs.html")]
struct LogTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::LogEntry>,
    total: i64,
    page: i64,
    previous: bool,
    next: bool,
    filter: governance::LogFilter,
    previous_href: String,
    next_href: String,
}
async fn logs_page(Page(state): Page, Query(query): Query<governance::LogFilter>) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let page = query.page.unwrap_or(1);
    match governance::logs_filtered(&state.db, actor, &query).await {
        Ok((rows, total)) => render(&LogTemplate {
            p: meta(
                &state,
                crate::i18n::current().maintenance_logs_title().into(),
                crate::i18n::current().maintenance_logs_description().into(),
                "/manage/logs",
                "",
                "",
            ),
            previous_href: query.location(page - 1),
            next_href: query.location(page + 1),
            filter: query,
            rows,
            total,
            page,
            previous: page > 1,
            next: page * governance::PAGE_SIZE < total,
        }),
        Err(error) => failure(&state, error),
    }
}
struct ForumChoice {
    fid: i64,
    name: String,
    selected: bool,
}
struct AssignmentFields {
    uid: i64,
    username: String,
    adminid: i64,
    expected_version: i64,
    reason: String,
    forums: Vec<ForumChoice>,
    request_key: String,
}
#[derive(Template)]
#[template(path = "manage/moderators.html")]
struct ModeratorTemplate<'a> {
    p: PageMeta<'a>,
    members: Vec<governance::RoleMember>,
    fields: Option<AssignmentFields>,
    message: String,
    q: String,
    total: i64,
    previous: String,
    next: String,
}
async fn moderators_render(
    state: &AppState,
    query: MemberQuery,
    status: StatusCode,
    message: String,
    draft: Option<RoleAssignment>,
) -> Response {
    let actor = match actor(state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let page = query.page.unwrap_or(1);
    let (members, total) = match governance::members(&state.db, actor, &query.q, page).await {
        Ok(result) => result,
        Err(error) => return failure(state, error),
    };
    let fields = if let Some(uid) = query.uid {
        let member = match governance::member(&state.db, actor, uid).await {
            Ok(member) => member,
            Err(error) => return failure(state, error),
        };
        if member.adminid == 1 {
            None
        } else {
            let forums = match governance::forums(&state.db, actor).await {
                Ok(forums) => forums,
                Err(error) => return failure(state, error),
            };
            let (adminid, selected, reason) = match draft {
                Some(draft) => (draft.adminid, draft.forum_ids, draft.reason),
                None => (member.adminid, member.forum_ids, String::new()),
            };
            Some(AssignmentFields {
                uid,
                username: member.username,
                adminid,
                expected_version: member.version,
                reason,
                forums: forums
                    .into_iter()
                    .map(|forum| ForumChoice {
                        fid: forum.fid,
                        name: forum.label(),
                        selected: selected.contains(&forum.fid),
                    })
                    .collect(),
                request_key: identity::random_token(),
            })
        }
    } else {
        None
    };
    let href = |page: i64| {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        params
            .append_pair("q", &query.q)
            .append_pair("page", &page.to_string());
        format!("/manage/moderators?{}", params.finish())
    };
    let previous = if page > 1 {
        href(page - 1)
    } else {
        String::new()
    };
    let next = if page * governance::PAGE_SIZE < total {
        href(page + 1)
    } else {
        String::new()
    };
    let mut response = render(&ModeratorTemplate {
        p: meta(
            state,
            crate::i18n::current().forum_moderators_title().into(),
            crate::i18n::current().forum_moderators_description().into(),
            "/manage/moderators",
            "",
            "",
        ),
        members,
        fields,
        message,
        q: query.q,
        total,
        previous,
        next,
    });
    *response.status_mut() = status;
    response
}
async fn moderators_page(Page(state): Page, Query(query): Query<MemberQuery>) -> Response {
    moderators_render(&state, query, StatusCode::OK, String::new(), None).await
}
async fn assign(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let actor = match actor(&state) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let number = |name: &str| form.get(name).and_then(|value| value.parse::<i64>().ok());
    let (Some(target_uid), Some(adminid), Some(expected_version)) = (
        number("target_uid"),
        number("adminid"),
        number("expected_version"),
    ) else {
        return error_page(
            &state,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().forum_moderators_error_form(),
        );
    };
    let forum_ids = if crate::identity::forum_role(adminid) {
        form.keys()
            .filter_map(|name| name.strip_prefix("forum_"))
            .filter_map(|value| value.parse::<i64>().ok())
            .collect()
    } else {
        vec![]
    };
    let request = RoleAssignment {
        target_uid,
        adminid,
        expected_version,
        forum_ids,
        request_key: form.get("request_key").cloned().unwrap_or_default(),
        reason: form.get("reason").cloned().unwrap_or_default(),
    };
    match state.identity.assign_role(actor, request.clone()).await {
        Ok(uid) => auth::redirect(&format!("/manage/moderators?uid={uid}")),
        Err(error) => {
            let status = match &error {
                identity::IdentityError::Conflict(_) => StatusCode::CONFLICT,
                identity::IdentityError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
                _ => return failure(&state, error),
            };
            moderators_render(
                &state,
                MemberQuery {
                    uid: Some(target_uid),
                    ..MemberQuery::default()
                },
                status,
                error.to_string(),
                Some(request),
            )
            .await
        }
    }
}
