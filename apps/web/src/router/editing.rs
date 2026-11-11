//! Editing and withdrawal use the same HTML protocol as publishing.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::{
    content::edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
    identity,
};
use axum::{
    extract::{Form, Path as AxumPath},
    http::HeaderMap,
    routing::post,
};

pub(super) fn service(state: &AppState) -> ContentEditor {
    ContentEditor::new(
        state.db.clone(),
        state.config.content.clone(),
        state.config.media.orphan_seconds,
    )
    .with_site_url(state.site_url())
    .with_global(state.global.clone())
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/edit/{target}", get(edit_page).post(save))
        .route("/api/posts/{pid}/withdraw", post(withdraw))
        .route("/api/posts/{pid}/restore", post(restore))
}

async fn target(state: &AppState, target: &str) -> Result<(i64, i64, bool), Response> {
    let (is_thread, value) = if let Some(value) = target.strip_prefix("thread-") {
        (true, value)
    } else if let Some(value) = target.strip_prefix("post-") {
        (false, value)
    } else {
        return Err(error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().editor_error_edit_target(),
        ));
    };
    let Some(id) = value.parse::<i64>().ok().filter(|id| *id > 0) else {
        return Err(error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().editor_error_edit_target(),
        ));
    };
    let sql = if is_thread {
        "SELECT pid,tid,first FROM gonof_forum_post WHERE first_tid=?"
    } else {
        "SELECT pid,tid,first FROM gonof_forum_post WHERE pid=?"
    };
    match crate::db::query(sql)
        .bind(id)
        .fetch_optional(state.db.executor())
        .await
    {
        Ok(Some(row)) => {
            let decode = || -> anyhow::Result<_> {
                Ok((
                    row.try_get("pid")?,
                    row.try_get("tid")?,
                    row.try_get::<i64, _>("first")? == 1,
                ))
            };
            decode().map_err(|e| database_error(state, e))
        }
        Ok(None) => Err(error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().editor_error_edit_target(),
        )),
        Err(err) => Err(database_error(state, err.into())),
    }
}

async fn edit_page(Page(state): Page, AxumPath(segment): AxumPath<String>) -> Response {
    if !state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| s.member.is_staff())
        && let Some(response) = posting::gate(&state, &format!("/edit/{segment}"))
    {
        return response;
    }
    let (pid, tid, first) = match target(&state, &segment).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let session = state
        .viewer
        .session
        .as_ref()
        .expect("editing gate requires session");
    let post = match service(&state).edit_form(session.member.actor(), pid).await {
        Ok(post) => post,
        Err(err) => {
            let (status, message) = posting::write_error(err);
            return error_page(&state, status, &message);
        }
    };
    let canonical = if first {
        format!("thread-{tid}")
    } else {
        format!("post-{pid}")
    };
    if segment != canonical {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(
                header::LOCATION,
                crate::site::href(format!("/edit/{canonical}")),
            )],
        )
            .into_response();
    }
    let mut e = posting::Editor {
        edit_pid: pid,
        edit_thread: first,
        tid,
        fid: post.fid,
        title: post.title,
        body: post.body,
        visibility: if first {
            post.visibility
        } else {
            String::new()
        },
        expected_post_version: post.version,
        expected_thread_version: post.thread_version,
        management: post.can_manage && !post.can_author_edit,
        location: format!("/thread-{tid}"),
        ..Default::default()
    };
    e.prepare();
    posting::editor_page(&state, &e, StatusCode::OK).await
}

async fn save(
    Page(state): Page,
    AxumPath(segment): AxumPath<String>,
    headers: HeaderMap,
    Form(mut e): Form<posting::Editor>,
) -> Response {
    let (pid, tid, first) = match target(&state, &segment).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    e.edit_pid = pid;
    e.edit_thread = first;
    e.tid = tid;
    e.preserve_input = true;
    let key = e.request_key.clone();
    e.prepare();
    e.request_key = key;
    // Same as posting::submit: the expired-form response uses the editor that is already prepared and carries
    // edit_pid.
    if let Some(reason) = auth::csrf_failure(&state, &headers, &e._csrf) {
        e.message = crate::i18n::current().editor_error_stale(reason);
        return posting::failure(&state, &headers, &e, StatusCode::FORBIDDEN).await;
    }
    let Some(session) = &state.viewer.session else {
        e.message = crate::i18n::current().editor_error_sign_in_to_edit().into();
        return posting::failure(&state, &headers, &e, StatusCode::UNAUTHORIZED).await;
    };
    e.management = session.member.is_staff();
    // Only the first post carries a visibility scope; when the form has no such field (a reply, or the forum is
    // already stricter than public) the original value is kept.
    let visibility = if first {
        match posting::submitted_visibility(&e.visibility) {
            Ok(value) => value,
            Err(err) => {
                let (status, message) = posting::write_error(err);
                e.message = message;
                return posting::failure(&state, &headers, &e, status).await;
            }
        }
    } else {
        None
    };
    let result = service(&state)
        .edit(
            session.member.actor(),
            EditRequest {
                pid,
                expected_post_version: e.expected_post_version,
                expected_thread_version: e.expected_thread_version,
                title: first.then(|| e.title.clone()),
                body: Some(e.body.clone()),
                request_key: e.request_key.clone(),
                reason: e.reason.clone(),
                visibility,
            },
        )
        .await;
    match result {
        Ok(result) => posting::published(&state, &headers, &e, result).await,
        Err(err) => {
            let (status, message) = posting::write_error(err);
            e.message = message;
            if status == StatusCode::CONFLICT
                && let Ok(current) = service(&state).editable(session.member.actor(), pid).await
            {
                e.current_title = current.title;
                e.current_body = current.body;
            }
            posting::failure(&state, &headers, &e, status).await
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ActionForm {
    _csrf: String,
    request_key: String,
    expected_post_version: i64,
    expected_thread_version: i64,
    reason: String,
    view: String,
}
csrf_forms!(ActionForm);
async fn withdraw(
    Page(state): Page,
    AxumPath(pid): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Response {
    change(state, pid, headers, form, ContentAction::Withdraw).await
}
async fn restore(
    Page(state): Page,
    AxumPath(pid): AxumPath<i64>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<ActionForm>,
) -> Response {
    change(state, pid, headers, form, ContentAction::Restore).await
}
async fn change(
    state: AppState,
    pid: i64,
    headers: HeaderMap,
    form: ActionForm,
    action: ContentAction,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service(&state)
        .change(
            session.member.actor(),
            ChangeRequest {
                pid,
                action,
                expected_post_version: form.expected_post_version,
                expected_thread_version: form.expected_thread_version,
                request_key: form.request_key,
                reason: form.reason,
            },
        )
        .await
    {
        Ok(result) => {
            let mut e = posting::Editor {
                edit_pid: pid,
                tid: result.tid,
                edit_thread: result.position == 1,
                view: form.view,
                request_key: identity::random_token(),
                ..Default::default()
            };
            // A state action must never clear a concurrent editor draft.
            e.draft = format!("state-post-{pid}");
            posting::published(&state, &headers, &e, result).await
        }
        Err(err) => {
            let (status, message) = posting::write_error(err);
            error_page(&state, status, &message)
        }
    }
}
