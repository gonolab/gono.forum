//! Content restoration requests: members submit and view them in their personal center, and staff handle them in the
//! admin area.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::{content::writes::ContentWriteError, identity, moderation as governance};
use axum::extract::{DefaultBodyLimit, Path as AxumPath};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/restorations", get(my_list).post(submit))
        .route("/my/restorations/new", get(new_form))
        .route("/my/restorations/{id}", get(my_detail))
        .route("/manage/restorations", get(queue))
        .route("/manage/restorations/{id}", get(review).post(resolve))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

fn failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}

#[allow(clippy::result_large_err)]
fn actor(state: &AppState, staff: bool, return_to: &str) -> Result<identity::Actor, Response> {
    let Some(s) = &state.viewer.session else {
        return Err(auth::redirect(&format!(
            "/login?return_to={}",
            url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect::<String>()
        )));
    };
    if staff && !s.member.manages_content() {
        return Err(error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().governance_manage_error_not_staff(),
        ));
    }
    Ok(s.member.actor())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PageQuery {
    page: Option<usize>,
}

#[derive(Template)]
#[template(path = "my/restorations.html")]
struct MyListTemplate<'a> {
    p: PageMeta<'a>,
    frame: super::my::Frame,
    rows: Vec<governance::Restoration>,
    total: usize,
    previous: String,
    next: String,
}

async fn my_list(Page(state): Page, Query(query): Query<PageQuery>) -> Response {
    let actor = match actor(&state, false, "/my/restorations") {
        Ok(a) => a,
        Err(r) => return r,
    };
    let page = query.page.unwrap_or(1);
    let result = match governance::own_restorations(&state.db, actor, page).await {
        Ok(result) => result,
        Err(e) => return failure(&state, e),
    };
    let mut p = meta(
        &state,
        crate::i18n::current().governance_restorations().into(),
        crate::i18n::current()
            .governance_restorations_description()
            .into(),
        "/my/restorations",
        "",
        "",
    );
    if let Some(error) = super::my::fill_shell(&state, &mut p).await {
        return error;
    }
    let last = result.total.div_ceil(governance::RESTORE_PAGE_SIZE).max(1);
    render(&MyListTemplate {
        p,
        frame: super::my::frame("restorations"),
        rows: result.rows,
        total: result.total,
        previous: if page > 1 {
            format!("/my/restorations?page={}", page - 1)
        } else {
            String::new()
        },
        next: if page < last {
            format!("/my/restorations?page={}", page + 1)
        } else {
            String::new()
        },
    })
}

#[derive(Template)]
#[template(path = "my/restoration_form.html")]
struct FormTemplate<'a> {
    p: PageMeta<'a>,
    frame: super::my::Frame,
    target: governance::RestoreTarget,
    blocked: &'static str,
    key: String,
    reason: String,
    message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NewQuery {
    pid: i64,
}

async fn form_view(
    state: &AppState,
    pid: i64,
    key: String,
    reason: String,
    message: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state, false, &format!("/my/restorations/new?pid={pid}")) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let target = match governance::restore_target(&state.db, pid, Some(actor.uid)).await {
        Ok(t) => t,
        Err(e) => return failure(state, e),
    };
    let blocked = target
        .eligibility(
            crate::content::edits::edit_window(&state.frame.settings),
            identity::now(),
        )
        .message();
    let mut p = meta(
        state,
        crate::i18n::current().governance_restore_title().into(),
        crate::i18n::current()
            .governance_restore_description()
            .into(),
        "/my/restorations/new",
        "",
        "",
    );
    if let Some(error) = super::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&FormTemplate {
        p,
        frame: super::my::frame("restorations"),
        target,
        blocked,
        key,
        reason,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn new_form(Page(state): Page, Query(query): Query<NewQuery>) -> Response {
    form_view(
        &state,
        query.pid,
        identity::random_token(),
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}

#[derive(Deserialize)]
struct SubmitForm {
    _csrf: String,
    pid: i64,
    reason: String,
    request_key: String,
}
csrf_forms!(SubmitForm);

async fn submit(Page(state): Page, CsrfForm(form): CsrfForm<SubmitForm>) -> Response {
    let actor = match actor(&state, false, "/my/restorations") {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::submit_restoration(
        &state.db,
        actor,
        form.pid,
        &form.reason,
        &form.request_key,
        &governance::RestorationRules::from_settings(&state.frame.settings),
        identity::now(),
    )
    .await
    {
        Ok(id) => auth::redirect(&format!("/my/restorations/{id}")),
        Err(ContentWriteError::NotFound) => failure(&state, ContentWriteError::NotFound),
        Err(e) => {
            let (status, message) = posting::write_error(e);
            form_view(
                &state,
                form.pid,
                form.request_key,
                form.reason,
                message,
                status,
            )
            .await
        }
    }
}

#[derive(Template)]
#[template(path = "my/restoration.html")]
struct MyDetailTemplate<'a> {
    p: PageMeta<'a>,
    frame: super::my::Frame,
    r: governance::Restoration,
    target: Option<governance::RestoreTarget>,
}

async fn my_detail(Page(state): Page, AxumPath(id): AxumPath<i64>) -> Response {
    let actor = match actor(&state, false, &format!("/my/restorations/{id}")) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let r = match governance::restoration_detail(&state.db, actor, id, false).await {
        Ok(r) => r,
        Err(e) => return failure(&state, e),
    };
    let target = match governance::restore_target(&state.db, r.pid, Some(actor.uid)).await {
        Ok(t) => Some(t),
        Err(ContentWriteError::NotFound) => None,
        Err(e) => return failure(&state, e),
    };
    let mut p = meta(
        &state,
        crate::i18n::current().governance_restoration_number(id.to_string()),
        crate::i18n::current()
            .governance_restoration_description()
            .into(),
        &format!("/my/restorations/{id}"),
        "",
        "",
    );
    if let Some(error) = super::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&MyDetailTemplate {
        p,
        frame: super::my::frame("restorations"),
        r,
        target,
    })
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QueueQuery {
    status: String,
    cursor: i64,
}

#[derive(Template)]
#[template(path = "manage/restorations.html")]
struct QueueTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::Restoration>,
    status: String,
    next: String,
}

async fn queue(Page(state): Page, Query(mut query): Query<QueueQuery>) -> Response {
    let actor = match actor(&state, true, "/manage/restorations") {
        Ok(a) => a,
        Err(r) => return r,
    };
    if query.status.is_empty() {
        query.status = "pending".into();
    }
    let mut rows =
        match governance::restoration_queue(&state.db, actor, &query.status, query.cursor).await {
            Ok(rows) => rows,
            Err(e) => return failure(&state, e),
        };
    let next = if rows.len() > 20 {
        rows.truncate(20);
        format!(
            "/manage/restorations?status={}&cursor={}",
            query.status,
            rows.last().map_or(0, |r| r.id)
        )
    } else {
        String::new()
    };
    render(&QueueTemplate {
        p: meta(
            &state,
            crate::i18n::current().governance_restorations().into(),
            crate::i18n::current()
                .governance_manage_restorations_description()
                .into(),
            "/manage/restorations",
            "",
            "",
        ),
        rows,
        status: query.status,
        next,
    })
}

#[derive(Template)]
#[template(path = "manage/restoration.html")]
struct ReviewTemplate<'a> {
    p: PageMeta<'a>,
    r: governance::Restoration,
    target: Option<governance::RestoreTarget>,
    recuse: bool,
    message: String,
    resolution: String,
    selected: String,
}

async fn review_view(
    state: &AppState,
    id: i64,
    message: String,
    resolution: String,
    selected: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state, true, &format!("/manage/restorations/{id}")) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let r = match governance::restoration_detail(&state.db, actor, id, true).await {
        Ok(r) => r,
        Err(e) => return failure(state, e),
    };
    let target = match governance::restore_target(&state.db, r.pid, None).await {
        Ok(t) => Some(t),
        Err(ContentWriteError::NotFound) => None,
        Err(e) => return failure(state, e),
    };
    let is_admin = state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| s.member.is_admin);
    let hider = match governance::hider_now(&state.db, r.pid).await {
        Ok(hider) => hider,
        Err(e) => return failure(state, e),
    };
    let recuse = governance::recused(&r, actor.uid, is_admin, hider);
    let mut response = render(&ReviewTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_restoration_number(id.to_string()),
            crate::i18n::current()
                .governance_manage_restoration_description()
                .into(),
            &format!("/manage/restorations/{id}"),
            "",
            "",
        ),
        r,
        target,
        recuse,
        message,
        resolution,
        selected,
    });
    *response.status_mut() = status;
    response
}

async fn review(Page(state): Page, AxumPath(id): AxumPath<i64>) -> Response {
    review_view(
        &state,
        id,
        String::new(),
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}

#[derive(Deserialize)]
struct ResolveForm {
    _csrf: String,
    status: String,
    resolution: String,
}
csrf_forms!(ResolveForm);

async fn resolve(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ResolveForm>,
) -> Response {
    let actor = match actor(&state, true, &format!("/manage/restorations/{id}")) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let editor = super::editing::service(&state);
    match governance::resolve_restoration(
        &state.db,
        &editor,
        actor,
        id,
        &form.status,
        &form.resolution,
        identity::now(),
    )
    .await
    {
        // Accepting may unhide the first post of a pinned thread, putting it back into the pin index.
        Ok(()) => match pins_changed(&state).await {
            Some(response) => response,
            None => auth::redirect(&format!("/manage/restorations/{id}")),
        },
        Err(e) => {
            let (status, message) = posting::write_error(e);
            review_view(&state, id, message, form.resolution, form.status, status).await
        }
    }
}
