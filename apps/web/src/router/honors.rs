//! Honor groups in the admin area, `/manage/honors` (user group decisions Q11–Q14, 2026-11-06): the group list and
//! creation, editing a group and its holders, granting and revoking per member.
//! The rules live in [`crate::honors`]; only administrators can enter.
use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::*;
use crate::{
    honors::{self, HonorInput, Saved},
    identity::{self, IdentityError},
};
use axum::{extract::Path, routing::post};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/honors", get(honors_page).post(create))
        .route("/manage/honors/{id}", get(honor_page).post(update))
        .route("/manage/honors/members/{uid}", get(member_page).post(grant))
        .route("/manage/honors/members/{uid}/revoke", post(revoke))
}

fn actor(state: &AppState) -> identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .expect("AdminPage guarantees a logged-in member")
}

fn form_message(error: IdentityError) -> Result<(StatusCode, String), IdentityError> {
    match error {
        IdentityError::Validation(message) => {
            Ok((StatusCode::UNPROCESSABLE_ENTITY, message.to_owned()))
        }
        IdentityError::Conflict(message) => Ok((StatusCode::CONFLICT, message.to_owned())),
        other => Err(other),
    }
}

fn done_notice(done: &str) -> &'static str {
    match done {
        "created" => crate::i18n::current().profile_manage_done_created(),
        "saved" => crate::i18n::current().profile_manage_done_saved(),
        "unchanged" => crate::i18n::current().profile_manage_done_unchanged(),
        "granted" => crate::i18n::current().profile_manage_done_granted(),
        "revoked" => crate::i18n::current().profile_manage_done_revoked(),
        _ => "",
    }
}

/// The expiry date is written in the form as "the last valid day": what is stored is midnight of the next day
/// (site time zone), the same as for verified members.
fn expiry_text(expires_at: Option<i64>, clock: &crate::clock::TimeFormat) -> String {
    expires_at.map_or_else(
        || crate::i18n::current().profile_manage_no_expiry().to_owned(),
        |at| crate::clock::date(at - 1, clock),
    )
}

#[derive(Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
    #[serde(default)]
    uid: Option<i64>,
    #[serde(default)]
    page: Option<i64>,
}

#[derive(Default, Deserialize)]
struct HonorForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    displayorder: String,
    /// Checkbox: submits `1` when checked, nothing when unchecked.
    #[serde(default)]
    enabled: String,
    #[serde(default)]
    version: i64,
}
csrf_forms!(HonorForm);

impl HonorForm {
    fn input(&self) -> Result<HonorInput, &'static str> {
        let displayorder = if self.displayorder.trim().is_empty() {
            0
        } else {
            self.displayorder
                .trim()
                .parse()
                .map_err(|_| crate::i18n::current().profile_manage_error_order())?
        };
        Ok(HonorInput {
            name: self.name.clone(),
            description: self.description.clone(),
            displayorder,
            enabled: !self.enabled.is_empty(),
        })
    }
}

struct HonorRow {
    id: i64,
    name: String,
    description: String,
    displayorder: i64,
    enabled: bool,
}

#[derive(Template)]
#[template(path = "manage/honors.html")]
struct HonorsTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<HonorRow>,
    form: HonorForm,
    notice: &'static str,
    message: String,
}

async fn honors_view(
    state: &AppState,
    form: Option<HonorForm>,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let rows = match honors::honors(&state.db).await {
        Ok(rows) => rows,
        Err(error) => return auth::identity_error(state, error),
    };
    let mut response = render(&HonorsTemplate {
        p: meta(
            state,
            crate::i18n::current().profile_manage_honors().into(),
            crate::i18n::current()
                .profile_manage_honors_description()
                .into(),
            "/manage/honors",
            "",
            "",
        ),
        rows: rows
            .into_iter()
            .map(|honor| HonorRow {
                id: honor.id,
                name: honor.name,
                description: honor.description,
                displayorder: honor.displayorder,
                enabled: honor.enabled,
            })
            .collect(),
        form: form.unwrap_or_else(|| HonorForm {
            enabled: "1".into(),
            ..Default::default()
        }),
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn honors_page(AdminPage(state): AdminPage, Query(query): Query<DoneQuery>) -> Response {
    if let Some(uid) = query.uid.filter(|uid| *uid > 0) {
        return auth::redirect(&format!("/manage/honors/members/{uid}"));
    }
    honors_view(
        &state,
        None,
        done_notice(&query.done),
        String::new(),
        StatusCode::OK,
    )
    .await
}

async fn create(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<HonorForm>) -> Response {
    let input = match form.input() {
        Ok(input) => input,
        Err(message) => {
            return honors_view(
                &state,
                Some(form),
                "",
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    match honors::create(&state.db, actor(&state), &input, identity::now()).await {
        // The honor group directory is in the in-process cache (floor badges read it): after saving, clear this
        // process and broadcast.
        Ok(_) => match state.global.announce(&[crate::cache::Block::Honors]).await {
            Ok(()) => auth::redirect("/manage/honors?done=created"),
            Err(error) => database_error(&state, error),
        },
        Err(error) => match form_message(error) {
            Ok((status, message)) => honors_view(&state, Some(form), "", message, status).await,
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

struct HolderRow {
    uid: i64,
    username: String,
    granted: String,
    expires: String,
}

#[derive(Template)]
#[template(path = "manage/honor.html")]
struct HonorTemplate<'a> {
    p: PageMeta<'a>,
    id: i64,
    form: HonorForm,
    holders: Vec<HolderRow>,
    previous: String,
    next: String,
    notice: &'static str,
    message: String,
}

async fn honor_view(
    state: &AppState,
    id: i64,
    page: i64,
    form: Option<HonorForm>,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let honor = match honors::honor(&state.db, id).await {
        Ok(Some(honor)) => honor,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().profile_manage_error_no_honor(),
            );
        }
        Err(error) => return auth::identity_error(state, error),
    };
    let (holders, more) = match honors::holders(&state.db, id, page).await {
        Ok(found) => found,
        Err(IdentityError::NotFound) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().common_error_no_such_page(),
            );
        }
        Err(error) => return auth::identity_error(state, error),
    };
    let clock = crate::clock::current();
    let mut response = render(&HonorTemplate {
        p: meta(
            state,
            crate::i18n::current().profile_manage_honor_title(&honor.name),
            crate::i18n::current()
                .profile_manage_honor_description()
                .into(),
            &format!("/manage/honors/{id}"),
            "",
            "",
        ),
        id,
        form: form.unwrap_or_else(|| HonorForm {
            name: honor.name.clone(),
            description: honor.description.clone(),
            displayorder: honor.displayorder.to_string(),
            enabled: if honor.enabled {
                "1".into()
            } else {
                String::new()
            },
            version: honor.version,
            ..Default::default()
        }),
        holders: holders
            .into_iter()
            .map(|holder| HolderRow {
                uid: holder.uid,
                username: holder.username,
                granted: crate::clock::date(holder.granted_at, &clock),
                expires: expiry_text(holder.expires_at, &clock),
            })
            .collect(),
        previous: if page > 1 {
            format!("/manage/honors/{id}?page={}", page - 1)
        } else {
            String::new()
        },
        next: if more {
            format!("/manage/honors/{id}?page={}", page + 1)
        } else {
            String::new()
        },
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn honor_page(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    Query(query): Query<DoneQuery>,
) -> Response {
    honor_view(
        &state,
        id,
        query.page.unwrap_or(1),
        None,
        done_notice(&query.done),
        String::new(),
        StatusCode::OK,
    )
    .await
}

async fn update(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<HonorForm>,
) -> Response {
    let input = match form.input() {
        Ok(input) => input,
        Err(message) => {
            return honor_view(
                &state,
                id,
                1,
                Some(form),
                "",
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    match honors::update(
        &state.db,
        actor(&state),
        id,
        form.version,
        &input,
        identity::now(),
    )
    .await
    {
        Ok(Saved::Unchanged) => auth::redirect(&format!("/manage/honors/{id}?done=unchanged")),
        Ok(_) => match state.global.announce(&[crate::cache::Block::Honors]).await {
            Ok(()) => auth::redirect(&format!("/manage/honors/{id}?done=saved")),
            Err(error) => database_error(&state, error),
        },
        Err(IdentityError::NotFound) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().profile_manage_error_no_honor(),
        ),
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                honor_view(&state, id, 1, Some(form), "", message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

#[derive(Default, Deserialize)]
struct GrantForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    honor_id: i64,
    /// `<input type="date">`: the last valid day; empty means no expiry.
    #[serde(default)]
    expires: String,
    #[serde(default)]
    reason: String,
}
csrf_forms!(GrantForm);

struct GrantRow {
    honor_id: i64,
    name: String,
    granted: String,
    expires: String,
    /// Form value of the validity (the last valid day), used to prefill the renewal form.
    expires_value: String,
    state: &'static str,
}

struct Choice {
    id: i64,
    name: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "manage/honor_member.html")]
struct MemberTemplate<'a> {
    p: PageMeta<'a>,
    uid: i64,
    username: String,
    member_state: &'static str,
    grants: Vec<GrantRow>,
    choices: Vec<Choice>,
    form: GrantForm,
    notice: &'static str,
    message: String,
}

async fn member_view(
    state: &AppState,
    uid: i64,
    form: Option<GrantForm>,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let subject = match crate::directory::subject(&state.db, uid).await {
        Ok(Some(subject)) => subject,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().profile_manage_error_no_member(),
            );
        }
        Err(error) => return auth::identity_error(state, error),
    };
    let (grants, groups) = match (
        honors::member_grants(&state.db, uid).await,
        honors::honors(&state.db).await,
    ) {
        (Ok(grants), Ok(groups)) => (grants, groups),
        (Err(error), _) | (_, Err(error)) => return auth::identity_error(state, error),
    };
    let clock = crate::clock::current();
    let now = identity::now();
    let form = form.unwrap_or_default();
    let mut response = render(&MemberTemplate {
        p: meta(
            state,
            crate::i18n::current().profile_manage_honors().into(),
            crate::i18n::current()
                .profile_manage_member_description()
                .into(),
            &format!("/manage/honors/members/{uid}"),
            "",
            "",
        ),
        uid,
        member_state: if subject.account_state != "active" {
            crate::i18n::current().profile_manage_state_deleting()
        } else if subject.freeze {
            crate::i18n::current().profile_manage_state_frozen()
        } else {
            crate::i18n::current().profile_manage_state_normal()
        },
        username: subject.username,
        grants: grants
            .into_iter()
            .map(|grant| GrantRow {
                honor_id: grant.honor_id,
                state: if !grant.enabled {
                    crate::i18n::current().profile_manage_hidden_disabled()
                } else if grant.expires_at.is_some_and(|at| at <= now) {
                    crate::i18n::current().profile_manage_hidden_expired()
                } else if subject.account_state != "active" {
                    crate::i18n::current().profile_manage_hidden_deleting()
                } else if subject.freeze {
                    crate::i18n::current().profile_manage_hidden_frozen()
                } else {
                    crate::i18n::current().profile_manage_shown()
                },
                name: grant.name,
                granted: crate::clock::date(grant.granted_at, &clock),
                expires: expiry_text(grant.expires_at, &clock),
                expires_value: grant
                    .expires_at
                    .map(|at| {
                        crate::clock::input_value(at - 1, &clock)
                            .chars()
                            .take(10)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
        choices: groups
            .into_iter()
            .filter(|group| group.enabled)
            .map(|group| Choice {
                selected: group.id == form.honor_id,
                id: group.id,
                name: group.name,
            })
            .collect(),
        form,
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn member_page(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    Query(query): Query<DoneQuery>,
) -> Response {
    member_view(
        &state,
        uid,
        None,
        done_notice(&query.done),
        String::new(),
        StatusCode::OK,
    )
    .await
}

async fn grant(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    CsrfForm(form): CsrfForm<GrantForm>,
) -> Response {
    let clock = crate::clock::current();
    let expires_at = if form.expires.trim().is_empty() {
        None
    } else {
        match crate::clock::parse_input(&format!("{}T00:00", form.expires.trim()), &clock) {
            Some(start) => Some(start + 86_400),
            None => {
                return member_view(
                    &state,
                    uid,
                    Some(form),
                    "",
                    crate::i18n::current().profile_manage_error_date().into(),
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await;
            }
        }
    };
    match honors::grant(
        &state.db,
        actor(&state),
        uid,
        form.honor_id,
        expires_at,
        identity::now(),
    )
    .await
    {
        Ok(Saved::Unchanged) => {
            auth::redirect(&format!("/manage/honors/members/{uid}?done=unchanged"))
        }
        Ok(Saved::Created) => auth::redirect(&format!("/manage/honors/members/{uid}?done=granted")),
        Ok(Saved::Updated) => auth::redirect(&format!("/manage/honors/members/{uid}?done=saved")),
        Err(IdentityError::NotFound) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().profile_manage_error_no_member(),
        ),
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                member_view(&state, uid, Some(form), "", message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

async fn revoke(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    CsrfForm(form): CsrfForm<GrantForm>,
) -> Response {
    match honors::revoke(
        &state.db,
        actor(&state),
        uid,
        form.honor_id,
        &form.reason,
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("/manage/honors/members/{uid}?done=revoked")),
        Err(IdentityError::NotFound) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().profile_manage_error_no_member(),
        ),
        Err(error) => match form_message(error) {
            Ok((status, message)) => member_view(&state, uid, None, "", message, status).await,
            Err(other) => auth::identity_error(&state, other),
        },
    }
}
