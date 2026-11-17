//! Posting forms and HTML endpoints share one transaction service.
use super::extract::{CheckedForm, Page, csrf_forms};
use super::*;
use crate::{
    content::writes::{
        ContentWriteError, ContentWriter, NewReply, NewThread, Published, ThreadVisibility,
    },
    identity, markdown,
};
use axum::{
    extract::{DefaultBodyLimit, Form, Path as AxumPath},
    http::HeaderMap,
    routing::post,
};

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Editor {
    pub _csrf: String,
    #[serde(deserialize_with = "empty_number")]
    pub category: i64,
    #[serde(deserialize_with = "empty_number")]
    pub budget: i64,
    #[serde(deserialize_with = "empty_number")]
    pub shares: i64,
    pub distribution: String,
    pub deadline: String,
    #[serde(deserialize_with = "empty_number")]
    pub max_choices: i64,
    pub options: String,
    pub source_name: String,
    pub source_url: String,
    pub source_author: String,
    /// Lottery entry mode: `reply` (default) or `click`.
    pub entry_mode: String,
    /// Entry requirements of a one-click lottery, one per line.
    pub requirements: String,
    /// Checkbox: the author reviews entries before the draw.
    pub review: String,
    /// Checkbox: allows the author to draw early.
    pub early_draw: String,
    /// Lottery prizes, one per line (see `activities::parse_prize_lines`); when empty, the budget and share count
    /// describe a single credit prize.
    pub prizes: String,
    /// Redemption codes, one per line. Not kept in the browser's local draft (`ACTIVITY_FIELDS` in `composer.js`).
    pub lottery_codes: String,
    /// The thread's visitor scope: `public` or `members`. Empty means public for a new thread; when editing, empty
    /// keeps the original value.
    pub visibility: String,
    pub title: String,
    pub body: String,
    #[serde(deserialize_with = "empty_number")]
    pub fid: i64,
    pub tid: i64,
    pub root_pid: i64,
    pub reply_to_pid: i64,
    pub quote_pid: i64,
    pub request_key: String,
    pub view: String,
    pub location: String,
    pub return_to: String,
    pub attachment_ids: String,
    pub edit_pid: i64,
    pub edit_thread: bool,
    pub expected_post_version: i64,
    pub expected_thread_version: i64,
    pub reason: String,
    #[serde(skip)]
    pub management: bool,
    #[serde(skip)]
    pub preserve_input: bool,
    #[serde(skip)]
    pub current_body: String,
    #[serde(skip)]
    pub current_title: String,
    #[serde(skip)]
    pub action: String,
    #[serde(skip)]
    pub draft: String,
    #[serde(skip)]
    pub message: String,
    #[serde(skip)]
    pub preview: String,
}
csrf_forms!(Editor);
fn empty_number<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Ok(0)
    } else {
        value.parse().map_err(serde::de::Error::custom)
    }
}
impl Editor {
    pub fn heading(&self) -> &'static str {
        if self.edit_pid > 0 {
            if self.edit_thread {
                crate::i18n::current().editor_heading_edit_thread()
            } else {
                crate::i18n::current().editor_heading_edit_reply()
            }
        } else if self.tid == 0 {
            crate::i18n::current().editor_heading_new_thread()
        } else {
            crate::i18n::current().editor_heading_new_reply()
        }
    }
    pub fn return_query(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("return_to", &auth::safe_return(&self.return_to));
        query.finish()
    }
    pub fn prepare(&mut self) {
        if self.max_choices == 0 {
            self.max_choices = 1;
        }
        if self.distribution.is_empty() {
            self.distribution = "equal".into();
        }
        if self.request_key.is_empty() {
            self.request_key = identity::random_token();
        }
        self.action = if self.edit_pid > 0 {
            if self.edit_thread {
                format!("/edit/thread-{}", self.tid)
            } else {
                format!("/edit/post-{}", self.edit_pid)
            }
        } else if self.tid == 0 {
            "/new".into()
        } else if self.root_pid > 0 {
            format!("/api/posts/{}/sub-comments", self.root_pid)
        } else {
            format!("/api/thread-{}/comments", self.tid)
        };
        self.draft = if self.edit_pid > 0 {
            format!("edit-post-{}", self.edit_pid)
        } else if self.tid == 0 {
            "topic-new".into()
        } else if self.quote_pid > 0 {
            format!(
                "reply-{}-{}-quote-{}",
                self.tid, self.reply_to_pid, self.quote_pid
            )
        } else {
            format!("reply-{}-{}", self.tid, self.reply_to_pid)
        };
        if self.return_to.is_empty() {
            self.return_to = if self.tid == 0 {
                "/new".into()
            } else {
                self.location.clone()
            };
        }
    }
}
#[derive(Clone)]
pub(super) struct ForumChoice {
    pub fid: i64,
    pub name: String,
    pub slug: String,
    /// This forum or its parent forum is members-only: the thread, following the forum, is shown only to logged-in
    /// members.
    pub members_only: bool,
    /// Child forum: its name gets an indentation prefix in the drop-down.
    pub child: bool,
    /// Posting guidelines, plain text by line (empty when not filled in).
    pub guidance: Vec<String>,
}
#[derive(Template)]
#[template(path = "editor/page.html")]
struct EditorTemplate<'a> {
    p: PageMeta<'a>,
    e: &'a Editor,
    forums: Vec<ForumChoice>,
    attachments: Vec<crate::media::Upload>,
    /// When editing a thread whose forum is already stricter than "Public", no visibility options are offered; only
    /// this note is shown.
    forum_scope_note: &'static str,
}

/// Visibility in the form. Empty means public for a new thread; when editing, empty means keep the original value.
pub(super) fn submitted_visibility(
    value: &str,
) -> Result<Option<ThreadVisibility>, ContentWriteError> {
    if value.is_empty() {
        return Ok(None);
    }
    ThreadVisibility::parse(value)
        .map(Some)
        .ok_or(ContentWriteError::Validation(
            crate::i18n::current().editor_error_visibility(),
        ))
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/new", get(new_page).post(publish))
        .route("/new/{slug}", get(new_forum_page).post(publish))
        .route("/api/{thread}/comments", post(reply))
        .route("/api/posts/{root}/sub-comments", post(sub_reply))
        .route("/api/preview", post(preview))
        .route("/api/mentions", get(mentions))
        .route("/posts/{pid}", get(post_permalink))
        .layer(DefaultBodyLimit::max(256 * 1024))
}

async fn post_permalink(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    AxumPath(pid): AxumPath<i64>,
    headers: HeaderMap,
) -> Response {
    let mut state = (*state).clone();
    state.viewer = viewer;
    let tid: Option<i64> =
        match crate::db::query_scalar("SELECT tid FROM gonof_forum_post WHERE pid=?")
            .bind(pid)
            .fetch_optional(state.db.executor())
            .await
        {
            Ok(value) => value,
            Err(error) => return database_error(&state, error.into()),
        };
    let Some(tid) = tid.filter(|tid| *tid > 0) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_quote(),
        );
    };
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    // The floor to jump to is built from the visitor's default reply view (reading preferences);
    // anonymous visitors and members who never chose follow the site default.
    // This only returns a redirect without rendering a page and does not go through the page frame, so it reads once
    // by primary key itself; the site default and floors per page come from the in-process cache.
    let settings = match state.global.settings(&state.db).await {
        Ok(block) => block.settings.clone(),
        Err(error) => return auth::identity_error(&state, error),
    };
    let flat = match crate::reading::for_viewer(&state.db, viewer_uid, &settings.reading).await {
        Ok(reading) => reading.flat,
        Err(error) => return database_error(&state, error),
    };
    // Readable forums likewise skip the page frame and take this visitor's copy from the in-process forum cache: the
    // thread row has no copy of its forum's visibility.
    let forums = match state
        .global
        .forums(&state.db, viewer_uid.is_some(), settings.cache_ttl())
        .await
    {
        Ok(forums) => forums,
        Err(error) => return database_error(&state, error),
    };
    let fids: Vec<i64> = forums.iter().map(|forum| forum.fid).collect();
    match content::post_location_for_viewer(
        &state.db,
        tid as u64,
        pid,
        flat,
        content::Readable {
            uid: viewer_uid,
            forums: &fids,
        },
        crate::urls::Paging::from_settings(&settings),
    )
    .await
    {
        Ok(Some(location)) if hx(&headers) => {
            ([("HX-Redirect", crate::site::href(location))], "").into_response()
        }
        Ok(Some(location)) => (
            StatusCode::FOUND,
            [(header::LOCATION, crate::site::href(location))],
        )
            .into_response(),
        Ok(None) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_quote(),
        ),
        Err(error) => database_error(&state, error),
    }
}
/// Forum choice for posting: this visitor's readable forums from the page frame (in-process cache,
/// docs/settings-and-caching.md, "The process cache"), with no extra database query. Forums the member blocked
/// (including through blocking the parent forum) are not listed; grouped by parent and child, child forums follow
/// their parent forum with an indentation prefix before the name; parent forums can be posted to as usual.
/// Each item carries its posting guidelines, which the posting page shows for the selected forum.
pub(super) fn choices(state: &AppState) -> Vec<ForumChoice> {
    let frame = &state.frame;
    let open: Vec<&crate::organization::ForumRecord> = frame
        .forums
        .iter()
        .filter(|forum| !frame.forum_filters.is_blocked(forum.fid))
        .collect();
    crate::organization::grouped_by_parent(&open, |forum| (forum.fid, forum.parent_fid))
        .into_iter()
        .map(|(forum, child)| ForumChoice {
            fid: forum.fid,
            name: forum.name.clone(),
            slug: forum.slug.clone(),
            child,
            // The parent forum of a readable forum is necessarily readable too and in the same list; the inherited
            // scope is decided from the parent and child rows.
            members_only: forum.visibility == "members"
                || frame
                    .parent_of(forum)
                    .is_some_and(|parent| parent.visibility == "members"),
            guidance: guidance_lines(&forum.guidance),
        })
        .collect()
}

/// Whether any forum in the drop-down has posting guidelines: if none, the guideline area and the hint are left out.
pub(super) fn guided(forums: &[ForumChoice]) -> bool {
    forums.iter().any(|forum| !forum.guidance.is_empty())
}

/// Posting guidelines are shown line by line as plain text: leading and trailing blank lines are removed, each line is
/// escaped as is, and the template joins lines with `<br>`.
pub(super) fn guidance_lines(guidance: &str) -> Vec<String> {
    let trimmed = guidance.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect()
}
pub(super) fn gate(state: &AppState, destination: &str) -> Option<Response> {
    let base = if state.viewer.session.is_none() {
        "/login"
    } else if state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| !s.member.can_contribute())
    {
        "/verify-email"
    } else {
        return None;
    };
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("return_to", &auth::safe_return(destination));
    let mut response = auth::redirect(&format!("{base}?{}", params.finish()));
    auth::auxiliary_set_cookie(
        &mut response,
        "gonof_return",
        &auth::safe_return(destination),
        3600,
    );
    Some(response)
}
/// Posting and editing pages. The forum drop-down comes from the forum cache in the page frame ([`choices`]), with no
/// database query.
pub(super) async fn editor_page(state: &AppState, e: &Editor, status: StatusCode) -> Response {
    let mut e = e.clone();
    e.management |= !e.reason.is_empty();
    let e = &e;
    let forums = choices(state);
    let mut attachments = Vec::new();
    if let Some(session) = &state.viewer.session {
        let mut ids = markdown::references(&e.body)
            .map(|r| r.attachments)
            .unwrap_or_default();
        ids.extend(
            e.attachment_ids
                .split(',')
                .filter_map(|s| s.parse::<i64>().ok())
                .filter(|i| *i > 0)
                .take(20),
        );
        // Fetches the whole batch at once (`Media::editor_uploads`) rather than querying per attachment. Ones that
        // cannot be fetched are not shown.
        let ids: Vec<i64> = ids.into_iter().collect();
        match state
            .media
            .editor_uploads(
                session.member.actor(),
                &ids,
                (e.edit_pid > 0).then_some(e.edit_pid),
            )
            .await
        {
            Ok(found) => attachments = found,
            Err(error) => tracing::warn!(error = %error, "failed to read editor attachments"),
        }
    }
    // When editing a thread the forum is fixed: not finding it means the forum is not readable for the current account
    // (private or disabled), which is stricter than members-only.
    // When the thread column holds a reserved value (such as `private`), no options are offered either, and editing
    // only keeps the original value.
    let forum_scope_note = if e.edit_thread {
        match forums.iter().find(|forum| forum.fid == e.fid) {
            _ if !["", "public", "members"].contains(&e.visibility.as_str()) => {
                crate::i18n::current().editor_scope_reserved()
            }
            Some(forum) if !forum.members_only => "",
            Some(_) => crate::i18n::current().editor_scope_members(),
            None => crate::i18n::current().editor_scope_closed(),
        }
    } else {
        ""
    };
    let mut response = render(&EditorTemplate {
        // The publish / edit page is the editor as a whole page, so composer.js must be loaded.
        p: meta(
            state,
            e.heading().into(),
            crate::i18n::current().editor_description().into(),
            &e.action,
            "",
            "",
        )
        .with_composer(true),
        e,
        forums,
        attachments,
        forum_scope_note,
    });
    *response.status_mut() = status;
    response
}
async fn new_page(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
) -> Response {
    new_inner(&state, viewer, None).await
}
async fn new_forum_page(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    AxumPath(slug): AxumPath<String>,
) -> Response {
    new_inner(&state, viewer, Some(slug)).await
}
async fn new_inner(state: &AppState, viewer: auth::Viewer, slug: Option<String>) -> Response {
    let state = match auth::page_state(state, viewer).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let path = slug
        .as_ref()
        .map(|s| format!("/new/{s}"))
        .unwrap_or_else(|| "/new".into());
    if let Some(r) = gate(&state, &path) {
        return r;
    }
    // The default deadline is set only on the first GET; POST and preview keep the original value, since the digest of
    // a retried request must not change over time.
    let mut e = Editor {
        deadline: format_deadline(identity::now() + DEFAULT_DEADLINE_SECONDS),
        ..Editor::default()
    };
    if let Some(slug) = slug {
        if let Some(forum) = state
            .frame
            .forum(&slug)
            .filter(|f| state.frame.forum_filters.is_blocked(f.fid))
        {
            return super::forum_filters::forum_blocked_page(&state, forum, &path);
        }
        // Multi-domain sites: forums outside the scope are posted to on their canonical domain.
        if let Some(target) = state
            .frame
            .any_forum(&slug)
            .and_then(|forum| state.frame.elsewhere(forum))
        {
            let uri: axum::http::Uri = path.parse().unwrap_or_default();
            return super::elsewhere_response(&state, target, &uri);
        }
        let Some(forum) = choices(&state).into_iter().find(|f| f.slug == slug) else {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().editor_error_forum(),
            );
        };
        e.fid = forum.fid;
    }
    e.prepare();
    editor_page(&state, &e, StatusCode::OK).await
}
pub(super) fn write_error(error: ContentWriteError) -> (StatusCode, String) {
    match error {
        ContentWriteError::Validation(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.into()),
        ContentWriteError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().editor_error_sign_in().into(),
        ),
        ContentWriteError::Forbidden(m) => (StatusCode::FORBIDDEN, m.into()),
        ContentWriteError::NotFound => (
            StatusCode::NOT_FOUND,
            crate::i18n::current().editor_error_target().into(),
        ),
        ContentWriteError::Conflict(m) => (StatusCode::CONFLICT, m.into()),
        ContentWriteError::Quota(m) => (StatusCode::TOO_MANY_REQUESTS, m.into()),
        ContentWriteError::Restricted(m) => (StatusCode::FORBIDDEN, m),
        ContentWriteError::Internal(error) => {
            // Committed but other processes could not be notified (docs/settings-and-caching.md, "Invalidation"):
            // 503, stating that it was saved.
            if let Some(unpublished) = crate::cache::unpublished(&error) {
                return (StatusCode::SERVICE_UNAVAILABLE, unpublished.to_string());
            }
            if error.downcast_ref::<crate::cache::Unavailable>().is_some() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    crate::debug::visible_error(&error),
                );
            }
            if let Some(crate::identity::IdentityError::Suspended(message)) =
                error.downcast_ref::<crate::identity::IdentityError>()
            {
                return (StatusCode::FORBIDDEN, message.clone());
            }
            if let Some(failure) = error.downcast_ref::<crate::activities::ActivityFailure>() {
                return (StatusCode::UNPROCESSABLE_ENTITY, failure.0.clone());
            }
            tracing::error!(%error,"publish transaction failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::i18n::current().editor_error_publish().into(),
            )
        }
    }
}
async fn publish(
    Page(state): Page,
    headers: HeaderMap,
    form: Result<Form<Editor>, axum::extract::rejection::FormRejection>,
) -> Response {
    submit(state, headers, form, None, None).await
}
async fn reply(
    Page(state): Page,
    AxumPath(thread): AxumPath<String>,
    headers: HeaderMap,
    form: Result<Form<Editor>, axum::extract::rejection::FormRejection>,
) -> Response {
    let tid = thread
        .strip_prefix("thread-")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0);
    if tid.is_none() {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    }
    submit(state, headers, form, tid, None).await
}
async fn sub_reply(
    Page(state): Page,
    AxumPath(root): AxumPath<i64>,
    headers: HeaderMap,
    form: Result<Form<Editor>, axum::extract::rejection::FormRejection>,
) -> Response {
    submit(state, headers, form, None, Some(root)).await
}
async fn submit(
    state: AppState,
    headers: HeaderMap,
    form: Result<Form<Editor>, axum::extract::rejection::FormRejection>,
    tid: Option<i64>,
    root: Option<i64>,
) -> Response {
    let Ok(Form(mut e)) = form else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().common_error_form(),
        );
    };
    e.edit_pid = 0;
    e.edit_thread = false;
    if let Some(tid) = tid {
        e.tid = tid;
        e.root_pid = 0;
        e.reply_to_pid = 0;
    }
    if let Some(root) = root {
        let sql = "SELECT tid FROM gonof_forum_post WHERE pid=? AND first=0 AND root_pid IS NULL";
        match crate::db::query_scalar::<i64>(sql)
            .bind(root)
            .fetch_optional(state.db.executor())
            .await
        {
            Ok(Some(tid)) => {
                e.tid = tid;
                e.root_pid = root;
                if e.reply_to_pid == 0 {
                    e.reply_to_pid = root
                }
            }
            Ok(None) => {
                return error_page(
                    &state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().editor_error_parent(),
                );
            }
            Err(err) => return database_error(&state, err.into()),
        }
    }
    if tid.is_none() && root.is_none() {
        e.tid = 0;
        e.root_pid = 0;
        e.reply_to_pid = 0;
    }
    let submitted_key = e.request_key.clone();
    e.prepare();
    e.request_key = submitted_key;
    // When invalid, the editor that was already prepared must be filled back (forum, reply target and draft key all
    // resolved); the extractor cannot hand over that state, so it is validated explicitly here.
    if let Some(reason) = auth::csrf_failure(&state, &headers, &e._csrf) {
        e.message = crate::i18n::current().editor_error_stale(reason);
        return failure(&state, &headers, &e, StatusCode::FORBIDDEN).await;
    }
    let Some(session) = &state.viewer.session else {
        e.message = crate::i18n::current().editor_error_sign_in_to_post().into();
        return failure(&state, &headers, &e, StatusCode::UNAUTHORIZED).await;
    };
    let refs = match markdown::references(&e.body) {
        Ok(v) => v,
        Err(err) => {
            e.message = err.to_string();
            return failure(&state, &headers, &e, StatusCode::UNPROCESSABLE_ENTITY).await;
        }
    };
    let writer = ContentWriter::new(state.db.clone(), state.config.content.clone())
        .with_codes(state.config.identity.two_factor.clone())
        .with_site_url(state.site_url());
    let visibility = if e.tid == 0 {
        match submitted_visibility(&e.visibility) {
            Ok(value) => value.unwrap_or_default(),
            Err(err) => {
                let (status, message) = write_error(err);
                e.message = message;
                return failure(&state, &headers, &e, status).await;
            }
        }
    } else {
        ThreadVisibility::Public
    };
    let prizes = if e.tid == 0 && e.category == 3 && !e.prizes.trim().is_empty() {
        match crate::activities::parse_prize_lines(
            &e.prizes,
            &e.lottery_codes,
            &state.frame.settings.site.credit_name,
        ) {
            Ok(list) => list,
            Err(message) => {
                e.message = message;
                return failure(&state, &headers, &e, StatusCode::UNPROCESSABLE_ENTITY).await;
            }
        }
    } else {
        Vec::new()
    };
    let result = if e.tid == 0 {
        writer
            .publish_actor(
                session.member.actor(),
                NewThread {
                    activity: crate::activities::ActivitySpec {
                        kind: e.category,
                        budget: e.budget,
                        shares: e.shares,
                        distribution: e.distribution.clone(),
                        deadline: parse_deadline(&e.deadline),
                        max_choices: e.max_choices,
                        options: e
                            .options
                            .lines()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect(),
                        source_name: e.source_name.clone(),
                        source_url: e.source_url.clone(),
                        source_author: e.source_author.clone(),
                        entry_mode: if e.entry_mode == "click" {
                            "click".into()
                        } else {
                            "reply".into()
                        },
                        requirements: e
                            .requirements
                            .lines()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect(),
                        review: !e.review.is_empty(),
                        early_draw: !e.early_draw.is_empty(),
                        prizes,
                    },
                    fid: e.fid,
                    title: e.title.clone(),
                    body: e.body.clone(),
                    attachment_ids: refs.attachments.into_iter().collect(),
                    request_key: e.request_key.clone(),
                    visibility,
                },
            )
            .await
    } else {
        writer
            .reply_actor(
                session.member.actor(),
                NewReply {
                    tid: e.tid,
                    root_pid: (e.root_pid > 0).then_some(e.root_pid),
                    reply_to_pid: (e.reply_to_pid > 0).then_some(e.reply_to_pid),
                    body: e.body.clone(),
                    attachment_ids: refs.attachments.into_iter().collect(),
                    request_key: e.request_key.clone(),
                },
            )
            .await
    };
    match result {
        Ok(result) => published(&state, &headers, &e, result).await,
        Err(err) => {
            let (status, message) = write_error(err);
            e.message = message;
            failure(&state, &headers, &e, status).await
        }
    }
}

pub(super) async fn failure(
    state: &AppState,
    headers: &HeaderMap,
    e: &Editor,
    status: StatusCode,
) -> Response {
    let mut e = e.clone();
    e.preserve_input = true;
    let mut response = editor_page(state, &e, status).await;
    if hx(headers) {
        response
            .headers_mut()
            .insert("HX-Retarget", HeaderValue::from_static("body"));
        response
            .headers_mut()
            .insert("HX-Reswap", HeaderValue::from_static("innerHTML"));
        response
            .headers_mut()
            .insert("X-Composer-Fragment", HeaderValue::from_static("true"));
    }
    response
}

pub(super) fn hx(headers: &HeaderMap) -> bool {
    headers.get("HX-Request").is_some_and(|v| v == "true")
}
pub(super) async fn published(
    state: &AppState,
    headers: &HeaderMap,
    e: &Editor,
    result: Published,
) -> Response {
    let _thread =
        match content::thread_for_viewer(&state.db, result.tid as u64, state.readable()).await {
            Ok(Some(t)) => t,
            Ok(None) if e.edit_pid > 0 => {
                return saved_redirect(
                    state,
                    headers,
                    e,
                    &format!("/manage/content/{}", result.pid),
                );
            }
            Ok(None) => {
                return error_page(
                    state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().editor_error_thread(),
                );
            }
            Err(err) => return database_error(state, err),
        };
    let location = match content::post_location_for_viewer(
        &state.db,
        result.tid as u64,
        result.pid,
        e.view == "flat",
        state.readable(),
        crate::urls::Paging::from_settings(&state.frame.settings),
    )
    .await
    {
        Ok(Some(location)) => location,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().editor_error_reply(),
            );
        }
        Err(error) => return database_error(state, error),
    };
    if e.edit_pid > 0 {
        return saved_redirect(state, headers, e, &location);
    }
    let uid = state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.uid)
        .unwrap_or(0);
    let marker =
        serde_json::json!({"uid":uid.to_string(),"draft":e.draft,"request_key":e.request_key})
            .to_string();
    let mut response = if hx(headers) && result.position > 1 {
        let target_page = location.split('#').next().unwrap_or(&location);
        let origin = auth::safe_return(&e.location);
        let origin = origin.split('#').next().unwrap_or("");
        let target_query = serde_urlencoded_query(target_page);
        let origin_query = serde_urlencoded_query(origin);
        let same_page = origin.split('?').next() == target_page.split('?').next()
            && origin_query.view == target_query.view
            && target_query.sub_root.is_none_or(|root| {
                target_query.sub_page.unwrap_or(1)
                    == if origin_query.sub_root == Some(root) {
                        origin_query.sub_page.unwrap_or(1)
                    } else {
                        1
                    }
            });
        if same_page {
            let query: BrowseQuery = serde_urlencoded_query(target_page);
            let page = PageRoute::parse(
                target_page
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/'),
                &[],
            )
            .map(|r| r.page())
            .unwrap_or(1);
            let mut response = topic_response(
                state,
                &target_page.parse().unwrap(),
                &query,
                result.tid as u64,
                page,
                true,
                None,
            )
            .await;
            response.headers_mut().insert(
                "HX-Push-Url",
                HeaderValue::from_str(&crate::site::href(&location)).expect("internal post URL"),
            );
            response
        } else {
            (
                StatusCode::OK,
                [("HX-Redirect", crate::site::href(&location))],
            )
                .into_response()
        }
    } else if hx(headers) {
        (
            StatusCode::OK,
            [("HX-Redirect", crate::site::href(&location))],
        )
            .into_response()
    } else {
        auth::redirect(&location)
    };
    let trigger=serde_json::json!({"composer:published":{"uid":uid.to_string(),"draft":e.draft,"request_key":e.request_key},"composer:locate":{"position":result.position}}).to_string();
    response.headers_mut().insert(
        "HX-Trigger-After-Swap",
        HeaderValue::from_str(&trigger).expect("JSON header"),
    );
    if !hx(headers) || response.headers().contains_key("HX-Redirect") {
        auth::auxiliary_set_cookie(&mut response, "gonof_published", &marker, 60);
    }
    response
}
fn saved_redirect(state: &AppState, headers: &HeaderMap, e: &Editor, location: &str) -> Response {
    let uid = state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.uid)
        .unwrap_or(0);
    let marker =
        serde_json::json!({"uid":uid.to_string(),"draft":e.draft,"request_key":e.request_key})
            .to_string();
    let mut response = if hx(headers) {
        (
            StatusCode::OK,
            [("HX-Redirect", crate::site::href(location))],
        )
            .into_response()
    } else {
        auth::redirect(location)
    };
    auth::auxiliary_set_cookie(&mut response, "gonof_published", &marker, 60);
    response
}
pub(super) fn serde_urlencoded_query(path: &str) -> BrowseQuery {
    let mut query = BrowseQuery::default();
    for (key, value) in url::form_urlencoded::parse(
        path.split_once('?')
            .map(|(_, q)| q.split('#').next().unwrap_or(""))
            .unwrap_or("")
            .as_bytes(),
    ) {
        match key.as_ref() {
            "view" => query.view = value.into_owned(),
            "sub_root" => query.sub_root = value.parse().ok(),
            "sub_page" => query.sub_page = value.parse().ok(),
            _ => {}
        }
    }
    query
}
async fn preview(Page(state): Page, headers: HeaderMap, form: CheckedForm<Editor>) -> Response {
    let mut e = match form {
        CheckedForm::Valid(e) => e,
        CheckedForm::Malformed => {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().common_error_form(),
            );
        }
        CheckedForm::StaleToken(mut e, reason) => {
            e.prepare();
            e.preserve_input = true;
            e.message = crate::i18n::current().editor_error_stale(reason);
            return failure(&state, &headers, &e, StatusCode::FORBIDDEN).await;
        }
    };
    e.prepare();
    e.preserve_input = true;
    if e.body.len() > usize::try_from(state.frame.settings.posting.body_max_bytes).unwrap_or(0) {
        e.message = crate::i18n::current().editor_error_body_long().into();
        return failure(&state, &headers, &e, StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    let names = match markdown::member_names(&state.db, &[&e.body]).await {
        Ok(v) => v,
        Err(err) => {
            e.message = err.to_string();
            return failure(&state, &headers, &e, StatusCode::UNPROCESSABLE_ENTITY).await;
        }
    };
    if e.edit_pid > 0 {
        let Some(session) = &state.viewer.session else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        match super::editing::service(&state)
            .editable(session.member.actor(), e.edit_pid)
            .await
        {
            Ok(post) => {
                e.management = post.can_manage && !post.can_author_edit;
                e.edit_thread = post.first;
            }
            Err(err) => {
                let (status, message) = write_error(err);
                e.message = message;
                return failure(&state, &headers, &e, status).await;
            }
        }
    }
    let mut paths = std::collections::BTreeMap::new();
    if let Some(session) = &state.viewer.session {
        let refs = match markdown::references(&e.body) {
            Ok(refs) => refs,
            Err(err) => {
                e.message = err.to_string();
                return failure(&state, &headers, &e, StatusCode::UNPROCESSABLE_ENTITY).await;
            }
        };
        let ids: Vec<i64> = refs
            .attachments
            .into_iter()
            .take(state.config.content.max_attachments)
            .collect();
        // Fetches the whole batch at once (`Media::editor_uploads`) rather than querying per attachment; the preview
        // requests once 350 ms after typing stops.
        let uploads = state
            .media
            .editor_uploads(
                session.member.actor(),
                &ids,
                (e.edit_pid > 0).then_some(e.edit_pid),
            )
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(error = %error, "failed to read preview attachments");
                Vec::new()
            });
        for upload in uploads.into_iter().filter(|upload| upload.is_ready()) {
            let path = match upload.bound_pid {
                Some(pid) => format!("/media/{}?pid={pid}", upload.aid),
                None => format!("/media/{}", upload.aid),
            };
            paths.insert(upload.aid, path);
        }
    }
    let rendered = markdown::render_preview(&e.body, &names, &paths, state.site_url());
    // The preview fills cards with the author's own read permissions, matching what the author sees after publishing.
    let tids = crate::content::cards::page_tids(&rendered.cards);
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    let cards = match crate::content::cards::topic_cards(
        &state.db,
        viewer_uid,
        &state.frame.all_forums,
        &tids,
    )
    .await
    {
        Ok(cards) => cards,
        Err(error) => return database_error(&state, error),
    };
    e.preview = markdown::fill_cards(&rendered.html, &rendered.cards, &cards);
    e.prepare();
    if hx(&headers) {
        Html(format!("<div class=\"prose\">{}</div>", e.preview)).into_response()
    } else {
        editor_page(&state, &e, StatusCode::OK).await
    }
}
#[derive(Default, Deserialize)]
struct MentionQuery {
    #[serde(default)]
    q: String,
}
async fn mentions(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    Query(query): Query<MentionQuery>,
) -> Response {
    let Some(session) = &viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let q = query.q.trim();
    if q.is_empty() || q.chars().count() > 50 {
        return Html(String::new()).into_response();
    }
    let pattern = format!(
        "{}%",
        q.to_lowercase()
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    );
    let sql = &format!(
        "SELECT m.uid,m.username FROM gonof_common_member m WHERE m.account_state='active' AND m.freeze=0 AND LOWER(m.username) LIKE ? ESCAPE '!' AND {} ORDER BY m.username,m.uid LIMIT 10",
        content::unblocked_sql(Some(session.member.uid), "m.uid")
    );
    match crate::db::query(sql)
        .bind(pattern)
        .fetch_all(state.db.executor())
        .await
    {
        Ok(rows) => Html(
            rows.iter()
                .filter_map(|r| {
                    Some(format!(
                        "<button type=\"button\" data-uid=\"{}\" data-username=\"{}\">{}</button>",
                        r.try_get::<i64, _>("uid").ok()?,
                        markdown::escape(&r.try_get::<String, _>("username").ok()?),
                        markdown::escape(&r.try_get::<String, _>("username").ok()?)
                    ))
                })
                .collect::<String>(),
        )
        .into_response(),
        Err(err) => database_error(&state, err.into()),
    }
}

struct UserRow {
    pid: u64,
    position: usize,
    /// The verb in recent activity: "Posted" or "Replied to".
    action: &'static str,
    href: String,
    title: String,
    excerpt: String,
    time: String,
    /// The post time as a timestamp, for the `datetime` of the shown time.
    dateline: u64,
    forum_name: String,
    forum_slug: String,
    category: &'static str,
    replies: usize,
}
#[derive(Template)]
#[template(path = "user/profile.html")]
struct UserTemplate<'a> {
    p: PageMeta<'a>,
    block_control: super::social::Control,
    follow_control: super::social::Control,
    /// Following and follower counts appear together with the personal space gate.
    follows: crate::interaction::social::FollowCounts,
    message_entry: Option<MessageEntry>,
    username: String,
    uid: i64,
    avatar: AvatarView,
    /// The "Frozen" and "Being deleted" labels in the profile area: a freeze shows no reason or term; sanction details
    /// are only in the member's own `/my/sanctions`.
    frozen: bool,
    deleting: bool,
    /// Escaped bio HTML with addresses already wrapped as links (`bio::html`).
    bio: String,
    joined: String,
    /// The sign-up time as a timestamp, for the `datetime` of the shown date.
    joined_at: i64,
    own_profile: bool,
    stats: content::MemberStats,
    /// Level and growth points, queried only after the space visibility gate.
    level: crate::growth::Level,
    /// Name of the next level; empty when already at the top level.
    next_level: String,
    /// Verification badge, verification description and linked website, queried only after the space visibility
    /// gate.
    verification: Option<crate::directory::Badge>,
    /// Honor badges, likewise queried only after the space visibility gate.
    honors: Vec<crate::honors::HonorBadge>,
    /// "Online" or "Last seen today / N days ago / date"; empty when there is no record.
    presence: String,
    view: &'static str,
    tabs: Vec<Tab>,
    total: usize,
    pages: Vec<PageLink>,
    previous: Option<String>,
    next: Option<String>,
    rows: Vec<UserRow>,
}
pub(super) async fn space_page(Page(state): Page, OriginalUri(uri): OriginalUri) -> Response {
    member_space(&state, uri.path()).await
}

async fn member_space(state: &AppState, path: &str) -> Response {
    use urls::{MemberRoute, MemberView};
    let Some(route) = MemberRoute::parse(path) else {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    let profile = content::public_member_profile(&state.db, route.uid).await;
    let member = match profile {
        Ok(Some(content::MemberProfile::Member(v))) => v,
        // Deleted account: the profile page and collection pages all get the same 410 placeholder page, before the
        // visibility gate (it carries none of the original profile).
        Ok(Some(content::MemberProfile::Deleted)) => return deleted_space(state, &route.path()),
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().profile_error_not_found(),
            );
        }
        Err(err) => return database_error(state, err),
    };
    let viewer_uid = state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.uid);
    // Gate every space view before fetching recent content, counts or collection pages.
    if !member.space_visibility.can_view(member.uid, viewer_uid) {
        if member.space_visibility == crate::identity::SpaceVisibility::Members
            && viewer_uid.is_none()
        {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("return_to", &route.path())
                .finish();
            return auth::redirect(&format!("/login?{query}"));
        }
        return error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().profile_error_private(),
        );
    }
    // Forums the visitor blocked are not visible in personal spaces either; hidden forums appear as usual.
    let blocked_fids = state.frame.forum_filters.blocked();
    // Statistics and tab totals read the member count table, the same number for every visitor;
    // entries the list cannot read are still not shown.
    let stats = match content::member_stats_for_viewer(&state.db, member.uid, viewer_uid).await {
        Ok(stats) => stats,
        Err(err) => return database_error(state, err),
    };
    let now = identity::now();
    let presence = match content::member_presence(&state.db, member.uid, now).await {
        Ok(content::Presence::Online) => crate::i18n::current().profile_online().to_owned(),
        Ok(content::Presence::LastSeen(seen)) => crate::i18n::current()
            .profile_last_seen(crate::clock::day_label(seen, now, &crate::clock::current())),
        Ok(content::Presence::Unknown) => String::new(),
        Err(err) => return database_error(state, err),
    };
    let level = match crate::growth::growth(&state.db, member.uid).await {
        Ok(growth) => state.frame.settings.growth.level(growth),
        Err(err) => return database_error(state, err),
    };
    let next_level = state
        .frame
        .settings
        .growth
        .name_of(level.number)
        .unwrap_or_default();
    let verification = match crate::directory::badge(&state.db, member.uid, now).await {
        Ok(badge) => badge,
        Err(err) => return auth::identity_error(state, err),
    };
    let honors = match crate::honors::member_badges(&state.db, member.uid, now).await {
        Ok(badges) => badges,
        Err(err) => return auth::identity_error(state, err),
    };
    let (posts, total) = match route.view {
        MemberView::Overview => {
            let posts = match content::recent_member_content_for_viewer(
                &state.db,
                member.uid,
                state.readable(),
                &blocked_fids,
                crate::urls::Paging::from_settings(&state.frame.settings),
            )
            .await
            {
                Ok(posts) => posts,
                Err(err) => return database_error(state, err),
            };
            let total = posts.len();
            (posts, total)
        }
        view => {
            let (kind, total) = match view {
                MemberView::Threads => (content::MemberContentKind::Threads, stats.threads),
                _ => (content::MemberContentKind::Posts, stats.replies),
            };
            match content::member_content_page_for_viewer(
                &state.db,
                member.uid,
                kind,
                route.page,
                20,
                state.readable(),
                &blocked_fids,
                (
                    total,
                    crate::urls::Paging::from_settings(&state.frame.settings),
                ),
            )
            .await
            {
                Ok(collection) => (collection.posts, collection.total),
                Err(err) => return database_error(state, err),
            }
        }
    };
    let page_count = total.div_ceil(20).max(1);
    if route.page > page_count {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_no_such_page(),
        );
    }
    let canonical = route.path();
    if path != canonical {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(canonical))],
        )
            .into_response();
    }
    // Reply excerpts must be rendered before taking plain text: raw Markdown would expose spoilers, old user names in
    // mentions, and attachment syntax.
    // Thread rows show no excerpt and take no part in the user name query.
    let replies: Vec<_> = posts
        .iter()
        .filter(|post| post.position != 1)
        .map(|post| post.message.as_str())
        .collect();
    let names = match crate::markdown::member_names(&state.db, &replies).await {
        Ok(names) => names,
        Err(err) => return database_error(state, err),
    };
    let rows = posts
        .into_iter()
        .map(|post| UserRow {
            pid: post.pid,
            position: post.position,
            action: if post.position == 1 {
                crate::i18n::current().profile_action_thread()
            } else {
                crate::i18n::current().profile_action_reply()
            },
            href: post.href,
            title: post.subject,
            excerpt: if post.position == 1 {
                String::new()
            } else {
                crate::markdown::excerpt(&post.message, &names, 180)
            },
            time: post.time,
            dateline: post.dateline,
            forum_name: post.forum_name,
            forum_slug: post.forum_slug,
            category: content::category_label(post.category),
            replies: post.replies,
        })
        .collect();
    let tabs = [MemberView::Overview, MemberView::Threads, MemberView::Posts]
        .into_iter()
        .map(|view| Tab {
            label: view.label(),
            href: MemberRoute {
                view,
                page: 1,
                ..route
            }
            .path(),
            active: route.view == view,
        })
        .collect();
    // Only render a bounded page window even for very large personal collections.
    let pages = page_window(route.page, page_count)
        .map(|number| PageLink {
            number,
            href: route.with_page(number).path(),
            active: number == route.page,
        })
        .collect();
    let title = if route.view == MemberView::Overview {
        member.username.clone()
    } else {
        crate::i18n::current().common_paged_title(
            crate::i18n::current().profile_collection(member.username.as_str(), route.view.label()),
            route.page,
        )
    };
    let block_control = match super::social::control(
        state,
        crate::interaction::social::Relation::Block,
        member.uid,
        canonical.clone(),
    )
    .await
    {
        Ok(control) => control,
        Err(error) => return database_error(state, error),
    };
    let mut follow_control = match super::social::control(
        state,
        crate::interaction::social::Relation::Member,
        member.uid,
        canonical.clone(),
    )
    .await
    {
        Ok(control) => control,
        Err(error) => return database_error(state, error),
    };
    // Accounts being deleted cannot be newly followed; existing follows can still be removed.
    if !member.active && !follow_control.active {
        follow_control.can_change = false;
        follow_control.notice = "";
    }
    let follows = match crate::interaction::social::follow_counts(&state.db, member.uid).await {
        Ok(counts) => counts,
        Err(error) => return database_error(state, error),
    };
    let message_entry = match message_entry(state, &member).await {
        Ok(entry) => entry,
        Err(error) => return database_error(state, error),
    };
    let avatar = avatar_view(
        Some(member.uid),
        member.avatar_aid,
        member.avatar,
        &member.username,
    );
    let mut p = meta(
        state,
        title,
        crate::i18n::current().profile_collection(member.username.as_str(), route.view.label()),
        &canonical,
        "",
        "",
    );
    p.page_data = super::page_data::PageData::member(member.uid, route.page);
    // Profile and collection pages (including pagination) whose visibility is "Everyone" are indexable for anonymous
    // visitors, regardless of freezes and the account deletion cooling-off period.
    // The indexable list in `meta` only knows paths and cannot see visibility, so it is set here after the gate
    // passes; these pages are not in the sitemap.
    p.indexable = state.frame.settings.seo.indexing
        && viewer_uid.is_none()
        && member.space_visibility == crate::identity::SpaceVisibility::Public;
    if p.indexable {
        // An indexable profile page outputs `ProfilePage` and `og:type=profile`.
        let base = state.site_url().trim_end_matches('/');
        let url = format!("{base}{}", urls::space(member.uid));
        let image = if avatar.url.is_empty() {
            String::new()
        } else {
            format!("{base}{}", avatar.url)
        };
        p.og_type = "profile";
        p.profile_username = member.username.clone();
        p.json_ld = vec![super::syndication::profile_json_ld(
            &member, &url, &image, &stats,
        )];
    }
    render(&UserTemplate {
        block_control,
        follow_control,
        follows,
        message_entry,
        p,
        avatar,
        frozen: member.frozen,
        deleting: !member.active,
        own_profile: state
            .viewer
            .session
            .as_ref()
            .is_some_and(|session| session.member.uid == member.uid),
        uid: member.uid,
        bio: crate::bio::html(&member.bio),
        joined: crate::clock::date(member.regdate, &crate::clock::current()),
        joined_at: member.regdate,
        stats,
        level,
        next_level,
        verification,
        honors,
        presence,
        username: member.username,
        view: route.view.value(),
        tabs,
        total,
        pages,
        previous: (route.page > 1).then(|| route.with_page(route.page - 1).path()),
        next: (route.page < page_count).then(|| route.with_page(route.page + 1).path()),
        rows,
    })
}

/// Profile and collection pages of a deleted account: a 410 placeholder page that only says "This account has been
/// deleted" and shows no original user name, avatar, bio, statistics or content; `noindex`, with no canonical and no
/// structured data.
#[derive(Template)]
#[template(path = "user/deleted.html")]
struct DeletedMemberTemplate<'a> {
    p: PageMeta<'a>,
}

fn deleted_space(state: &AppState, path: &str) -> Response {
    let mut p = meta(
        state,
        crate::i18n::current().common_deleted_user().into(),
        crate::i18n::current().profile_deleted().into(),
        path,
        "",
        "",
    );
    p.indexable = false;
    // Like error pages, no canonical is output: the placeholder page is not the canonical version of any URL.
    p.canonical.clear();
    let mut response = render(&DeletedMemberTemplate { p });
    *response.status_mut() = StatusCode::GONE;
    response
}

/// The "Messages" entry on a member's profile page. The rules match the send transaction (`messages::send`):
/// not shown to anonymous visitors or on one's own profile; when sending is possible it links to the new message page
/// with the recipient prefilled; when not, it shows disabled with the reason.
/// Reasons on the viewer's side (site read-only, e-mail not verified) are stated as they are. When the other member is
/// being deleted, it says plainly "This member is closing their account and cannot receive private messages", and the
/// profile page already shows the "Being deleted" label; when the other member blocked you it still says "This member
/// does not accept your private messages", without revealing the block. Both sentences are the same as when sending is
/// rejected. A frozen member can still receive messages, and you can still send to someone you blocked (blocking is
/// one-way). A deleted account's profile is a 410 placeholder page with no entry. When the viewer is frozen or being
/// deleted the session is already invalid and is treated as anonymous; during the deletion cooling-off period the
/// account is still normal and can send as usual.
struct MessageEntry {
    href: String,
    reason: &'static str,
}

async fn message_entry(
    state: &AppState,
    member: &content::PublicMember,
) -> anyhow::Result<Option<MessageEntry>> {
    let Some(session) = &state.viewer.session else {
        return Ok(None);
    };
    let viewer = &session.member;
    if viewer.uid == member.uid {
        return Ok(None);
    }
    let reason = if state.frame.settings.site.read_only {
        crate::i18n::current().my_message_entry_read_only()
    } else if !viewer.can_contribute() {
        crate::login::Participation::current()
            .message(crate::i18n::current().my_message_entry_verify_email())
    } else if !member.active {
        crate::messages::recipient_deleting()
    } else if !crate::interaction::social::blocked_uids(&state.db, member.uid, &[viewer.uid])
        .await?
        .is_empty()
    {
        crate::messages::recipient_refuses()
    } else {
        ""
    };
    Ok(Some(MessageEntry {
        href: format!("/my/messages/new?to={}", member.uid),
        reason,
    }))
}

/// The reverse of `parse_deadline`: writes a timestamp as the text the datetime-local control expects.
/// Both must use the same [`crate::clock::TimeFormat`], otherwise when an administrator fills the form with the time
/// seen on the page, it is stored back off by one time zone.
pub(super) fn format_deadline(seconds: i64) -> String {
    crate::clock::input_value(seconds, &deadline_format())
}

/// Default activity deadline of the new-post form; composer.js uses the same three days when filling the value after a
/// failed re-render, generated in the site time zone given by `data-offset-minutes` on the input.
pub(super) const DEFAULT_DEADLINE_SECONDS: i64 = 3 * 86_400;
/// Deadlines are entered and displayed in the site time zone, the same basis as the other times on the page.
fn deadline_format() -> crate::clock::TimeFormat {
    crate::clock::current()
}

pub(super) fn parse_deadline(input: &str) -> i64 {
    if let Ok(seconds) = input.parse::<i64>() {
        return seconds;
    }
    crate::clock::parse_input(input, &deadline_format()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_text_is_site_time_and_round_trips() {
        // Outside a request it uses the default basis (UTC+8, the same as the seeded site time zone): the text for the
        // same moment is eight hours later than UTC.
        assert_eq!(format_deadline(0), "1970-01-01T08:00");
        assert_eq!(parse_deadline("1970-01-01T08:00"), 0);
        // The control is precise to the minute, so whole-minute moments must round-trip unchanged.
        let minute = (identity::now() + DEFAULT_DEADLINE_SECONDS) / 60 * 60;
        assert_eq!(parse_deadline(&format_deadline(minute)), minute);
        // Pure digits are taken directly as a timestamp; unrecognized formats all become zero, and validation then
        // reports that the deadline must be in the future.
        assert_eq!(parse_deadline("1758000000"), 1_758_000_000);
        assert_eq!(parse_deadline("2026-09-19 12:00"), 0);
        assert_eq!(parse_deadline(""), 0);
        // Outside chrono's representable range it returns an empty string instead of panicking.
        assert_eq!(format_deadline(i64::MAX), "");
    }
}
