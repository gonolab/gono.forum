use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::{content::writes::ContentWriteError, identity, moderation as governance};
use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath},
    routing::post,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/reports", get(my_reports).post(submit))
        .route("/my/reports/new", get(new_report))
        .route("/my/reports/{id}", get(my_report))
        .route("/manage/reports", get(queue))
        .route("/manage/reports/{id}", get(review).post(resolve))
        .route("/my/sanctions", get(my_sanctions))
        .route("/manage/sanctions", get(sanctions_page).post(issue))
        .route("/manage/sanctions/{id}/revoke", post(revoke))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
fn failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}
#[allow(clippy::result_large_err)]
fn actor(state: &AppState, staff: bool) -> Result<identity::Actor, Response> {
    let Some(s) = &state.viewer.session else {
        return Err(auth::redirect("/login"));
    };
    if staff && !s.member.is_staff() {
        return Err(error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().governance_manage_error_not_staff(),
        ));
    }
    Ok(s.member.actor())
}
#[derive(Template)]
#[template(path = "reports/form.html")]
struct ReportFormTemplate<'a> {
    p: PageMeta<'a>,
    target: governance::ReportTargetView,
    key: String,
    reason: String,
    message: String,
}
async fn form_view(
    state: &AppState,
    target: governance::ReportTarget,
    key: String,
    reason: String,
    message: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state, false) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let target = match governance::report_target(&state.db, &target, false, actor.uid).await {
        Ok(t) => t,
        Err(e) => return failure(state, e),
    };
    if target.owner == Some(actor.uid) {
        return error_page(
            state,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().governance_error_report_own(),
        );
    }
    let mut response = render(&ReportFormTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_report_content().into(),
            crate::i18n::current()
                .governance_report_description()
                .into(),
            "/my/reports/new",
            "",
            "",
        ),
        target,
        key,
        reason,
        message,
    });
    *response.status_mut() = status;
    response
}
async fn new_report(Page(state): Page, Query(target): Query<governance::ReportTarget>) -> Response {
    form_view(
        &state,
        target,
        identity::random_token(),
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}
#[derive(Deserialize)]
struct ReportForm {
    _csrf: String,
    target_type: String,
    target_id: i64,
    reason: String,
    request_key: String,
}
csrf_forms!(ReportForm);
async fn submit(Page(state): Page, CsrfForm(form): CsrfForm<ReportForm>) -> Response {
    let actor = match actor(&state, false) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let target = governance::ReportTarget {
        target_type: form.target_type,
        target_id: form.target_id,
    };
    match governance::submit_report(
        &state.db,
        actor,
        &target,
        &form.reason,
        &form.request_key,
        identity::now(),
        state.frame.settings.limit.report_daily,
    )
    .await
    {
        Ok(id) => auth::redirect(&format!("/my/reports/{id}")),
        Err(e) => {
            let (status, message) = posting::write_error(e);
            form_view(
                &state,
                target,
                form.request_key,
                form.reason,
                message,
                status,
            )
            .await
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ListQuery {
    status: String,
    before: i64,
    uid: i64,
}
#[derive(Template)]
#[template(path = "reports/list.html")]
struct ReportListTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::Report>,
    staff: bool,
    status: String,
    next: String,
}

#[derive(Template)]
#[template(path = "my/reports.html")]
struct MyReportsTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    rows: Vec<governance::Report>,
    staff: bool,
    status: String,
    next: String,
}
async fn list_view(state: &AppState, query: ListQuery, staff: bool) -> Response {
    let actor = match actor(state, staff) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let mut rows =
        match governance::report_list(&state.db, actor, staff, &query.status, query.before).await {
            Ok(rows) => rows,
            Err(e) => return failure(state, e),
        };
    let path = if staff {
        "/manage/reports"
    } else {
        "/my/reports"
    };
    let next = if rows.len() > 20 {
        rows.truncate(20);
        format!(
            "{path}?status={}&before={}",
            query.status,
            rows.last().unwrap().id
        )
    } else {
        String::new()
    };
    let mut p = meta(
        state,
        if staff {
            crate::i18n::current().governance_manage_reports()
        } else {
            crate::i18n::current().governance_my_reports()
        }
        .into(),
        crate::i18n::current()
            .governance_reports_description()
            .into(),
        path,
        "",
        "",
    );
    if staff {
        return render(&ReportListTemplate {
            p,
            rows,
            staff,
            status: query.status,
            next,
        });
    }
    // The member view uses the personal center layout; the admin area keeps the original full-page layout, and both
    // share the same body partial.
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    render(&MyReportsTemplate {
        p,
        frame: crate::router::my::frame("reports"),
        rows,
        staff,
        status: query.status,
        next,
    })
}
async fn my_reports(Page(state): Page, Query(query): Query<ListQuery>) -> Response {
    list_view(&state, query, false).await
}
async fn queue(Page(state): Page, Query(mut query): Query<ListQuery>) -> Response {
    if query.status.is_empty() {
        query.status = "pending".into();
    }
    list_view(&state, query, true).await
}
#[derive(Template)]
#[template(path = "reports/detail.html")]
struct ReportDetailTemplate<'a> {
    p: PageMeta<'a>,
    r: governance::Report,
    target: Option<governance::ReportTargetView>,
    staff: bool,
    /// Moderators and global moderators recuse themselves from reports about their own content: view only, no review.
    recuse: bool,
    message: String,
    resolution: String,
    selected: String,
}
async fn detail_view(
    state: &AppState,
    id: i64,
    staff: bool,
    message: String,
    resolution: String,
    selected: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state, staff) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let r = match governance::report_detail(&state.db, actor, id, staff).await {
        Ok(r) => r,
        Err(e) => return failure(state, e),
    };
    let target = match governance::report_target(&state.db, &r.target, staff, actor.uid).await {
        Ok(t) => Some(t),
        Err(ContentWriteError::NotFound) => None,
        Err(e) => return failure(state, e),
    };
    let recuse = staff
        && state.viewer.session.as_ref().is_some_and(|s| {
            !s.member.is_admin
                && target
                    .as_ref()
                    .is_some_and(|t| t.owner == Some(s.member.uid))
        });
    let mut response = render(&ReportDetailTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_report_number(id.to_string()),
            crate::i18n::current()
                .governance_report_detail_description()
                .into(),
            // The same detail page serves two entry points: moderators come from the admin area, authors from the
            // personal center. Passing the wrong one would point canonical at the other's URL, and the admin
            // directory would not recognise which page this is.
            &if staff {
                format!("/manage/reports/{id}")
            } else {
                format!("/my/reports/{id}")
            },
            "",
            "",
        ),
        r,
        target,
        staff,
        recuse,
        message,
        resolution,
        selected,
    });
    *response.status_mut() = status;
    response
}
async fn my_report(Page(state): Page, AxumPath(id): AxumPath<i64>) -> Response {
    detail_view(
        &state,
        id,
        false,
        String::new(),
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}
async fn review(Page(state): Page, AxumPath(id): AxumPath<i64>) -> Response {
    detail_view(
        &state,
        id,
        true,
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
    let actor = match actor(&state, true) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::resolve_report(
        &state.db,
        actor,
        id,
        &form.status,
        &form.resolution,
        identity::now(),
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("/manage/reports/{id}")),
        Err(e) => {
            let (status, message) = posting::write_error(e);
            detail_view(
                &state,
                id,
                true,
                message,
                form.resolution,
                form.status,
                status,
            )
            .await
        }
    }
}
#[derive(Template)]
#[template(path = "my/sanctions.html")]
struct MySanctionsTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    staff: bool,
    /// The viewer's role: 1 administrator can impose all three sanctions, 2 global moderator can only mute.
    role: i64,
    member: Option<governance::SanctionMember>,
    rows: Vec<governance::Sanction>,
    form: governance::SanctionRequest,
    message: String,
    next: String,
}

#[derive(Template)]
#[template(path = "manage/sanctions.html")]
struct SanctionsTemplate<'a> {
    p: PageMeta<'a>,
    staff: bool,
    role: i64,
    member: Option<governance::SanctionMember>,
    rows: Vec<governance::Sanction>,
    form: governance::SanctionRequest,
    message: String,
    next: String,
}
async fn sanctions_view(
    state: &AppState,
    query: ListQuery,
    staff: bool,
    form: Option<governance::SanctionRequest>,
    message: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state, staff) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let role = state.viewer.session.as_ref().unwrap().member.adminid;
    if staff && !matches!(role, 1 | 2) {
        return error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().governance_manage_error_sanction_staff(),
        );
    }
    let uid = if staff { query.uid } else { actor.uid };
    let member = if staff && uid > 0 {
        match governance::sanction_member(&state.db, actor, uid).await {
            Ok(m) => Some(m),
            Err(e) => return failure(state, e),
        }
    } else {
        None
    };
    let mut rows = if uid > 0 {
        match governance::sanctions(&state.db, actor, uid, staff, query.before).await {
            Ok(r) => r,
            Err(e) => return failure(state, e),
        }
    } else {
        vec![]
    };
    let path = if staff {
        "/manage/sanctions"
    } else {
        "/my/sanctions"
    };
    let next = if rows.len() > 20 {
        rows.truncate(20);
        format!("{path}?uid={uid}&before={}", rows.last().unwrap().id)
    } else {
        String::new()
    };
    let mut form = form.unwrap_or_else(|| governance::SanctionRequest {
        uid,
        expected_version: member.as_ref().map(|m| m.version).unwrap_or(0),
        kind: if role == 1 {
            "warning"
        } else {
            governance::MUTE
        }
        .into(),
        hours: 0,
        reason: String::new(),
        request_key: identity::random_token(),
    });
    // Preserve the submitted version on errors; a fresh GET is required to acknowledge changed state.
    form.uid = uid;
    let mut p = meta(
        state,
        if staff {
            crate::i18n::current().governance_manage_sanctions()
        } else {
            crate::i18n::current().governance_my_sanctions()
        }
        .into(),
        crate::i18n::current()
            .governance_sanctions_description()
            .into(),
        path,
        "",
        "",
    );
    let mut response = if staff {
        render(&SanctionsTemplate {
            p,
            staff,
            role,
            member,
            rows,
            form,
            message,
            next,
        })
    } else {
        if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
            return error;
        }
        render(&MySanctionsTemplate {
            p,
            frame: crate::router::my::frame("sanctions"),
            staff,
            role,
            member,
            rows,
            form,
            message,
            next,
        })
    };
    *response.status_mut() = status;
    response
}
async fn sanctions_page(Page(state): Page, Query(query): Query<ListQuery>) -> Response {
    sanctions_view(&state, query, true, None, String::new(), StatusCode::OK).await
}
async fn my_sanctions(Page(state): Page, Query(query): Query<ListQuery>) -> Response {
    sanctions_view(&state, query, false, None, String::new(), StatusCode::OK).await
}
#[derive(Deserialize)]
struct SanctionForm {
    _csrf: String,
    uid: i64,
    expected_version: i64,
    kind: String,
    hours: i64,
    reason: String,
    request_key: String,
}
csrf_forms!(SanctionForm);
async fn issue(Page(state): Page, CsrfForm(form): CsrfForm<SanctionForm>) -> Response {
    let actor = match actor(&state, true) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let request = governance::SanctionRequest {
        uid: form.uid,
        expected_version: form.expected_version,
        kind: form.kind,
        hours: form.hours,
        reason: form.reason,
        request_key: form.request_key,
    };
    match governance::issue_sanction(&state.db, actor, &request, identity::now()).await {
        Ok(_) => auth::redirect(&format!("/manage/sanctions?uid={}", request.uid)),
        Err(e) => {
            let (status, message) = posting::write_error(e);
            sanctions_view(
                &state,
                ListQuery {
                    uid: request.uid,
                    ..Default::default()
                },
                true,
                Some(request),
                message,
                status,
            )
            .await
        }
    }
}
#[derive(Deserialize)]
struct RevokeForm {
    _csrf: String,
    reason: String,
}
csrf_forms!(RevokeForm);
async fn revoke(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<RevokeForm>,
) -> Response {
    let actor = match actor(&state, true) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::revoke_sanction(&state.db, actor, id, &form.reason, identity::now()).await {
        Ok(uid) => auth::redirect(&format!("/manage/sanctions?uid={uid}")),
        Err(e) => failure(&state, e),
    }
}
