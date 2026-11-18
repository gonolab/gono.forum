use super::extract::{AdminPage, CsrfForm, Page, csrf_forms};
use super::*;
use crate::{administration as admin, runtime};
use axum::{
    extract::{DefaultBodyLimit, Path},
    routing::post,
};

/// Request body limit of the document pages (navigation, policies, mail templates). A form is URL-encoded, so each
/// byte of a non-ASCII text takes three: a 60,000-byte body of Chinese text arrives as about 180 KB, past the 128 KiB
/// of the other admin forms.
const DOCUMENT_BODY_LIMIT: usize = 256 * 1024;

pub(super) fn routes() -> Router<Arc<AppState>> {
    let documents = Router::new()
        .route("/manage/navigation", get(navigation).post(save_navigation))
        .route("/manage/policies/{kind}", get(policy).post(save_policy))
        .route(
            "/manage/mail/templates/{kind}",
            get(mail_template).post(save_mail_template),
        )
        .layer(DefaultBodyLimit::max(DOCUMENT_BODY_LIMIT));
    Router::new()
        .route("/manage", get(overview))
        .route("/manage/users", get(users))
        .route("/manage/users/{uid}", get(user))
        .route("/manage/users/{uid}/api/revoke", post(revoke_api_key))
        .route("/manage/registration-sources", get(registration_sources))
        .route("/manage/invites", get(invites).post(create_invites))
        .route("/manage/invites/{id}", post(change_invite))
        .route("/manage/tasks", get(tasks))
        .route("/manage/tasks/{id}", get(task))
        .route("/manage/tasks/{id}/retry", post(retry_task))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .merge(documents)
}
fn actor(state: &AppState) -> crate::identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .expect("admin checked")
        .member
        .actor()
}
fn pages(path: &str, q: &admin::ListQuery, total: i64) -> (String, String) {
    let href = |page: i64| {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        params.append_pair("page", &page.to_string());
        if !q.q.is_empty() {
            params.append_pair("q", &q.q);
        }
        if !q.status.is_empty() {
            params.append_pair("status", &q.status);
        }
        if let Some(uid) = q.uid {
            params.append_pair("uid", &uid.to_string());
        }
        if let Some(role) = q.role {
            params.append_pair("role", &role.to_string());
        }
        format!("{path}?{}", params.finish())
    };
    (
        if q.page() > 1 {
            href(q.page() - 1)
        } else {
            String::new()
        },
        if q.page() * 20 < total {
            href(q.page() + 1)
        } else {
            String::new()
        },
    )
}
#[derive(Template)]
#[template(path = "manage/overview.html")]
struct Overview<'a> {
    p: PageMeta<'a>,
    counts: Vec<(&'static str, i64)>,
}
async fn overview(Page(state): Page) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage");
    };
    let role = match crate::manage::staff(&state.db, session.member.actor()).await {
        Ok(r) => r,
        Err(e) => return auth::identity_error(&state, e),
    };
    let mut counts = Vec::new();
    let uid = session.member.uid;
    // Reviewers only see pending site-wide reports (docs/roles.md, "Reviewers").
    if role == crate::identity::ROLE_REVIEWER {
        match crate::db::query_scalar::<i64>(
            "SELECT COUNT(*) FROM gonof_common_report WHERE status='pending'",
        )
        .fetch_one(state.db.executor())
        .await
        {
            Ok(n) => counts.push((crate::i18n::current().manage_count_reports(), n)),
            Err(e) => return database_error(&state, e.into()),
        }
        return render(&Overview {
            p: meta(
                &state,
                crate::i18n::current().manage_overview_title().into(),
                crate::i18n::current().manage_overview_description().into(),
                "/manage",
                "",
                "",
            ),
            counts,
        });
    }
    let scope = if crate::identity::forum_role(role) {
        // Moderators and trainee moderators manage the forums they are appointed to and their child forums
        // (docs/roles.md, "Moderators and boards").
        format!(
            " WHERE {}",
            crate::organization::moderates_fid_sql(&uid.to_string(), "t.fid")
        )
    } else {
        String::new()
    };
    let queries = vec![
        (
            crate::i18n::current().manage_count_threads(),
            format!("SELECT COUNT(*) FROM gonof_forum_thread t{scope}"),
        ),
        (
            crate::i18n::current().manage_count_replies(),
            format!(
                "SELECT COUNT(*) FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid{}p.first=0",
                if scope.is_empty() {
                    " WHERE ".to_owned()
                } else {
                    format!("{scope} AND ")
                }
            ),
        ),
    ];
    for (label, sql) in queries {
        match crate::db::query_scalar::<i64>(&sql)
            .fetch_one(state.db.executor())
            .await
        {
            Ok(n) => counts.push((label, n)),
            Err(e) => return database_error(&state, e.into()),
        }
    }
    if role == 1 {
        for (label, sql) in [
            (
                crate::i18n::current().manage_count_members(),
                "SELECT COUNT(*) FROM gonof_common_member WHERE account_state='active'",
            ),
            (
                crate::i18n::current().manage_count_reports(),
                "SELECT COUNT(*) FROM gonof_common_report WHERE status='pending'",
            ),
            (
                crate::i18n::current().manage_count_appeals(),
                "SELECT COUNT(*) FROM gonof_common_member_appeal WHERE status='pending'",
            ),
            (
                crate::i18n::current().manage_count_tasks(),
                "SELECT COUNT(*) FROM gonof_common_outbox WHERE status IN ('pending','processing')",
            ),
            (
                crate::i18n::current().manage_count_data_requests(),
                "SELECT COUNT(*) FROM gonof_common_data_request WHERE status IN ('pending','cooling','processing','failed')",
            ),
        ] {
            match crate::db::query_scalar::<i64>(sql)
                .fetch_one(state.db.executor())
                .await
            {
                Ok(n) => counts.push((label, n)),
                Err(e) => return database_error(&state, e.into()),
            }
        }
    }
    render(&Overview {
        p: meta(
            &state,
            crate::i18n::current().manage_overview_title().into(),
            crate::i18n::current().manage_overview_description().into(),
            "/manage",
            "",
            "",
        ),
        counts,
    })
}

#[derive(Template)]
#[template(path = "manage/users.html")]
struct Users<'a> {
    p: PageMeta<'a>,
    rows: Vec<admin::User>,
    q: admin::ListQuery,
    total: i64,
    previous: String,
    next: String,
    api_key: Option<AdminKeyView>,
}
/// API token status shown on the member detail page; only loaded when viewing a single member by UID.
struct AdminKeyView {
    hint: String,
    created: String,
    expires: String,
    last_used: String,
    last_used_ip: String,
    active: bool,
}
impl From<crate::identity::ApiKey> for AdminKeyView {
    fn from(key: crate::identity::ApiKey) -> Self {
        Self {
            active: key.active_at(crate::identity::now()),
            hint: key.hint,
            created: admin::local_time(key.dateline),
            expires: admin::local_time(key.expires_at),
            last_used: admin::local_time(key.last_used_at),
            last_used_ip: key.last_used_ip,
        }
    }
}
#[derive(Deserialize)]
struct RevokeForm {
    _csrf: String,
    #[serde(default)]
    reason: String,
}
csrf_forms!(RevokeForm);
async fn revoke_api_key(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    CsrfForm(form): CsrfForm<RevokeForm>,
) -> Response {
    let reason = match admin::reason(&form.reason) {
        Ok(reason) => reason,
        Err(e) => return auth::identity_error(&state, e),
    };
    match state
        .identity
        .revoke_member_api_key(actor(&state), uid, reason)
        .await
    {
        Ok(_) => auth::redirect(&format!("/manage/users/{uid}")),
        Err(e) => auth::identity_error(&state, e),
    }
}
async fn users(AdminPage(state): AdminPage, Query(q): Query<admin::ListQuery>) -> Response {
    users_view(&state, q).await
}
async fn user(AdminPage(state): AdminPage, Path(uid): Path<i64>) -> Response {
    users_view(
        &state,
        admin::ListQuery {
            uid: Some(uid),
            ..Default::default()
        },
    )
    .await
}
async fn users_view(state: &AppState, q: admin::ListQuery) -> Response {
    let (rows, total) = match admin::users(&state.db, actor(state), &q).await {
        Ok(found) => found,
        Err(e) => return auth::identity_error(state, e),
    };
    if q.uid.is_some() && total == 0 {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_manage_error_no_member(),
        );
    }
    let api_key = match q.uid {
        Some(uid) => match state.identity.api_key(uid).await {
            Ok(key) => key.map(AdminKeyView::from),
            Err(e) => return auth::identity_error(state, e),
        },
        None => None,
    };
    let (previous, next) = pages("/manage/users", &q, total);
    render(&Users {
        p: meta(
            state,
            crate::i18n::current().account_manage_users().into(),
            crate::i18n::current()
                .account_manage_users_description()
                .into(),
            "/manage/users",
            "",
            "",
        ),
        rows,
        q,
        total,
        previous,
        next,
        api_key,
    })
}

#[derive(Deserialize)]
#[serde(default)]
struct SourcePages {
    site_page: i64,
    channel_page: i64,
    inviter_page: i64,
    prefix_page: i64,
}

impl Default for SourcePages {
    fn default() -> Self {
        Self {
            site_page: 1,
            channel_page: 1,
            inviter_page: 1,
            prefix_page: 1,
        }
    }
}

impl SourcePages {
    fn href(&self, section: &str, page: i64) -> String {
        let selected = |name, current| if section == name { page } else { current };
        format!(
            "/manage/registration-sources?site_page={}&channel_page={}&inviter_page={}&prefix_page={}#{section}",
            selected("sites", self.site_page),
            selected("channels", self.channel_page),
            selected("inviters", self.inviter_page),
            selected("prefixes", self.prefix_page),
        )
    }
}

#[derive(Template)]
#[template(path = "manage/registration_sources.html")]
struct RegistrationSources<'a> {
    p: PageMeta<'a>,
    report: crate::registration_sources::Report,
    pages: SourcePages,
}

impl RegistrationSources<'_> {
    /// The example promotion link in the channel section's text (a `*_html` slot).
    fn channel_example(&self) -> crate::i18n::Markup {
        crate::i18n::Markup::tag(
            "code",
            format!("{}/register?utm_source=wechat", self.p.frame.site_url),
        )
    }
}

async fn registration_sources(
    AdminPage(state): AdminPage,
    Query(pages): Query<SourcePages>,
) -> Response {
    let report = match crate::registration_sources::counts(
        &state.db,
        actor(&state),
        pages.site_page,
        pages.channel_page,
        pages.inviter_page,
        pages.prefix_page,
    )
    .await
    {
        Ok(report) => report,
        Err(error) => return auth::identity_error(&state, error),
    };
    render(&RegistrationSources {
        p: meta(
            &state,
            crate::i18n::current().account_manage_sources().into(),
            crate::i18n::current()
                .account_manage_sources_description()
                .into(),
            "/manage/registration-sources",
            "",
            "",
        ),
        report,
        pages,
    })
}

#[derive(Template)]
#[template(path = "manage/invites.html")]
struct Invites<'a> {
    p: PageMeta<'a>,
    rows: Vec<admin::Invite>,
    q: admin::ListQuery,
    total: i64,
    previous: String,
    next: String,
    key: String,
    message: String,
    reason: String,
    count: i64,
}
async fn invite_view(
    state: &AppState,
    q: admin::ListQuery,
    message: String,
    reason: String,
    count: i64,
    key: String,
    status: StatusCode,
) -> Response {
    match admin::invites(&state.db, actor(state), &q).await {
        Ok((rows, total)) => {
            let (previous, next) = pages("/manage/invites", &q, total);
            let mut response = render(&Invites {
                p: meta(
                    state,
                    crate::i18n::current().account_manage_invites().into(),
                    crate::i18n::current()
                        .account_manage_invites_description()
                        .into(),
                    "/manage/invites",
                    "",
                    "",
                ),
                rows,
                q,
                total,
                previous,
                next,
                key,
                message,
                reason,
                count,
            });
            *response.status_mut() = status;
            response
        }
        Err(e) => auth::identity_error(state, e),
    }
}
async fn invites(AdminPage(state): AdminPage, Query(q): Query<admin::ListQuery>) -> Response {
    invite_view(
        &state,
        q,
        String::new(),
        String::new(),
        1,
        crate::identity::random_token(),
        StatusCode::OK,
    )
    .await
}
#[derive(Deserialize)]
struct BatchForm {
    _csrf: String,
    request_key: String,
    count: i64,
    reason: String,
}
csrf_forms!(BatchForm);
async fn create_invites(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<BatchForm>,
) -> Response {
    match admin::create_invites(
        &state.db,
        actor(&state),
        form.count,
        &form.request_key,
        &form.reason,
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/invites"),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => {
                invite_view(
                    &state,
                    Default::default(),
                    message,
                    form.reason,
                    form.count,
                    form.request_key,
                    status,
                )
                .await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}
#[derive(Deserialize)]
struct InviteForm {
    _csrf: String,
    expected: String,
    desired: String,
    request_key: String,
}
csrf_forms!(InviteForm);
async fn change_invite(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<InviteForm>,
) -> Response {
    match admin::change_invite(
        &state.db,
        actor(&state),
        id,
        &form.expected,
        &form.desired,
        &form.request_key,
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/invites"),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => {
                invite_view(
                    &state,
                    Default::default(),
                    message,
                    String::new(),
                    1,
                    crate::identity::random_token(),
                    status,
                )
                .await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

#[derive(Template)]
#[template(path = "manage/tasks.html")]
struct Tasks<'a> {
    p: PageMeta<'a>,
    rows: Vec<admin::Task>,
    q: admin::ListQuery,
    total: i64,
    previous: String,
    next: String,
    message: String,
    reason: String,
}
async fn task_view(
    state: &AppState,
    q: admin::ListQuery,
    message: String,
    reason: String,
    status: StatusCode,
) -> Response {
    match admin::tasks(&state.db, actor(state), &q).await {
        Ok((rows, total)) => {
            if q.uid.is_some() && total == 0 {
                return error_page(
                    state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().maintenance_tasks_error_not_found(),
                );
            }
            let (previous, next) = pages("/manage/tasks", &q, total);
            let mut response = render(&Tasks {
                p: meta(
                    state,
                    crate::i18n::current().maintenance_tasks_title().into(),
                    crate::i18n::current()
                        .maintenance_tasks_description()
                        .into(),
                    "/manage/tasks",
                    "",
                    "",
                ),
                rows,
                q,
                total,
                previous,
                next,
                message,
                reason,
            });
            *response.status_mut() = status;
            response
        }
        Err(e) => auth::identity_error(state, e),
    }
}
async fn tasks(AdminPage(state): AdminPage, Query(q): Query<admin::ListQuery>) -> Response {
    task_view(&state, q, String::new(), String::new(), StatusCode::OK).await
}
async fn task(AdminPage(state): AdminPage, Path(id): Path<i64>) -> Response {
    task_view(
        &state,
        admin::ListQuery {
            uid: Some(id),
            ..Default::default()
        },
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}
#[derive(Deserialize)]
struct RetryForm {
    _csrf: String,
    attempts: i64,
    available_at: i64,
    lease_until: i64,
    expected: String,
    reason: String,
}
csrf_forms!(RetryForm);
async fn retry_task(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<RetryForm>,
) -> Response {
    match admin::retry_task(
        &state.db,
        actor(&state),
        id,
        form.attempts,
        form.available_at,
        form.lease_until,
        &form.expected,
        &form.reason,
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("/manage/tasks/{id}")),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => {
                task_view(
                    &state,
                    admin::ListQuery {
                        uid: Some(id),
                        ..Default::default()
                    },
                    message,
                    form.reason,
                    status,
                )
                .await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

/// Whether a failed form save should refill and re-render the form, or go to the shared error page.
///
/// Only validation failures and version conflicts are "fixable by the user": the form is refilled with the matching
/// status code; missing permission, missing records and database failures go to the error page. Showing the text of
/// `IdentityError::Internal` as a form hint would put the anyhow error chain on the page, which is both misleading
/// and should not be exposed.
fn form_failure(
    error: crate::identity::IdentityError,
) -> Result<(StatusCode, String), crate::identity::IdentityError> {
    use crate::identity::IdentityError;
    match error {
        IdentityError::Validation(message) => {
            Ok((StatusCode::UNPROCESSABLE_ENTITY, message.to_owned()))
        }
        IdentityError::Conflict(message) => Ok((StatusCode::CONFLICT, message.to_owned())),
        other => Err(other),
    }
}

/// `?lang=` of the per-language edit pages (docs/architecture.md, "Languages"); absent means the site language.
#[derive(Default, Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

/// The language a mail template or policy page edits: `?lang=`, or the site language; `None` for a tag that is not
/// compiled in (the page answers 404).
fn edit_language(state: &AppState, query: &LangQuery) -> Option<crate::i18n::Lang> {
    match &query.lang {
        Some(tag) => crate::i18n::Lang::parse(tag),
        None => Some(state.frame.settings.site.lang()),
    }
}

/// `?lang=…` for links and forms of an edit page; empty for the site language (the address without it).
fn lang_query(state: &AppState, lang: crate::i18n::Lang) -> String {
    if lang == state.frame.settings.site.lang() {
        String::new()
    } else {
        format!("?lang={}", lang.tag())
    }
}

/// `?lang=…` of the form on an edit page, always naming the edited language: a form read while that language was the
/// site language must not save into another one after the site language changes.
fn form_query(lang: crate::i18n::Lang) -> String {
    format!("?lang={}", lang.tag())
}

/// Where saving returns to: the same page in the same language.
fn edit_address(state: &AppState, path: String, lang: Option<crate::i18n::Lang>) -> String {
    match lang {
        Some(lang) => format!("{path}{}", lang_query(state, lang)),
        None => path,
    }
}

/// One tab of the language switch on the edit pages.
struct LanguageTab {
    tag: &'static str,
    name: &'static str,
    current: bool,
    customized: bool,
}

/// The language tabs of one kind (`mail.…` or `policy.…`): every compiled-in language, the current one marked, and
/// whether each already has its own text. The site language comes first.
async fn language_tabs(
    state: &AppState,
    kind: &str,
    current: crate::i18n::Lang,
) -> anyhow::Result<Vec<LanguageTab>> {
    let rows = runtime::localized_rows(&state.db)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let site = state.frame.settings.site.lang();
    let mut languages: Vec<crate::i18n::Lang> = crate::i18n::Lang::all().collect();
    languages.sort_by_key(|lang| *lang != site);
    Ok(languages
        .into_iter()
        .map(|lang| LanguageTab {
            tag: lang.tag(),
            name: lang.name(),
            current: lang == current,
            customized: rows
                .iter()
                .any(|row| row.kind == kind && row.language == lang),
        })
        .collect())
}

#[derive(Template)]
#[template(path = "manage/document.html")]
struct Document<'a> {
    p: PageMeta<'a>,
    document: runtime::Document,
    path: String,
    /// Policies: `?lang=…` of the edited language for the form (empty for the navigation document) and the language
    /// switch (empty for the navigation document).
    form_query: String,
    languages: Vec<LanguageTab>,
    /// Policies: the built-in title and body in the edited language, shown as placeholders and used while both
    /// fields are blank; empty for the navigation document, which has no built-in text.
    builtin: (String, String),
    /// Body limit in UTF-8 bytes, checked by the browser too (`maxlength` counts characters).
    max_bytes: usize,
    heading: String,
    help: String,
    message: String,
    /// Navigation document items that would not be shown right now (they reference a disabled or admin-only forum,
    /// or one whose parent forum is).
    hidden: Vec<String>,
}

/// Navigation document items that reference disabled or admin-only forums (a child forum is just as unreadable when
/// its parent forum is disabled or admin-only). Page header rendering silently drops unreadable forums; this only
/// lets administrators see them on the edit page, and saving still works.
async fn hidden_navigation(
    state: &AppState,
    document: &runtime::Document,
) -> anyhow::Result<Vec<String>> {
    let Ok(items) = runtime::parse_navigation(&document.body) else {
        return Ok(Vec::new());
    };
    let fids: Vec<i64> = items
        .iter()
        .filter_map(|item| item.target.strip_prefix("forum:")?.parse().ok())
        .collect();
    if fids.is_empty() {
        return Ok(Vec::new());
    }
    let Some(session) = &state.viewer.session else {
        return Ok(Vec::new());
    };
    // Administrators can read every forum (including disabled and admin-only ones).
    let forums = crate::organization::managed_forums(&state.db, session.member.actor())
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let open = |forum: &crate::organization::ForumRecord| {
        forum.status == 1 && forum.visibility != "private"
    };
    let mut hidden = Vec::new();
    for item in &items {
        let Some(fid) = item
            .target
            .strip_prefix("forum:")
            .and_then(|fid| fid.parse::<i64>().ok())
        else {
            continue;
        };
        let Some(forum) = forums.iter().find(|forum| forum.fid == fid) else {
            continue;
        };
        let parent = forum
            .parent_fid
            .and_then(|parent| forums.iter().find(|candidate| candidate.fid == parent));
        let t = crate::i18n::current();
        let reason = if forum.status == 0 {
            t.policy_manage_hidden_disabled()
        } else if forum.visibility == "private" {
            t.policy_manage_hidden_private()
        } else if parent.is_some_and(|parent| !open(parent)) {
            t.policy_manage_hidden_parent()
        } else {
            continue;
        };
        hidden.push(t.policy_manage_hidden_item(item.label.as_str(), item.target.as_str(), reason));
    }
    Ok(hidden)
}
fn document_view(
    state: &AppState,
    document: runtime::Document,
    path: String,
    kind: &str,
    (lang, languages): (Option<crate::i18n::Lang>, Vec<LanguageTab>),
    (message, hidden): (String, Vec<String>),
    status: StatusCode,
) -> Response {
    let t = crate::i18n::current();
    let (heading, help) = if kind == "navigation" {
        (
            t.policy_manage_navigation(),
            t.policy_manage_navigation_help(),
        )
    } else if kind == "policy.verified" {
        (
            t.manage_tab_policy_verified(),
            t.policy_manage_verified_help(),
        )
    } else if kind == "policy.links" {
        (t.manage_tab_policy_links(), t.policy_manage_links_help())
    } else {
        (t.policy_manage_heading(), t.policy_manage_help())
    };
    let builtin = match (lang, kind.strip_prefix("policy.")) {
        (Some(lang), Some(policy)) => {
            super::site::builtin_policy(policy, lang, &state.frame.settings)
        }
        _ => Default::default(),
    };
    let mut response = render(&Document {
        p: meta(state, heading.into(), heading.into(), &path, "", ""),
        document,
        path,
        form_query: lang.map(form_query).unwrap_or_default(),
        languages,
        builtin,
        max_bytes: 60_000,
        heading: heading.into(),
        help: help.into(),
        message,
        hidden,
    });
    *response.status_mut() = status;
    response
}
async fn document_get(
    state: &AppState,
    kind: &str,
    path: String,
    lang: Option<crate::i18n::Lang>,
) -> Response {
    let (read, languages) = match lang {
        Some(lang) => (
            runtime::document_in(&state.db, kind, lang).await,
            match language_tabs(state, kind, lang).await {
                Ok(tabs) => (Some(lang), tabs),
                Err(error) => return database_error(state, error),
            },
        ),
        None => (runtime::document(&state.db, kind).await, Default::default()),
    };
    match read {
        Ok(doc) => {
            let hidden = if kind == "navigation" {
                match hidden_navigation(state, &doc).await {
                    Ok(hidden) => hidden,
                    Err(error) => return database_error(state, error),
                }
            } else {
                Vec::new()
            };
            document_view(
                state,
                doc,
                path,
                kind,
                languages,
                (String::new(), hidden),
                StatusCode::OK,
            )
        }
        Err(e) => auth::identity_error(state, e),
    }
}
async fn navigation(AdminPage(state): AdminPage) -> Response {
    document_get(&state, "navigation", "/manage/navigation".into(), None).await
}
async fn policy(
    AdminPage(state): AdminPage,
    Path(kind): Path<String>,
    Query(query): Query<LangQuery>,
) -> Response {
    let lang = edit_language(&state, &query);
    if !matches!(
        kind.as_str(),
        "terms" | "privacy" | "rules" | "contact" | "verified" | "links"
    ) || lang.is_none()
    {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().policy_manage_not_found(),
        );
    }
    document_get(
        &state,
        &format!("policy.{kind}"),
        format!("/manage/policies/{kind}"),
        lang,
    )
    .await
}
#[derive(Deserialize)]
struct DocumentForm {
    _csrf: String,
    title: String,
    body: String,
    /// `reset` on a policy page: "Restore built-in text", the same as saving a blank title and body.
    #[serde(default)]
    operation: String,
}
csrf_forms!(DocumentForm);
async fn document_post(
    state: &AppState,
    form: DocumentForm,
    kind: &str,
    path: String,
    lang: Option<crate::i18n::Lang>,
) -> Response {
    let candidate = if form.operation == "reset" && lang.is_some() {
        runtime::Document::default()
    } else {
        runtime::Document {
            title: form.title,
            body: runtime::form_lines(&form.body),
            updated_at: 0,
        }
    };
    let saved = match lang {
        Some(lang) => {
            runtime::save_document_in(
                &state.db,
                &state.global,
                actor(state),
                kind,
                lang,
                candidate.clone(),
            )
            .await
        }
        None => {
            runtime::save_document(
                &state.db,
                &state.global,
                actor(state),
                kind,
                candidate.clone(),
            )
            .await
        }
    };
    match saved {
        Ok(_) => auth::redirect(&edit_address(state, path, lang)),
        // Without the optimistic lock this can no longer be a version conflict; a malformed navigation line is a
        // validation failure, so 422.
        Err(error) => match form_failure(error) {
            Ok((status, message)) => {
                let languages = match lang {
                    Some(lang) => match language_tabs(state, kind, lang).await {
                        Ok(tabs) => (Some(lang), tabs),
                        Err(error) => return database_error(state, error),
                    },
                    None => Default::default(),
                };
                document_view(
                    state,
                    candidate,
                    path,
                    kind,
                    languages,
                    (message, Vec::new()),
                    status,
                )
            }
            Err(other) => auth::identity_error(state, other),
        },
    }
}
async fn save_navigation(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<DocumentForm>,
) -> Response {
    document_post(
        &state,
        form,
        "navigation",
        "/manage/navigation".into(),
        None,
    )
    .await
}
async fn save_policy(
    AdminPage(state): AdminPage,
    Path(kind): Path<String>,
    Query(query): Query<LangQuery>,
    CsrfForm(form): CsrfForm<DocumentForm>,
) -> Response {
    let Some(lang) = edit_language(&state, &query) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().policy_manage_not_found(),
        );
    };
    document_post(
        &state,
        form,
        &format!("policy.{kind}"),
        format!("/manage/policies/{kind}"),
        Some(lang),
    )
    .await
}

#[derive(Template)]
#[template(path = "manage/mail_template.html")]
struct MailTemplatePage<'a> {
    p: PageMeta<'a>,
    value: crate::mail::templates::MailTemplate,
    kind: String,
    /// `?lang=…` of the edited language, empty for the site language, for the links to the other kinds; the form
    /// always carries the language ([`form_query`]); and the language switch (docs/architecture.md, "Languages").
    lang_query: String,
    form_query: String,
    languages: Vec<LanguageTab>,
    /// Every template kind and its name in this request's language.
    kinds: Vec<(&'static str, &'static str)>,
    variables: String,
    message: String,
    preview_subject: String,
    preview_body: String,
}
async fn mail_template_view(
    state: &AppState,
    (kind, lang): (String, crate::i18n::Lang),
    value: crate::mail::templates::MailTemplate,
    message: String,
    preview: (String, String),
    status: StatusCode,
) -> Response {
    let languages = match language_tabs(state, &format!("mail.{kind}"), lang).await {
        Ok(tabs) => tabs,
        Err(error) => return database_error(state, error),
    };
    let variables = crate::mail::templates::variables(&kind)
        .join(crate::i18n::current().common_list_separator());
    let mut response = render(&MailTemplatePage {
        p: meta(
            state,
            crate::i18n::current().mail_templates_title().into(),
            crate::i18n::current().mail_templates_description().into(),
            &format!("/manage/mail/templates/{kind}"),
            "",
            "",
        ),
        value,
        kind,
        lang_query: lang_query(state, lang),
        form_query: form_query(lang),
        languages,
        kinds: crate::mail::templates::KINDS
            .iter()
            .map(|(name, label)| (*name, label(crate::i18n::current())))
            .collect(),
        variables,
        message,
        preview_subject: preview.0,
        preview_body: preview.1,
    });
    *response.status_mut() = status;
    response
}
async fn mail_template(
    AdminPage(state): AdminPage,
    Path(kind): Path<String>,
    Query(query): Query<LangQuery>,
) -> Response {
    let Some(lang) = edit_language(&state, &query) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().mail_templates_error_not_found(),
        );
    };
    match crate::mail::templates::current_in(&state.db, &kind, lang).await {
        Ok(value) => {
            mail_template_view(
                &state,
                (kind, lang),
                value,
                String::new(),
                Default::default(),
                StatusCode::OK,
            )
            .await
        }
        Err(_) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().mail_templates_error_not_found(),
        ),
    }
}
#[derive(Deserialize)]
struct MailTemplateForm {
    _csrf: String,
    subject: String,
    body: String,
    operation: String,
}
csrf_forms!(MailTemplateForm);
async fn save_mail_template(
    AdminPage(state): AdminPage,
    Path(kind): Path<String>,
    Query(query): Query<LangQuery>,
    CsrfForm(form): CsrfForm<MailTemplateForm>,
) -> Response {
    let Some(lang) = edit_language(&state, &query) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().mail_templates_error_not_found(),
        );
    };
    let value = crate::mail::templates::MailTemplate {
        subject: form.subject,
        body: runtime::form_lines(&form.body),
    };
    if form.operation == "preview" {
        // The sample values and the sample time are written in the language being edited.
        let format = crate::clock::TimeFormat {
            lang,
            ..crate::clock::current()
        };
        return match crate::mail::templates::preview(&kind, &value, state.site_url(), &format) {
            Ok(preview) => {
                mail_template_view(
                    &state,
                    (kind, lang),
                    value,
                    crate::i18n::current()
                        .mail_templates_preview_unsaved()
                        .into(),
                    preview,
                    StatusCode::OK,
                )
                .await
            }
            Err(e) => {
                mail_template_view(
                    &state,
                    (kind, lang),
                    value,
                    e.to_string(),
                    Default::default(),
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await
            }
        };
    }
    if !matches!(form.operation.as_str(), "save" | "reset") {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().mail_templates_error_operation(),
        );
    }
    if !crate::mail::templates::KINDS
        .iter()
        .any(|(value, _)| *value == kind)
    {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().mail_templates_error_not_found(),
        );
    }
    // Hints from text validation are written for administrators and stay in the form; the remaining failures on save
    // are permission or database problems and, as with `form_failure`, go to the shared error page (with status code,
    // logging and redaction).
    if form.operation == "save"
        && let Err(error) = crate::mail::templates::validate(&kind, &value)
    {
        return mail_template_view(
            &state,
            (kind, lang),
            value,
            error.to_string(),
            Default::default(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    match crate::mail::templates::save_in(
        &state.db,
        actor(&state),
        &kind,
        lang,
        value.clone(),
        form.operation == "reset",
    )
    .await
    {
        Ok(_) => auth::redirect(&edit_address(
            &state,
            format!("/manage/mail/templates/{kind}"),
            Some(lang),
        )),
        Err(error) => match error.downcast::<crate::identity::IdentityError>() {
            Ok(identity) => auth::identity_error(&state, identity),
            Err(error) => database_error(&state, error),
        },
    }
}
