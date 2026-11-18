use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{content::writes::ContentWriteError, identity, moderation as governance};
use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath},
    http::HeaderMap,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/appeals", get(index).post(request_access))
        .route("/appeals/access", get(access_page).post(exchange))
        .route("/appeals/exit", axum::routing::post(exit))
        .route("/appeals/sanctions/{id}", get(detail).post(submit))
        // Signed-in members enter appeals from the account record; frozen accounts still use /appeals with a
        // mailbox ticket.
        .route("/my/sanctions/{id}/appeal", get(detail).post(submit))
        .route("/manage/appeals", get(queue))
        .route("/manage/appeals/{id}", get(review).post(resolve))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
fn failure(state: &AppState, error: ContentWriteError) -> Response {
    if matches!(error, ContentWriteError::Unauthorized) {
        return auth::redirect("/appeals?expired=true");
    }
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}
fn access(state: &AppState, headers: &HeaderMap) -> Option<governance::AppealAccess> {
    // An explicit email grant determines the account within the appeal area even
    // when the browser already has a different ordinary forum session.
    if let Some(raw) = auth::cookie(headers, &state.viewer.plain_cookie(COOKIE)) {
        return Some(governance::AppealAccess::Token(raw));
    }
    state
        .viewer
        .session
        .as_ref()
        .map(|session| governance::AppealAccess::Session(session.member.actor()))
}

/// The appeal access token of a member who confirmed by mail; named with this forum's suffix
/// ([`auth::cookie_name`]).
const COOKIE: &str = "gonof_appeal";

fn cookie(response: &mut Response, state: &AppState, value: &str, seconds: i64) {
    let secure = if state.viewer.secure { "; Secure" } else { "" };
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={value}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
            state.viewer.plain_cookie(COOKIE),
            // Sent only to the appeal pages, with this request's path prefix.
            crate::site::href("/appeals"),
            seconds.max(0)
        ))
        .expect("internal cookie"),
    );
}
#[allow(clippy::result_large_err)]
fn admin(state: &AppState) -> Result<identity::Actor, Response> {
    let Some(session) = &state.viewer.session else {
        return Err(auth::redirect("/login"));
    };
    if !session.member.is_admin {
        return Err(error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().governance_manage_error_appeal_admin(),
        ));
    }
    Ok(session.member.actor())
}
#[derive(Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    before: i64,
    #[serde(default)]
    sent: bool,
    #[serde(default)]
    expired: bool,
    #[serde(default = "pending")]
    status: String,
}
fn pending() -> String {
    "pending".into()
}
#[derive(Default, Deserialize)]
struct AccessForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    token: String,
}
csrf_forms!(AccessForm);
#[derive(Template)]
#[template(path = "appeals/access.html")]
struct AccessTemplate<'a> {
    p: PageMeta<'a>,
    token: String,
    confirm: bool,
    message: String,
}
fn access_view(
    state: &AppState,
    token: String,
    confirm: bool,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut response = render(&AccessTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_appeals().into(),
            crate::i18n::current()
                .governance_appeals_description()
                .into(),
            "/appeals",
            "",
            "",
        ),
        token,
        confirm,
        message: message.into(),
    });
    *response.status_mut() = status;
    response
}
#[derive(Template)]
#[template(path = "appeals/list.html")]
struct CasesTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::AppealCase>,
    next: String,
}
async fn index(Page(state): Page, headers: HeaderMap, Query(query): Query<ListQuery>) -> Response {
    if query.sent {
        return access_view(
            &state,
            String::new(),
            false,
            crate::i18n::current().governance_appeals_sent(),
            StatusCode::OK,
        );
    }
    if !query.expired
        && let Some(access) = access(&state, &headers)
    {
        match governance::appeal_cases(&state.db, &access, query.before, identity::now()).await {
            Ok(mut rows) => {
                let next = if rows.len() > 20 {
                    rows.truncate(20);
                    format!("/appeals?before={}", rows.last().unwrap().sanction.id)
                } else {
                    String::new()
                };
                return render(&CasesTemplate {
                    p: meta(
                        &state,
                        crate::i18n::current().governance_my_appeals().into(),
                        crate::i18n::current()
                            .governance_appeals_list_description()
                            .into(),
                        "/appeals",
                        "",
                        "",
                    ),
                    rows,
                    next,
                });
            }
            Err(ContentWriteError::Unauthorized) => {}
            Err(e) => return failure(&state, e),
        }
    }
    let mut response = access_view(
        &state,
        String::new(),
        false,
        if query.expired {
            crate::i18n::current().governance_appeals_expired()
        } else {
            ""
        },
        StatusCode::OK,
    );
    cookie(&mut response, &state, "", 0);
    response
}
async fn request_access(Page(state): Page, CsrfForm(form): CsrfForm<AccessForm>) -> Response {
    match state
        .limit(&[Rule::new(
            "appeal-mail-ip",
            &state.viewer.client_ip,
            10,
            3600,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return access_view(
                &state,
                String::new(),
                false,
                crate::i18n::current().governance_appeals_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    let started = tokio::time::Instant::now();
    let result = governance::request_appeal_access(&state.db, &form.email, identity::now()).await;
    tokio::time::sleep_until(started + std::time::Duration::from_millis(250)).await;
    match result {
        Ok(()) => auth::redirect("/appeals?sent=true"),
        Err(e) => failure(&state, e),
    }
}
async fn access_page(Page(state): Page, Query(form): Query<AccessForm>) -> Response {
    match governance::check_appeal_access(&state.db, &form.token, identity::now()).await {
        Ok(()) => access_view(&state, form.token, true, "", StatusCode::OK),
        Err(ContentWriteError::Unauthorized) => access_view(
            &state,
            String::new(),
            false,
            crate::i18n::current().governance_appeals_invalid(),
            StatusCode::BAD_REQUEST,
        ),
        Err(e) => failure(&state, e),
    }
}
async fn exchange(Page(state): Page, CsrfForm(form): CsrfForm<AccessForm>) -> Response {
    match state
        .limit(&[Rule::new(
            "appeal-exchange",
            &state.viewer.client_ip,
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
                crate::i18n::current().my_error_too_many_attempts(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    match governance::exchange_appeal_access(&state.db, &form.token, identity::now()).await {
        Ok((token, seconds)) => {
            let mut r = auth::redirect("/appeals");
            cookie(&mut r, &state, &token, seconds);
            r
        }
        Err(e) => failure(&state, e),
    }
}
#[derive(Default, Deserialize)]
struct AppealForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    request_key: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    resolution: String,
}
csrf_forms!(AppealForm);
#[derive(Template)]
#[template(path = "appeals/detail.html")]
struct DetailTemplate<'a> {
    p: PageMeta<'a>,
    case: governance::AppealCase,
    staff: bool,
    /// Entered with a forum session: the form and redirects stay at /my/sanctions/:id/appeal.
    member: bool,
    form: AppealForm,
    message: String,
}
/// Appeals of signed-in members hang under the account record; ticket holders stay at /appeals (the Path of the
/// ticket cookie only reaches /appeals).
fn member_access(access: &governance::AppealAccess) -> bool {
    matches!(access, governance::AppealAccess::Session(_))
}
fn case_href(member: bool, id: i64) -> String {
    if member {
        format!("/my/sanctions/{id}/appeal")
    } else {
        format!("/appeals/sanctions/{id}")
    }
}
fn detail_view(
    state: &AppState,
    case: governance::AppealCase,
    staff: bool,
    member: bool,
    form: AppealForm,
    message: String,
    status: StatusCode,
) -> Response {
    let member = !staff && member;
    let path = if staff {
        "/manage/appeals".to_owned()
    } else if member {
        case_href(true, case.sanction.id)
    } else {
        "/appeals".to_owned()
    };
    let mut r = render(&DetailTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_appeal_title().into(),
            crate::i18n::current()
                .governance_appeal_description()
                .into(),
            &path,
            "",
            "",
        ),
        case,
        staff,
        member,
        form,
        message,
    });
    *r.status_mut() = status;
    r
}
async fn detail(Page(state): Page, headers: HeaderMap, AxumPath(id): AxumPath<i64>) -> Response {
    let Some(access) = access(&state, &headers) else {
        return auth::redirect("/appeals");
    };
    match governance::appeal_case(&state.db, &access, id, identity::now()).await {
        Ok(case) => detail_view(
            &state,
            case,
            false,
            member_access(&access),
            AppealForm {
                request_key: identity::random_token(),
                ..Default::default()
            },
            String::new(),
            StatusCode::OK,
        ),
        Err(e) => failure(&state, e),
    }
}
async fn submit(
    Page(state): Page,
    headers: HeaderMap,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<AppealForm>,
) -> Response {
    let Some(access) = access(&state, &headers) else {
        return auth::redirect("/appeals");
    };
    match governance::submit_appeal(
        &state.db,
        &access,
        id,
        &form.reason,
        &form.request_key,
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect(&case_href(member_access(&access), id)),
        Err(e) => {
            let case = match governance::appeal_case(&state.db, &access, id, identity::now()).await
            {
                Ok(c) => c,
                Err(e) => return failure(&state, e),
            };
            let (status, message) = posting::write_error(e);
            detail_view(
                &state,
                case,
                false,
                member_access(&access),
                form,
                message,
                status,
            )
        }
    }
}
#[derive(Template)]
#[template(path = "appeals/queue.html")]
struct QueueTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<governance::Appeal>,
    status: String,
    next: String,
}
async fn queue(Page(state): Page, Query(query): Query<ListQuery>) -> Response {
    let actor = match admin(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::appeal_queue(&state.db, actor, &query.status, query.before).await {
        Ok(mut rows) => {
            let next = if rows.len() > 20 {
                rows.truncate(20);
                format!(
                    "/manage/appeals?status={}&before={}",
                    query.status,
                    rows.last().unwrap().id
                )
            } else {
                String::new()
            };
            render(&QueueTemplate {
                p: meta(
                    &state,
                    crate::i18n::current().governance_manage_appeals().into(),
                    crate::i18n::current()
                        .governance_manage_appeals_description()
                        .into(),
                    "/manage/appeals",
                    "",
                    "",
                ),
                rows,
                status: query.status,
                next,
            })
        }
        Err(e) => failure(&state, e),
    }
}
async fn review(Page(state): Page, AxumPath(id): AxumPath<i64>) -> Response {
    let actor = match admin(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::review_appeal(&state.db, actor, id, identity::now()).await {
        Ok(case) => detail_view(
            &state,
            case,
            true,
            false,
            AppealForm::default(),
            String::new(),
            StatusCode::OK,
        ),
        Err(e) => failure(&state, e),
    }
}
async fn resolve(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<AppealForm>,
) -> Response {
    let actor = match admin(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match governance::resolve_appeal(
        &state.db,
        actor,
        id,
        &form.status,
        &form.resolution,
        identity::now(),
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("/manage/appeals/{id}")),
        Err(e) => {
            let case = match governance::review_appeal(&state.db, actor, id, identity::now()).await
            {
                Ok(c) => c,
                Err(e) => return failure(&state, e),
            };
            let (status, message) = posting::write_error(e);
            detail_view(&state, case, true, false, form, message, status)
        }
    }
}

pub(super) fn clear_cookie(response: &mut Response, state: &AppState) {
    cookie(response, state, "", 0);
}
async fn exit(
    Page(state): Page,
    headers: HeaderMap,
    CsrfForm(_): CsrfForm<AccessForm>,
) -> Response {
    if let Some(raw) = auth::cookie(&headers, &state.viewer.plain_cookie(COOKIE))
        && let Err(e) = governance::revoke_appeal_access(&state.db, &raw, identity::now()).await
    {
        return failure(&state, e);
    }
    let mut response = auth::redirect("/login");
    clear_cookie(&mut response, &state);
    response
}
