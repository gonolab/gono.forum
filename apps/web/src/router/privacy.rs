use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{identity, privacy as service};
use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath},
    http::HeaderMap,
    routing::post,
};
use tokio_util::io::ReaderStream;
pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/data", get(index))
        .route("/my/data/requests", post(submit))
        .route("/my/data/requests/{id}/cancel", post(cancel))
        .route("/my/data/requests/{id}/download", get(download))
        .route("/data/access", get(access_page).post(access_submit))
        .route("/data/exit", post(exit))
        .route("/manage/data-requests", get(manage))
        .route("/manage/data-requests/{id}/retry", post(retry))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
fn access(state: &AppState, headers: &HeaderMap) -> Option<service::Access> {
    if let Some(raw) = auth::cookie(headers, "gonof_privacy") {
        return Some(service::Access::Token(raw));
    }
    state
        .viewer
        .session
        .as_ref()
        .map(|s| service::Access::Session(s.member.actor()))
}
pub(super) fn cookie(response: &mut Response, state: &AppState, value: &str, seconds: i64) {
    let secure = if state.viewer.secure { "; Secure" } else { "" };
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "gonof_privacy={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
            seconds.max(0)
        ))
        .expect("privacy cookie"),
    );
}
fn failure(state: &AppState, e: identity::IdentityError) -> Response {
    if matches!(e, identity::IdentityError::Unauthorized) {
        auth::redirect("/data/access?expired=true")
    } else {
        auth::identity_error(state, e)
    }
}
#[derive(Default, Deserialize)]
struct QueryFields {
    #[serde(default)]
    before: i64,
    #[serde(default)]
    sent: bool,
    #[serde(default)]
    expired: bool,
    #[serde(default)]
    token: String,
}
#[derive(Default, Deserialize)]
struct FormFields {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    content_mode: String,
    #[serde(default)]
    request_key: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    confirm: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    reason: String,
}
csrf_forms!(FormFields);
#[derive(Template)]
#[template(path = "my/data.html")]
struct MyDataTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    uid: i64,
    rows: Vec<service::DataRequest>,
    next: String,
    request_key: String,
    restricted: bool,
    message: String,
    posts: i64,
    balance: i64,
    admin: bool,
    selected_kind: String,
    selected_mode: String,
}

#[derive(Template)]
#[template(path = "privacy/index.html")]
struct IndexTemplate<'a> {
    p: PageMeta<'a>,
    uid: i64,
    rows: Vec<service::DataRequest>,
    next: String,
    request_key: String,
    restricted: bool,
    message: String,
    posts: i64,
    balance: i64,
    admin: bool,
    selected_kind: String,
    selected_mode: String,
}
async fn index(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<QueryFields>,
) -> Response {
    index_view(&state, &headers, query.before, String::new(), None).await
}
async fn index_view(
    state: &AppState,
    headers: &HeaderMap,
    before: i64,
    message: String,
    draft: Option<(String, String)>,
) -> Response {
    let Some(access) = access(state, headers) else {
        return auth::redirect("/data/access");
    };
    let (uid, mut rows) = match service::list(&state.db, &access, before, identity::now()).await {
        Ok(v) => v,
        Err(e) => return failure(state, e),
    };
    let next = if rows.len() > 20 {
        rows.truncate(20);
        format!("/my/data?before={}", rows.last().unwrap().id)
    } else {
        String::new()
    };
    let summary = async {
        let posts: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid=? AND erased_at=0",
        )
        .bind(uid)
        .fetch_one(state.db.executor())
        .await?;
        let balance: Option<i64> = crate::db::query_scalar(
            "SELECT available FROM gonof_common_credit_account WHERE uid=?",
        )
        .bind(uid)
        .fetch_optional(state.db.executor())
        .await?;
        let role: i64 =
            crate::db::query_scalar("SELECT adminid FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_one(state.db.executor())
                .await?;
        Ok::<_, sqlx::Error>((posts, balance.unwrap_or(0), role == 1))
    }
    .await;
    let (posts, balance, admin) = match summary {
        Ok(v) => v,
        Err(e) => return database_error(state, e.into()),
    };
    let (selected_kind, selected_mode) =
        draft.unwrap_or_else(|| ("export".into(), "retain".into()));
    let restricted = matches!(access, service::Access::Token(_));
    let mut p = meta(
        state,
        crate::i18n::current().governance_data().into(),
        crate::i18n::current().governance_data_description().into(),
        "/my/data",
        "",
        "",
    );
    // A frozen account coming in with an email ticket has no forum session, and the personal center layout would offer
    // a whole set of entries it cannot enter, so this case keeps the original full-page layout; both share the same
    // body partial.
    if restricted {
        return render(&IndexTemplate {
            p,
            uid,
            rows,
            next,
            request_key: identity::random_token(),
            restricted,
            message,
            posts,
            balance,
            admin,
            selected_kind,
            selected_mode,
        });
    }
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    render(&MyDataTemplate {
        p,
        frame: crate::router::my::frame("data"),
        uid,
        rows,
        next,
        request_key: identity::random_token(),
        restricted,
        message,
        posts,
        balance,
        admin,
        selected_kind,
        selected_mode,
    })
}
async fn submit(
    Page(state): Page,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<FormFields>,
) -> Response {
    match state
        .limit(&[Rule::new(
            "privacy-submit",
            &state.viewer.client_ip,
            10,
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
    let Some(access) = access(&state, &headers) else {
        return auth::redirect("/data/access");
    };
    if form.confirm != "yes" {
        let mut response = index_view(
            &state,
            &headers,
            0,
            crate::i18n::current()
                .governance_error_data_confirm()
                .into(),
            Some((form.kind.clone(), form.content_mode.clone())),
        )
        .await;
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return response;
    }
    let draft = Some((form.kind.clone(), form.content_mode.clone()));
    let request = service::NewRequest {
        kind: form.kind,
        content_mode: form.content_mode,
        request_key: form.request_key,
        password: form.password,
    };
    match service::submit(&state.db, &access, request, identity::now()).await {
        Ok(_) => auth::redirect("/my/data"),
        Err(identity::IdentityError::Validation(m)) => {
            let mut response = index_view(&state, &headers, 0, m.into(), draft).await;
            *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            response
        }
        Err(identity::IdentityError::Conflict(m)) => {
            let message = if m == service::UNSETTLED {
                service::unsettled_message(
                    crate::i18n::current(),
                    &state.frame.settings.site.credit_name,
                )
            } else {
                m.into()
            };
            let mut response = index_view(&state, &headers, 0, message, draft).await;
            *response.status_mut() = StatusCode::CONFLICT;
            response
        }
        Err(e) => failure(&state, e),
    }
}
async fn cancel(
    Page(state): Page,
    headers: HeaderMap,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(_): CsrfForm<FormFields>,
) -> Response {
    let Some(access) = access(&state, &headers) else {
        return auth::redirect("/data/access");
    };
    match service::cancel(&state.db, &access, id, identity::now()).await {
        Ok(()) => auth::redirect("/my/data"),
        Err(e) => failure(&state, e),
    }
}
async fn download(Page(state): Page, headers: HeaderMap, AxumPath(id): AxumPath<i64>) -> Response {
    let Some(access) = access(&state, &headers) else {
        return auth::redirect("/data/access");
    };
    let dir = state.identity.private_directory().join("data-exports");
    let (path, bytes) = match service::download(&state.db, &access, id, &dir, identity::now()).await
    {
        Ok(v) => v,
        Err(e) => return failure(&state, e),
    };
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().governance_error_export_missing(),
            );
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, bytes.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=gonof-data-{id}.zip"),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from_stream(ReaderStream::new(file)))
        .expect("private download")
}
#[derive(Template)]
#[template(path = "privacy/access.html")]
struct AccessTemplate<'a> {
    p: PageMeta<'a>,
    token: String,
    message: String,
}
fn access_view(state: &AppState, token: String, message: &str) -> Response {
    render(&AccessTemplate {
        p: meta(
            state,
            crate::i18n::current().governance_data_access().into(),
            crate::i18n::current()
                .governance_data_access_description()
                .into(),
            "/data/access",
            "",
            "",
        ),
        token,
        message: message.into(),
    })
}
async fn access_page(Page(state): Page, Query(q): Query<QueryFields>) -> Response {
    if !q.token.is_empty() {
        return match service::check_access(&state.db, &q.token, identity::now()).await {
            Ok(()) => access_view(
                &state,
                q.token,
                crate::i18n::current().governance_data_access_confirm_note(),
            ),
            Err(identity::IdentityError::Unauthorized) => access_view(
                &state,
                String::new(),
                crate::i18n::current().governance_data_access_invalid(),
            ),
            Err(e) => failure(&state, e),
        };
    }
    access_view(
        &state,
        String::new(),
        if q.sent {
            crate::i18n::current().governance_data_access_sent()
        } else if q.expired {
            crate::i18n::current().governance_data_access_expired()
        } else {
            ""
        },
    )
}
/// Email tickets for data requests: each normalised email address may request at most 3 times per hour.
const PRIVACY_ACCESS_EMAIL_LIMIT: (u32, i64) = (3, 3600);

async fn access_submit(Page(state): Page, CsrfForm(form): CsrfForm<FormFields>) -> Response {
    match state
        .limit(&[Rule::new(
            "privacy-access",
            &state.viewer.client_ip,
            10,
            3600,
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
    if form.token.is_empty() {
        let start = tokio::time::Instant::now();
        // Each email address may request at most 3 times per hour: someone who knows a frozen member's email can,
        // even by switching IPs, only make that person receive 3 emails per hour. As with password recovery, the key
        // is NFKC-normalised before taking the digest; it comes after the per-IP limit, so requests stopped by the IP
        // limit do not write the email key. Over the limit no email is sent, but the receipt and timing are the same
        // as a normal request, revealing neither whether the email exists nor whether it is frozen.
        let rule = Rule::new(
            "privacy-access-email",
            identity::identifier_limit_key(&form.email),
            PRIVACY_ACCESS_EMAIL_LIMIT.0,
            PRIVACY_ACCESS_EMAIL_LIMIT.1,
        );
        let result = match state.limit(&[rule]).await {
            Ok(true) => service::request_access(&state.db, &form.email, identity::now())
                .await
                .map_err(|e| failure(&state, e)),
            Ok(false) => Ok(()),
            Err(unavailable) => Err(unavailable),
        };
        tokio::time::sleep_until(start + std::time::Duration::from_millis(250)).await;
        return match result {
            Ok(()) => auth::redirect("/data/access?sent=true"),
            Err(response) => response,
        };
    }
    match service::exchange_access(&state.db, &form.token, identity::now()).await {
        Ok((token, seconds)) => {
            let mut r = auth::redirect("/my/data");
            cookie(&mut r, &state, &token, seconds);
            r
        }
        Err(e) => failure(&state, e),
    }
}
async fn exit(
    Page(state): Page,
    headers: HeaderMap,
    CsrfForm(_): CsrfForm<FormFields>,
) -> Response {
    if let Some(raw) = auth::cookie(&headers, "gonof_privacy")
        && let Err(e) = service::exit_access(&state.db, &raw, identity::now()).await
    {
        return failure(&state, e);
    }
    let mut response = auth::redirect("/data/access");
    cookie(&mut response, &state, "", 0);
    response
}
#[derive(Template)]
#[template(path = "privacy/manage.html")]
struct ManageTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<service::DataRequest>,
    next: String,
}
async fn manage(Page(state): Page, Query(q): Query<QueryFields>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login");
    };
    let mut rows = match service::manage(&state.db, session.member.actor(), q.before).await {
        Ok(r) => r,
        Err(e) => return failure(&state, e),
    };
    let next = if rows.len() > 20 {
        rows.truncate(20);
        format!("/manage/data-requests?before={}", rows.last().unwrap().id)
    } else {
        String::new()
    };
    render(&ManageTemplate {
        p: meta(
            &state,
            crate::i18n::current().governance_manage_data().into(),
            crate::i18n::current()
                .governance_manage_data_description()
                .into(),
            "/manage/data-requests",
            "",
            "",
        ),
        rows,
        next,
    })
}
async fn retry(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<FormFields>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login");
    };
    match service::retry(
        &state.db,
        session.member.actor(),
        id,
        &form.reason,
        identity::now(),
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/data-requests"),
        Err(e) => failure(&state, e),
    }
}
