use super::extract::{CheckedForm, CsrfForm, Page, csrf_forms};
use super::posting::Editor;
use super::*;
use crate::rate_limit::Rule;
use crate::{
    identity, markdown,
    media::{MediaError, NewUpload, StorageInput, StorageSettings, Upload},
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as AxumPath},
    http::HeaderMap,
    routing::post,
};
use tokio::io::AsyncWriteExt;

pub(super) fn routes(upload_limit: usize) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/media", post(upload))
        .route("/api/media/{aid}", get(status))
        .route("/api/media/{aid}/cancel", post(cancel))
        .route("/api/media/{aid}/continue", post(continue_editor))
        .route("/media/{aid}", get(download))
        .route("/thumb/{tid}/{aid}", get(thumbnail))
        .route("/manage/storage", get(storage_page).post(storage_save))
        .layer(DefaultBodyLimit::max(upload_limit))
}
fn error(state: &AppState, error: MediaError) -> Response {
    super::media_failure(state, error)
}
#[derive(Template)]
#[template(path = "editor/attachment.html")]
struct AttachmentTemplate<'a> {
    upload: &'a Upload,
}
fn attachment_response(upload: &Upload) -> Response {
    render(&AttachmentTemplate { upload })
}
#[derive(Template)]
#[template(path = "editor/upload_status.html")]
struct UploadStatusTemplate<'a> {
    p: PageMeta<'a>,
    upload: &'a Upload,
    e: &'a Editor,
}
fn upload_page(state: &AppState, upload: &Upload, e: &Editor) -> Response {
    render(&UploadStatusTemplate {
        p: meta(
            state,
            crate::i18n::current().editor_upload_title().into(),
            crate::i18n::current().editor_upload_description().into(),
            "/new",
            "",
            "",
        ),
        upload,
        e,
    })
}
fn field(e: &mut Editor, name: &str, value: String) {
    match name {
        "_csrf" => e._csrf = value,
        "request_key" => e.request_key = value,
        "title" => e.title = value,
        "body" => e.body = value,
        "fid" => e.fid = value.parse().unwrap_or(0),
        "tid" => e.tid = value.parse().unwrap_or(0),
        "root_pid" => e.root_pid = value.parse().unwrap_or(0),
        "reply_to_pid" => e.reply_to_pid = value.parse().unwrap_or(0),
        "quote_pid" => e.quote_pid = value.parse().unwrap_or(0),
        "view" => e.view = value,
        "location" => e.location = auth::safe_return(&value),
        "return_to" => e.return_to = auth::safe_return(&value),
        "attachment_ids" => e.attachment_ids = value,
        "edit_pid" => e.edit_pid = value.parse().unwrap_or(0),
        "edit_thread" => e.edit_thread = value == "true",
        "expected_post_version" => e.expected_post_version = value.parse().unwrap_or(0),
        "expected_thread_version" => e.expected_thread_version = value.parse().unwrap_or(0),
        "reason" => e.reason = value,
        // A no-JS upload goes through multipart, and any field missed here falls back to its default after the round
        // trip: the category drops back to regular and the budget and options are cleared, so the published thread is
        // not what the author filled in. The same goes for visibility: falling back to empty publishes publicly, and a
        // thread meant for members only would be visible to anonymous visitors.
        "visibility" => e.visibility = value,
        "category" => e.category = value.parse().unwrap_or(0),
        "budget" => e.budget = value.parse().unwrap_or(0),
        "shares" => e.shares = value.parse().unwrap_or(0),
        "distribution" => e.distribution = value,
        "deadline" => e.deadline = value,
        "max_choices" => e.max_choices = value.parse().unwrap_or(0),
        "options" => e.options = value,
        "entry_mode" => e.entry_mode = value,
        "requirements" => e.requirements = value,
        "review" => e.review = value,
        "early_draw" => e.early_draw = value,
        "prizes" => e.prizes = value,
        "lottery_codes" => e.lottery_codes = value,
        "source_name" => e.source_name = value,
        "source_url" => e.source_url = value,
        "source_author" => e.source_author = value,
        _ => {}
    }
}
async fn upload_error(
    state: &AppState,
    headers: &HeaderMap,
    e: &mut Editor,
    status: StatusCode,
    message: &str,
) -> Response {
    if posting::hx(headers) {
        return error_page(state, status, message);
    }
    keep_stored_audience(state, e).await;
    e.prepare();
    e.preserve_input = true;
    e.message = message.to_owned();
    posting::editor_page(state, e, status).await
}
/// A thread edit refilled from a body that ended before its audience field (cut off or malformed) takes the thread's
/// stored audience. Left empty, the editor would check "public", and saving it would open a members-only thread to
/// guests. A thread the member can no longer edit keeps the empty value: saving it fails anyway.
async fn keep_stored_audience(state: &AppState, e: &mut Editor) {
    if !e.edit_thread || e.edit_pid <= 0 || !e.visibility.is_empty() {
        return;
    }
    let Some(session) = &state.viewer.session else {
        return;
    };
    match super::editing::service(state)
        .editable(session.member.actor(), e.edit_pid)
        .await
    {
        Ok(post) => e.visibility = post.visibility,
        Err(error) => tracing::warn!(error = %error, "failed to read the thread being edited"),
    }
}
/// Attachment uploads per account: 30 per 5 minutes and 120 per hour, counted by uid.
///
/// A post references at most 20 attachments (the default `max_attachments` and the Markdown hard limit are both 20); the
/// editor must be able to upload 20 dragged-in files in one go, with 10 more left for retrying failures; 120 per hour
/// equals 6 posts in a row with full attachments, which normal writing never reaches. The byte quota (daily
/// `daily_max_bytes`) does not limit the count: flooding with 1-byte files queues a processing job for each, which is
/// what the count limit addresses. Like the buckets for other uses, the
/// count is shared across processes when Redis is enabled.
const UPLOAD_BURST: (u32, i64) = (30, 300);
const UPLOAD_HOURLY: (u32, i64) = (120, 3600);

/// `Ok(true)` when allowed. With Redis enabled but unavailable, `Err` carries 503 and the reason, refilled into the
/// editor.
async fn upload_allowed(state: &AppState, uid: i64) -> Result<bool, (StatusCode, String)> {
    let (burst, burst_window) = UPLOAD_BURST;
    let (hourly, hourly_window) = UPLOAD_HOURLY;
    state
        .limits
        .allow(&[
            Rule::new("media-upload", uid, burst, burst_window),
            Rule::new("media-upload-hour", uid, hourly, hourly_window),
        ])
        .await
        .map_err(|error| {
            let detail = crate::debug::visible_error(&error);
            tracing::error!(error = %detail, "cache unavailable");
            (StatusCode::SERVICE_UNAVAILABLE, detail)
        })
}

/// Reads the rest of a refused file part without keeping any of it, so that the fields after it can still be read.
async fn drain(part: &mut axum::extract::multipart::Field<'_>) -> Result<(), (StatusCode, String)> {
    while part
        .chunk()
        .await
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                crate::i18n::current().media_error_interrupted().to_owned(),
            )
        })?
        .is_some()
    {}
    Ok(())
}

fn stale_upload_form(reason: &str) -> String {
    crate::i18n::current().editor_error_stale(reason)
}

async fn upload(Page(state): Page, headers: HeaderMap, mut multipart: Multipart) -> Response {
    let Some(session) = &state.viewer.session else {
        return error_page(
            &state,
            StatusCode::UNAUTHORIZED,
            crate::i18n::current().editor_error_sign_in(),
        );
    };
    if !session.member.can_contribute() {
        return error_page(
            &state,
            StatusCode::FORBIDDEN,
            crate::login::Participation::current()
                .message(crate::i18n::current().media_error_verify()),
        );
    }
    let mut e = Editor::default();
    // Same order as avatar uploads: the origin is judged from the request headers alone, before reading the body;
    // the token is in the multipart body and must come before the file, and only after it is verified are counts
    // deducted and staging written. Otherwise a same-site, cross-origin page (another port on the same host, a sibling
    // subdomain) could auto-submit, use up the member's upload count and make the server write up to 25 MiB for
    // nothing. This site's forms and editor script all put `_csrf` before the file, and browsers submit in document
    // order.
    if let Some(reason) = auth::origin_failure(&headers) {
        return upload_error(
            &state,
            &headers,
            &mut e,
            StatusCode::FORBIDDEN,
            &stale_upload_form(&reason),
        )
        .await;
    }
    let mut uploaded = None;
    let mut description = String::new();
    let mut alt_text = String::new();
    let mut fields = 0;
    // None: no token read yet; Some(None): verified; Some(Some(reason)): verified but mismatched.
    let mut csrf: Option<Option<String>> = None;
    // The first refusal of the file part. A refused file is drained without writing a byte of it, and the fields after
    // it are still read: a form may put fields after the file (the no-JS editor puts it after its own), and an editor
    // refilled without them falls back to defaults. A members-only thread refilled as "public" becomes public when the
    // author saves again.
    let mut refused: Option<(StatusCode, String)> = None;
    let malformed = |message: &str| (StatusCode::BAD_REQUEST, message.to_owned());
    let result: Result<(), (StatusCode, String)> = async {
        while let Some(mut part) = multipart
            .next_field()
            .await
            .map_err(|_| malformed(crate::i18n::current().media_error_format()))?
        {
            fields += 1;
            // A no-JS upload carries the whole editor form, so leave room for activity and hidden fields.
            if fields > 40 {
                return Err(malformed(crate::i18n::current().media_error_fields()));
            }
            let name = part.name().unwrap_or("").to_owned();
            if name == "file" {
                let refusal = match &csrf {
                    _ if refused.is_some() => None,
                    None => Some((
                        StatusCode::FORBIDDEN,
                        stale_upload_form(crate::i18n::current().media_error_token_order()),
                    )),
                    Some(Some(reason)) => Some((StatusCode::FORBIDDEN, stale_upload_form(reason))),
                    Some(None) if uploaded.is_some() => {
                        Some(malformed(crate::i18n::current().media_error_one_file()))
                    }
                    Some(None) => match upload_allowed(&state, session.member.uid).await {
                        Ok(true) => None,
                        Ok(false) => Some((
                            StatusCode::TOO_MANY_REQUESTS,
                            crate::i18n::current().media_error_upload_rate().into(),
                        )),
                        Err(unavailable) => Some(unavailable),
                    },
                };
                if refused.is_some() || refusal.is_some() {
                    if refused.is_none() {
                        refused = refusal;
                    }
                    drain(&mut part).await?;
                    continue;
                }
                let filename = part.file_name().unwrap_or("").to_owned();
                let (path, mut file) = match state.media.staging_file().await {
                    Ok(staging) => staging,
                    Err(error) => {
                        refused = Some(malformed(&error.to_string()));
                        drain(&mut part).await?;
                        continue;
                    }
                };
                uploaded = Some((path, filename));
                let mut bytes = 0;
                let mut failure = None;
                while let Some(chunk) = part
                    .chunk()
                    .await
                    .map_err(|_| malformed(crate::i18n::current().media_error_interrupted()))?
                {
                    if failure.is_some() {
                        continue;
                    }
                    bytes += chunk.len() as u64;
                    if bytes > state.config.media.image_max_bytes {
                        failure = Some(crate::i18n::current().media_error_too_large());
                    } else if file.write_all(&chunk).await.is_err() {
                        failure = Some(crate::i18n::current().media_error_staging());
                    }
                }
                if failure.is_none() && file.sync_all().await.is_err() {
                    failure = Some(crate::i18n::current().media_error_staging());
                }
                if let Some(message) = failure {
                    drop(file);
                    if let Some((path, _)) = uploaded.take() {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    refused = Some(malformed(message));
                }
            } else {
                let mut value = Vec::new();
                while let Some(chunk) = part
                    .chunk()
                    .await
                    .map_err(|_| malformed(crate::i18n::current().media_error_field()))?
                {
                    value.extend_from_slice(&chunk);
                    if value.len() > 60000 {
                        return Err(malformed(crate::i18n::current().media_error_field_long()));
                    }
                }
                let value = String::from_utf8(value)
                    .map_err(|_| malformed(crate::i18n::current().media_error_utf8()))?;
                match name.as_str() {
                    // Only the first token counts: a verified token cannot be overwritten by a later field of the same
                    // name.
                    "_csrf" if csrf.is_some() => {
                        return Err(malformed(crate::i18n::current().media_error_field()));
                    }
                    "_csrf" => csrf = Some(auth::token_failure(&state, &value)),
                    "description" => description = value,
                    "alt_text" => alt_text = value,
                    _ => field(&mut e, &name, value),
                }
            }
        }
        match csrf {
            Some(None) => Ok(()),
            Some(Some(reason)) => Err((StatusCode::FORBIDDEN, stale_upload_form(&reason))),
            None => {
                let reason = auth::token_failure(&state, "").unwrap_or_default();
                Err((StatusCode::FORBIDDEN, stale_upload_form(&reason)))
            }
        }
    }
    .await;
    // A token failure found after the file outranks the refusal of the file itself.
    let result = result.and(refused.map_or(Ok(()), Err));
    if let Err((status, message)) = result {
        if let Some((path, _)) = uploaded {
            let _ = tokio::fs::remove_file(path).await;
        }
        return upload_error(&state, &headers, &mut e, status, &message).await;
    }
    let Some((path, filename)) = uploaded else {
        return upload_error(
            &state,
            &headers,
            &mut e,
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().media_error_no_file(),
        )
        .await;
    };
    let input = NewUpload {
        request_key: e.request_key.clone(),
        filename,
        description,
        alt_text,
        path,
    };
    match state
        .media
        .register_upload_actor(session.member.actor(), input)
        .await
    {
        Ok(upload) => {
            if posting::hx(&headers) {
                attachment_response(&upload)
            } else {
                e.prepare();
                upload_page(&state, &upload, &e)
            }
        }
        Err(err) if posting::hx(&headers) => error(&state, err),
        // No-JS editor: any failure returns to the editor with the author's title and body intact; status code and text
        // follow the error type, and the raw text of internal errors goes only to the log.
        Err(err) => {
            let (status, message) = super::media_message(err);
            e.prepare();
            e.message = message;
            posting::editor_page(&state, &e, status).await
        }
    }
}
async fn status(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    AxumPath(aid): AxumPath<i64>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    let Some(session) = viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let upload = if let Some(pid) = query.pid {
        match state
            .media
            .upload_for_post(session.member.actor(), aid, pid)
            .await
        {
            Ok(upload) => Ok(upload),
            Err(_) => {
                state
                    .media
                    .upload_for_actor(session.member.actor(), aid)
                    .await
            }
        }
    } else {
        state
            .media
            .upload_for_actor(session.member.actor(), aid)
            .await
    };
    match upload {
        Ok(upload) => attachment_response(&upload),
        Err(err) => error(&state, err),
    }
}
#[derive(Default, Deserialize)]
/// A form with only the token and no other fields.
struct TokenOnly {
    #[serde(default)]
    _csrf: String,
}
csrf_forms!(TokenOnly);
async fn cancel(
    Page(state): Page,
    AxumPath(aid): AxumPath<i64>,
    CsrfForm(_): CsrfForm<TokenOnly>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match state.media.cancel_actor(session.member.actor(), aid).await {
        Ok(()) => Html(format!(
            "<div data-attachment-id=\"{aid}\" data-state=\"purging\">{}</div>",
            crate::i18n::Markup::text(crate::i18n::current().media_cancelled())
        ))
        .into_response(),
        Err(err) => error(&state, err),
    }
}
async fn continue_editor(
    Page(state): Page,
    AxumPath(aid): AxumPath<i64>,
    headers: HeaderMap,
    form: CheckedForm<Editor>,
) -> Response {
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
            return upload_error(
                &state,
                &headers,
                &mut e,
                StatusCode::FORBIDDEN,
                &crate::i18n::current().editor_error_stale(reason),
            )
            .await;
        }
    };
    let Some(session) = &state.viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match state
        .media
        .upload_for_actor(session.member.actor(), aid)
        .await
    {
        Ok(upload) => {
            e.prepare();
            e.preserve_input = true;
            if e.edit_pid > 0 {
                match super::editing::service(&state)
                    .editable(session.member.actor(), e.edit_pid)
                    .await
                {
                    Ok(post) => e.management = post.can_manage && !post.can_author_edit,
                    Err(err) => {
                        let (status, message) = posting::write_error(err);
                        e.message = message;
                        return posting::failure(&state, &headers, &e, status).await;
                    }
                }
            }
            if upload.is_ready() {
                if !markdown::references(&e.body).is_ok_and(|r| r.attachments.contains(&aid)) {
                    e.body.push_str(&format!("\n\n{}", upload.markdown()));
                }
                // Upload and post keys have different scopes; a subsequent upload is a new request.
                e.request_key = identity::random_token();
                posting::editor_page(&state, &e, StatusCode::OK).await
            } else if matches!(upload.lifecycle.as_str(), "rejected" | "purging" | "purged") {
                e.request_key = identity::random_token();
                e.message = upload.scan_detail.clone();
                posting::editor_page(&state, &e, StatusCode::UNPROCESSABLE_ENTITY).await
            } else {
                upload_page(&state, &upload, &e)
            }
        }
        Err(err) => error(&state, err),
    }
}
#[derive(Default, Deserialize)]
struct DownloadQuery {
    pid: Option<i64>,
}
async fn download(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    method: axum::http::Method,
    headers: HeaderMap,
    uri: axum::http::Uri,
    AxumPath(aid): AxumPath<i64>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    // When a link boosted by hx-boost (e.g. a hand-written `<a href="/media/…">` in a post body) is followed, htmx would
    // swap the file content into body as a page. The object is not read here; htmx is told to fall back to a native
    // browser navigation and request again, so files download and images open as usual.
    if posting::hx(&headers) {
        let target = uri
            .path_and_query()
            .map_or(uri.path(), |value| value.as_str());
        if let Ok(target) = HeaderValue::from_str(&crate::site::href(target)) {
            return (
                [
                    (axum::http::HeaderName::from_static("hx-redirect"), target),
                    (
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("private, no-store"),
                    ),
                ],
                (),
            )
                .into_response();
        }
    }
    let image = match state
        .media
        .authorize_image(aid, query.pid, media_viewer(&viewer))
        .await
    {
        Ok(image) => image,
        Err(err) => return error(&state, err),
    };
    // Check permission before comparing entity tags: revalidation after a withdrawal or hiding gets 404 instead of
    // staying exposed through a 304.
    let etag = image.etag();
    let cache = |response: &mut Response| {
        if let Ok(value) = HeaderValue::from_str(&etag) {
            response.headers_mut().insert(header::ETAG, value);
        }
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(crate::media::IMAGE_CACHE_CONTROL),
        );
    };
    if super::if_none_match(&headers).is_some_and(|value| crate::media::etag_matches(value, &etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        cache(&mut response);
        return response;
    }
    let content_type = HeaderValue::from_str(&image.upload.mime_type).ok();
    // HEAD returns headers only: type and size come from the attachment row (processing writes the object's actual
    // size), without reading the object.
    if method == axum::http::Method::HEAD {
        let mut response = StatusCode::OK.into_response();
        if let Some(value) = content_type {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from(image.upload.filesize),
        );
        cache(&mut response);
        return response;
    }
    match state.media.open_image(&image).await {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(file.body.into_async_read());
            let mut response = Body::from_stream(stream).into_response();
            if let Some(value) = content_type {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&file.filesize.to_string()).expect("file size"),
            );
            cache(&mut response);
            response
        }
        Err(err) => error(&state, err),
    }
}

/// Thread list thumbnails: permission is decided by the current thread, first post and forum state, and
/// anything unreadable or missing is 404; image requests have no use for an HTML error page, so a line of text is
/// returned.
async fn thumbnail(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    headers: HeaderMap,
    AxumPath((tid, aid)): AxumPath<(i64, i64)>,
) -> Response {
    match state
        .media
        .thread_thumbnail(
            tid,
            aid,
            media_viewer(&viewer),
            super::if_none_match(&headers),
        )
        .await
    {
        Ok(image) => super::display_image_response(image),
        Err(error) => {
            let (status, message) = match error {
                MediaError::NotFound => (
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().media_thumbnail_missing(),
                ),
                MediaError::Unavailable(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    crate::i18n::current().media_thumbnail_unavailable(),
                ),
                other => {
                    tracing::warn!(error = %other, "failed to read thumbnail");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        crate::i18n::current().media_thumbnail_failed(),
                    )
                }
            };
            (
                status,
                [(header::CACHE_CONTROL, "private, no-store")],
                format!("{message}\n"),
            )
                .into_response()
        }
    }
}

/// The visitor already verified by the session layer for this request ([`crate::media::MediaViewer`]): reading
/// attachments does not reread the member row.
pub(super) fn media_viewer(viewer: &auth::Viewer) -> Option<crate::media::MediaViewer> {
    viewer
        .session
        .as_ref()
        .map(|session| crate::media::MediaViewer {
            actor: session.member.actor(),
            adminid: session.member.adminid,
            contributes: session.member.can_contribute(),
        })
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct StorageForm {
    _csrf: String,
    revision: i64,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    path_style: String,
    access_key: String,
    secret_key: String,
    operation: String,
}
csrf_forms!(StorageForm);
#[derive(Template)]
#[template(path = "manage/storage.html")]
struct StorageTemplate<'a> {
    p: PageMeta<'a>,
    settings: StorageSettings,
    message: String,
    /// Refilled after a test or a refused save; the Secret Key never is.
    access_key: String,
    /// A Secret Key was typed but not saved: the page asks for it again before saving.
    secret_dropped: bool,
}
/// What the administrator submitted, shown again instead of the saved configuration when the submission did not
/// end in a save, so a failed test or a refused save does not empty the form.
struct Submitted {
    revision: i64,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    path_style: bool,
    access_key: String,
    secret_typed: bool,
}
async fn storage_page(Page(state): Page) -> Response {
    storage_render(&state, String::new(), StatusCode::OK, None).await
}
async fn storage_render(
    state: &AppState,
    message: String,
    status: StatusCode,
    submitted: Option<Submitted>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/storage");
    };
    match state.media.settings_actor(session.member.actor()).await {
        Ok(mut settings) => {
            let (access_key, secret_dropped) = match submitted {
                Some(submitted) => {
                    settings.revision = submitted.revision;
                    settings.endpoint = submitted.endpoint;
                    settings.region = submitted.region;
                    settings.bucket = submitted.bucket;
                    settings.prefix = submitted.prefix;
                    settings.path_style = submitted.path_style;
                    (submitted.access_key, submitted.secret_typed)
                }
                None => (String::new(), false),
            };
            let mut response = render(&StorageTemplate {
                p: meta(
                    state,
                    crate::i18n::current().media_manage_storage_heading().into(),
                    crate::i18n::current()
                        .media_manage_storage_description()
                        .into(),
                    "/manage/storage",
                    "",
                    "",
                ),
                settings,
                message,
                access_key,
                secret_dropped,
            });
            *response.status_mut() = status;
            response
        }
        Err(err) => error(state, err),
    }
}
async fn storage_save(Page(state): Page, CsrfForm(form): CsrfForm<StorageForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let input = StorageInput {
        revision: form.revision,
        endpoint: form.endpoint,
        region: form.region,
        bucket: form.bucket,
        prefix: form.prefix,
        path_style: matches!(form.path_style.as_str(), "true" | "on"),
        access_key: form.access_key,
        secret_key: form.secret_key,
    };
    let submitted = Submitted {
        revision: form.revision,
        endpoint: input.endpoint.clone(),
        region: input.region.clone(),
        bucket: input.bucket.clone(),
        prefix: input.prefix.clone(),
        path_style: input.path_style,
        access_key: input.access_key.clone(),
        secret_typed: !input.secret_key.is_empty(),
    };
    let testing = form.operation == "test";
    let result = if testing {
        state
            .media
            .test_settings_actor(session.member.actor(), input)
            .await
            .map(|_| crate::i18n::current().media_manage_storage_tested())
    } else {
        state
            .media
            .save_settings_actor(session.member.actor(), input)
            .await
            .map(|_| crate::i18n::current().media_manage_storage_saved())
    };
    match result.map_err(super::media_form_failure) {
        // A successful test saved nothing: keep the form for the save that follows.
        Ok(message) if testing => {
            storage_render(&state, message.into(), StatusCode::OK, Some(submitted)).await
        }
        Ok(message) => storage_render(&state, message.into(), StatusCode::OK, None).await,
        Err(Ok((status, message))) => {
            storage_render(&state, message, status, Some(submitted)).await
        }
        Err(Err(err)) => error(&state, err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-JS upload re-parses the whole editor as multipart text fields; one missing branch in `field` silently drops
    /// a field after the round trip, so each name the form actually submits is pinned here.
    #[test]
    fn the_upload_fallback_carries_every_editor_field_through_multipart() {
        let mut e = Editor::default();
        for (name, value) in [
            ("title", "Title"),
            ("body", "Body"),
            ("fid", "8"),
            ("category", "2"),
            ("budget", "100"),
            ("shares", "10"),
            ("distribution", "random"),
            ("deadline", "2026-09-19T12:00"),
            ("max_choices", "2"),
            ("options", "First\nSecond"),
            ("source_name", "Source"),
            ("source_url", "https://example.test/a"),
            ("source_author", "Author"),
        ] {
            field(&mut e, name, value.to_owned());
        }
        assert_eq!((e.fid, e.category, e.budget, e.shares), (8, 2, 100, 10));
        assert_eq!(e.max_choices, 2);
        assert_eq!(e.distribution, "random");
        assert_eq!(e.deadline, "2026-09-19T12:00");
        assert_eq!(e.options, "First\nSecond");
        assert_eq!(e.source_name, "Source");
        assert_eq!(e.source_url, "https://example.test/a");
        assert_eq!(e.source_author, "Author");
    }
}
