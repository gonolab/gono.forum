//! Profile, email change, and avatar settings that pass media checks.
use axum::extract::Multipart;
use tokio::io::AsyncWriteExt;

use super::super::{AvatarView, avatar_view};
use super::*;
use crate::media::{MediaError, NewUpload, Upload};
use crate::rate_limit::Rule;
use crate::router::extract::{CheckedForm, Page, csrf_forms};

const PROFILE_LIMIT: (u32, i64) = (30, 300);
const EMAIL_ACCOUNT_LIMIT: (u32, i64) = (1, 60);
const EMAIL_IP_LIMIT: (u32, i64) = (10, 3600);
const EMAIL_CONFIRM_IP_LIMIT: (u32, i64) = (30, 300);
const EMAIL_CONFIRM_TOKEN_LIMIT: (u32, i64) = (10, 300);
const AVATAR_LIMIT: (u32, i64) = (20, 3600);

pub(super) fn routes(upload_limit: usize) -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/profile", get(profile_page).post(save_profile))
        .route("/my/profile/privacy", post(save_privacy))
        .route("/my/profile/email", post(request_email))
        .route(
            "/confirm-email-change",
            get(confirm_page).post(confirm_email),
        )
        .route(
            "/my/profile/avatar",
            post(upload_avatar).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/my/profile/avatar/{aid}",
            get(avatar_status).post(apply_avatar),
        )
        .route("/my/profile/avatar/remove", post(remove_avatar))
        .route("/avatar/{uid}/{aid}", get(public_avatar))
        .route("/avatar/{uid}", get(member_avatar))
}

/// A 1×1 transparent PNG: when list avatars are served by uid, members without an avatar get this one (the text block
/// shows through).
const BLANK_AVATAR: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xe9, 0xfa, 0xdc, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];
const BLANK_AVATAR_ETAG: &str = "\"avatar-none\"";

/// Serves the current avatar by uid (for lists; thread rows hold no avatar copy): with an avatar
/// it is the same as `/avatar/{uid}/{aid}` (scaling, `ETag` negotiation); without one, the transparent image. Cache
/// headers are always [`crate::media::IMAGE_CACHE_CONTROL`]: after an avatar change, others get the new one at most 5
/// minutes later when they come back to revalidate.
async fn member_avatar(
    State(state): State<Arc<AppState>>,
    Path(uid): Path<i64>,
    headers: axum::http::HeaderMap,
) -> Response {
    let if_none_match = super::super::if_none_match(&headers);
    match state.media.current_avatar_image(uid, if_none_match).await {
        Ok(Some(image)) => super::super::display_image_response(image),
        Ok(None) => {
            let cached = if_none_match
                .is_some_and(|value| crate::media::etag_matches(value, BLANK_AVATAR_ETAG));
            let mut response = if cached {
                StatusCode::NOT_MODIFIED.into_response()
            } else {
                ([(header::CONTENT_TYPE, "image/png")], BLANK_AVATAR).into_response()
            };
            let headers = response.headers_mut();
            headers.insert(header::ETAG, HeaderValue::from_static(BLANK_AVATAR_ETAG));
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(crate::media::IMAGE_CACHE_CONTROL),
            );
            response
        }
        Err(error) => {
            tracing::warn!(error = %error, "failed to read avatar");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CACHE_CONTROL, "private, no-store")],
                format!(
                    "{}\n",
                    crate::i18n::current().settings_error_avatar_unavailable()
                ),
            )
                .into_response()
        }
    }
}

fn rule(scope: &'static str, key: impl std::fmt::Display, (attempts, window): (u32, i64)) -> Rule {
    Rule::new(scope, key, attempts, window)
}

fn stale_session(state: &AppState) -> Response {
    security::signed_out(state, "/login?session_ended=true")
}

#[derive(Default, Deserialize)]
struct ProfileForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    bio: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    token: String,
}
csrf_forms!(ProfileForm);

#[derive(Default, Deserialize)]
struct ProfileQuery {
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    privacy_saved: bool,
    #[serde(default)]
    email_sent: bool,
    #[serde(default)]
    avatar_saved: bool,
    #[serde(default)]
    avatar_removed: bool,
    #[serde(default)]
    token: String,
}

#[derive(Template)]
#[template(path = "settings/profile.html")]
struct ProfileTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    form: ProfileForm,
    space_visibility: &'static str,
    avatar: AvatarView,
    current_email: String,
    pending_email: String,
    username_available: bool,
    username_available_at: String,
    cooldown: String,
    can_upload: bool,
    has_avatar: bool,
    request_key: String,
    image_max_mb: u64,
    message: &'a str,
    success: bool,
}

async fn profile_response(
    state: &AppState,
    submitted: Option<ProfileForm>,
    message: &str,
    status: StatusCode,
) -> Response {
    if state.viewer.session.is_none() {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    }
    let profile = match state.identity.profile(&state.viewer.raw_sid).await {
        Ok(profile) => profile,
        Err(IdentityError::Unauthorized) => return stale_session(state),
        Err(error) => return identity_error(state, error),
    };
    let member = &profile.member;
    let mut form = submitted.unwrap_or_else(|| ProfileForm {
        username: member.username.clone(),
        bio: profile.bio,
        ..Default::default()
    });
    // Right after a rename completed on another page, this page's old form can still keep the bio and save again.
    if profile.username_available_at > identity::now() {
        form.username = member.username.clone();
    }
    let mut p = meta(
        state,
        crate::i18n::current().frame_my_profile().into(),
        crate::i18n::current()
            .settings_profile_description(crate::site::site_name(&state.frame.settings)),
        "/my/profile",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&ProfileTemplate {
        p,
        frame: crate::router::my::frame("profile"),
        form,
        space_visibility: profile.space_visibility.value(),
        avatar: avatar_view(
            Some(member.uid),
            member.avatar_aid.unwrap_or_default(),
            member.avatar,
            &member.username,
        ),
        current_email: member
            .email
            .clone()
            .unwrap_or_else(|| crate::i18n::current().settings_no_email().into()),
        pending_email: profile.pending_email.unwrap_or_default(),
        username_available: profile.username_available_at <= identity::now(),
        username_available_at: display_time(profile.username_available_at),
        cooldown: display_duration(
            state
                .frame
                .settings
                .username
                .cooldown_days
                .saturating_mul(86_400),
        ),
        can_upload: member.can_contribute(),
        has_avatar: member.avatar_aid.is_some(),
        request_key: identity::random_token(),
        image_max_mb: state.config.media.image_max_bytes / 1024 / 1024,
        message,
        success: status == StatusCode::OK,
    });
    *response.status_mut() = status;
    response
}

async fn profile_page(
    Page(state): Page,
    query: Result<Query<ProfileQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return profile_response(
            &state,
            None,
            crate::i18n::current().account_error_page_params(),
            StatusCode::BAD_REQUEST,
        )
        .await;
    };
    let message = if query.saved {
        crate::i18n::current().settings_profile_saved()
    } else if query.privacy_saved {
        crate::i18n::current().settings_privacy_saved()
    } else if query.email_sent {
        crate::i18n::current().settings_email_sent()
    } else if query.avatar_saved {
        crate::i18n::current().settings_avatar_saved()
    } else if query.avatar_removed {
        crate::i18n::current().settings_avatar_removed()
    } else {
        ""
    };
    profile_response(&state, None, message, StatusCode::OK).await
}

#[derive(Default, Deserialize)]
struct PrivacyForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    space_visibility: String,
}
csrf_forms!(PrivacyForm);

async fn save_privacy(Page(state): Page, form: CheckedForm<PrivacyForm>) -> Response {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(..) => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().settings_error_stale_check(),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state
        .limit(&[rule("privacy-update", session.member.uid, PROFILE_LIMIT)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().settings_error_save_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .update_space_visibility(&state.viewer.raw_sid, &form.space_visibility)
        .await
    {
        Ok(()) => redirect("/my/profile?privacy_saved=true"),
        Err(IdentityError::Unauthorized) => stale_session(&state),
        Err(IdentityError::Validation(message)) => {
            profile_response(&state, None, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn save_profile(Page(state): Page, form: CheckedForm<ProfileForm>) -> Response {
    // Always clear the current password before refilling the form; failure paths do not write it back to the page
    // either.
    let mut form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(mut form, reason) => {
            form.current_password.clear();
            return profile_response(
                &state,
                Some(form),
                &crate::i18n::current().settings_error_stale_check_reason(reason),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    form.current_password.clear();
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state
        .limit(&[rule("profile-update", session.member.uid, PROFILE_LIMIT)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return profile_response(
                &state,
                Some(form),
                crate::i18n::current().settings_error_save_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .update_profile(&state.viewer.raw_sid, &form.username, &form.bio)
        .await
    {
        Ok(()) => redirect("/my/profile?saved=true"),
        Err(IdentityError::Unauthorized) => stale_session(&state),
        Err(IdentityError::Validation(message)) => {
            profile_response(
                &state,
                Some(form),
                message,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(IdentityError::Conflict(message)) => {
            profile_response(&state, Some(form), message, StatusCode::CONFLICT).await
        }
        Err(IdentityError::Restricted(message)) => {
            profile_response(&state, Some(form), &message, StatusCode::FORBIDDEN).await
        }
        Err(error) => identity_error(&state, error),
    }
}

async fn request_email(Page(state): Page, form: CheckedForm<ProfileForm>) -> Response {
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(..) => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().settings_error_stale_current_password(),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    };
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state
        .limit(&[
            rule(
                "email-change-account",
                session.member.uid,
                EMAIL_ACCOUNT_LIMIT,
            ),
            rule("email-change-ip", &state.viewer.client_ip, EMAIL_IP_LIMIT),
        ])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return profile_response(
                &state,
                None,
                crate::i18n::current().account_error_apply_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match state
        .identity
        .request_email_change(&state.viewer.raw_sid, &form.current_password, &form.email)
        .await
    {
        Ok(()) => redirect("/my/profile?email_sent=true"),
        Err(IdentityError::Unauthorized) => stale_session(&state),
        Err(IdentityError::Validation(message)) => {
            profile_response(&state, None, message, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(IdentityError::Conflict(message)) => {
            profile_response(&state, None, message, StatusCode::CONFLICT).await
        }
        Err(error) => identity_error(&state, error),
    }
}

#[derive(Template)]
#[template(path = "auth/email_change.html")]
struct EmailTemplate<'a> {
    p: PageMeta<'a>,
    email: String,
    token: &'a str,
    message: &'a str,
    ttl: String,
}

fn email_response(
    state: &AppState,
    email: Option<String>,
    token: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut response = render(&EmailTemplate {
        p: meta(
            state,
            crate::i18n::current().account_email_change_title().into(),
            crate::i18n::current()
                .account_email_change_description(crate::site::site_name(&state.frame.settings)),
            "/confirm-email-change",
            "",
            "",
        ),
        token: if email.is_some() { token } else { "" },
        email: email.unwrap_or_default(),
        message,
        ttl: display_duration(
            state
                .frame
                .settings
                .account
                .email_change_ttl_hours
                .saturating_mul(3600),
        ),
    });
    *response.status_mut() = status;
    token_page_response(response, true)
}

async fn confirm_page(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<Viewer>,
    query: Result<Query<ProfileQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let state = match page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return token_page_response(response, true),
    };
    let Ok(Query(query)) = query else {
        return email_response(
            &state,
            None,
            "",
            crate::i18n::current().account_error_email_change_params(),
            StatusCode::BAD_REQUEST,
        );
    };
    match state.identity.email_change_status(&query.token).await {
        Ok(Some(email)) => email_response(&state, Some(email), &query.token, "", StatusCode::OK),
        Ok(None) => email_response(
            &state,
            None,
            "",
            crate::i18n::current().account_error_email_change_link(),
            StatusCode::BAD_REQUEST,
        ),
        Err(error) => token_page_response(identity_error(&state, error), true),
    }
}

async fn confirm_email(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<Viewer>,
    form: CheckedForm<ProfileForm>,
) -> Response {
    let state = match page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return token_page_response(response, true),
    };
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return email_response(
                &state,
                None,
                "",
                crate::i18n::current().common_error_form(),
                StatusCode::BAD_REQUEST,
            );
        }
        CheckedForm::StaleToken(..) => {
            return email_response(
                &state,
                None,
                "",
                crate::i18n::current().account_error_stale_email_link(),
                StatusCode::FORBIDDEN,
            );
        }
    };
    match state
        .limit(&[
            rule(
                "email-confirm-ip",
                &state.viewer.client_ip,
                EMAIL_CONFIRM_IP_LIMIT,
            ),
            rule(
                "email-confirm-token",
                identity::token_hash(&form.token),
                EMAIL_CONFIRM_TOKEN_LIMIT,
            ),
        ])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return email_response(
                &state,
                None,
                "",
                crate::i18n::current().account_error_too_many(),
                StatusCode::TOO_MANY_REQUESTS,
            );
        }
        Err(unavailable) => return unavailable,
    }
    match state.identity.confirm_email_change(&form.token).await {
        Ok(()) => token_page_response(
            security::signed_out(&state, "/login?email_changed=true"),
            true,
        ),
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            email_response(&state, None, "", message, StatusCode::UNPROCESSABLE_ENTITY)
        }
        Err(error) => token_page_response(identity_error(&state, error), true),
    }
}

fn media_error(state: &AppState, error: MediaError) -> Response {
    super::super::media_failure(state, error)
}

fn stale_avatar_form(reason: &str) -> String {
    crate::i18n::current().settings_error_stale_avatar(reason)
}

async fn upload_avatar(
    Page(state): Page,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    if !session.member.can_contribute() {
        return profile_response(
            &state,
            None,
            crate::login::Participation::current()
                .message(crate::i18n::current().settings_error_avatar_need_email()),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    // Both origin and token must be verified before deducting quota and writing to staging: otherwise other pages of
    // the same site (other ports on the same host, sibling subdomains) could auto-submit and use up the member's hourly
    // uploads, making the server write up to 27 MiB for nothing.
    // The origin only looks at request headers and is checked before reading the body; the token is in the multipart
    // body, so it can only be required to come before the file. This site's forms put `_csrf` before the file input,
    // and browsers submit in document order, so normal use is unaffected.
    if let Some(reason) = origin_failure(&headers) {
        return profile_response(
            &state,
            None,
            &stale_avatar_form(&reason),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    let mut csrf_checked = false;
    let mut request_key = String::new();
    let mut staged = None;
    let mut count = 0;
    let malformed = |message: &str| (StatusCode::BAD_REQUEST, message.to_owned());
    let result: Result<(), (StatusCode, String)> = async {
        while let Some(mut part) = multipart
            .next_field()
            .await
            .map_err(|_| malformed(crate::i18n::current().settings_error_upload_format()))?
        {
            count += 1;
            if count > 4 {
                return Err(malformed(
                    crate::i18n::current().settings_error_upload_fields(),
                ));
            }
            let name = part.name().unwrap_or_default().to_owned();
            if name == "file" {
                if !csrf_checked {
                    return Err((
                        StatusCode::FORBIDDEN,
                        stale_avatar_form(
                            crate::i18n::current().settings_error_upload_token_order(),
                        ),
                    ));
                }
                if staged.is_some() {
                    return Err(malformed(
                        crate::i18n::current().settings_error_upload_one(),
                    ));
                }
                let allowed = state
                    .limits
                    .allow(&[rule("profile-avatar", session.member.uid, AVATAR_LIMIT)])
                    .await
                    .map_err(|error| {
                        let detail = crate::debug::visible_error(&error);
                        tracing::error!(error = %detail, "cache unavailable");
                        (StatusCode::SERVICE_UNAVAILABLE, detail)
                    })?;
                if !allowed {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        crate::i18n::current()
                            .settings_error_avatar_too_many()
                            .into(),
                    ));
                }
                let filename = part.file_name().unwrap_or_default().to_owned();
                let (path, mut file) = state
                    .media
                    .staging_file()
                    .await
                    .map_err(|error| malformed(&error.to_string()))?;
                staged = Some((path, filename));
                let mut length = 0_u64;
                while let Some(chunk) = part.chunk().await.map_err(|_| {
                    malformed(crate::i18n::current().settings_error_avatar_interrupted())
                })? {
                    length += chunk.len() as u64;
                    if length > state.config.media.image_max_bytes {
                        return Err(malformed(
                            crate::i18n::current().settings_error_avatar_size(),
                        ));
                    }
                    file.write_all(&chunk).await.map_err(|_| {
                        malformed(crate::i18n::current().settings_error_avatar_staging())
                    })?;
                }
                file.sync_all().await.map_err(|_| {
                    malformed(crate::i18n::current().settings_error_avatar_staging())
                })?;
            } else {
                let mut bytes = Vec::new();
                while let Some(chunk) = part
                    .chunk()
                    .await
                    .map_err(|_| malformed(crate::i18n::current().settings_error_upload_field()))?
                {
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() > 256 {
                        return Err(malformed(
                            crate::i18n::current().settings_error_upload_field_long(),
                        ));
                    }
                }
                let value = String::from_utf8(bytes)
                    .map_err(|_| malformed(crate::i18n::current().settings_error_upload_utf8()))?;
                match name.as_str() {
                    // Only the first token counts: a verified token cannot be overridden by a later field of the same
                    // name.
                    "_csrf" if !csrf_checked => {
                        if let Some(reason) = token_failure(&state, &value) {
                            return Err((StatusCode::FORBIDDEN, stale_avatar_form(&reason)));
                        }
                        csrf_checked = true;
                    }
                    "request_key" => request_key = value,
                    _ => {
                        return Err(malformed(
                            crate::i18n::current().settings_error_upload_field(),
                        ));
                    }
                }
            }
        }
        if !csrf_checked {
            let reason = token_failure(&state, "").unwrap_or_default();
            return Err((StatusCode::FORBIDDEN, stale_avatar_form(&reason)));
        }
        Ok(())
    }
    .await;
    if let Err((status, message)) = result {
        if let Some((path, _)) = staged {
            let _ = tokio::fs::remove_file(path).await;
        }
        return profile_response(&state, None, &message, status).await;
    }
    let Some((path, filename)) = staged else {
        return profile_response(
            &state,
            None,
            crate::i18n::current().settings_error_avatar_choose(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    match state
        .media
        .register_upload_actor(
            session.member.actor(),
            NewUpload {
                request_key,
                filename,
                description: String::new(),
                alt_text: String::new(),
                path,
            },
        )
        .await
    {
        Ok(upload) => redirect(&format!("/my/profile/avatar/{}", upload.aid)),
        Err(error) => match super::super::media_form_failure(error) {
            Ok((status, message)) => profile_response(&state, None, &message, status).await,
            Err(error) => media_error(&state, error),
        },
    }
}

#[derive(Template)]
#[template(path = "settings/avatar.html")]
struct AvatarTemplate<'a> {
    p: PageMeta<'a>,
    upload: Upload,
    message: &'a str,
    ready: bool,
    finished: bool,
}

fn avatar_response(
    state: &AppState,
    upload: Upload,
    message: &str,
    status: StatusCode,
) -> Response {
    let ready = upload.is_ready() && upload.mime_type.starts_with("image/");
    let finished = ready
        || matches!(upload.lifecycle.as_str(), "rejected" | "purging" | "purged")
        || upload.is_ready();
    let mut response = render(&AvatarTemplate {
        p: meta(
            state,
            crate::i18n::current().settings_avatar_check().into(),
            crate::i18n::current()
                .settings_avatar_check_description()
                .into(),
            "/my/profile",
            "",
            "",
        ),
        upload,
        message,
        ready,
        finished,
    });
    *response.status_mut() = status;
    response
}

async fn avatar_status(Page(state): Page, Path(aid): Path<i64>) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state
        .media
        .upload_for_actor(session.member.actor(), aid)
        .await
    {
        Ok(upload) => avatar_response(&state, upload, "", StatusCode::OK),
        Err(error) => media_error(&state, error),
    }
}

async fn apply_avatar(
    Page(state): Page,
    Path(aid): Path<i64>,
    CsrfForm(_): CsrfForm<ActionForm>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state
        .media
        .set_avatar(session.member.actor(), Some(aid))
        .await
    {
        Ok(()) => redirect("/my/profile?avatar_saved=true"),
        Err(error) => match super::super::media_form_failure(error) {
            Ok((status, message)) => match state
                .media
                .upload_for_actor(session.member.actor(), aid)
                .await
            {
                Ok(upload) => avatar_response(&state, upload, &message, status),
                Err(_) => error_page(&state, status, &message),
            },
            Err(error) => media_error(&state, error),
        },
    }
}

async fn remove_avatar(Page(state): Page, CsrfForm(_): CsrfForm<ActionForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return redirect("/login?return_to=%2Fmy%2Fprofile");
    };
    match state.media.set_avatar(session.member.actor(), None).await {
        Ok(()) => redirect("/my/profile?avatar_removed=true"),
        Err(error) => media_error(&state, error),
    }
}

async fn public_avatar(
    State(state): State<Arc<AppState>>,
    Path((uid, aid)): Path<(i64, i64)>,
    headers: axum::http::HeaderMap,
) -> Response {
    match state
        .media
        .avatar_image(uid, aid, super::super::if_none_match(&headers))
        .await
    {
        Ok(image) => super::super::display_image_response(image),
        // The avatar route does not go through the session middleware (same as brand images), so there is no page
        // frame to render, and image requests have no use for an HTML error page.
        Err(error) => {
            let (status, message) = match error {
                MediaError::NotFound => (
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().settings_error_avatar_missing(),
                ),
                MediaError::Unavailable(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    crate::i18n::current().settings_error_avatar_unavailable(),
                ),
                other => {
                    tracing::warn!(error = %other, "failed to read avatar");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        crate::i18n::current().settings_error_avatar_read(),
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
