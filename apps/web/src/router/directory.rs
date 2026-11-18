//! The verified member directory `/verified`, the friend-link directory `/links` and applications `/links/apply`, plus
//! the admin pages `/manage/verifications` and `/manage/links`. The rules are in [`crate::directory`] and
//! [`crate::links`].
use super::extract::{AdminPage, CheckedForm, CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{
    directory::{self, Category, Filter},
    identity::{self, IdentityError},
    links::{self, LinkCategory, Status},
};
use axum::{extract::Path, routing::post};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/verified", get(verified_page))
        .route("/links", get(links_page))
        .route("/links/apply", get(apply_page).post(apply))
        .route("/links/apply/{id}/withdraw", post(withdraw))
        .route("/manage/verifications", get(verifications))
        .route(
            "/manage/verifications/{uid}",
            get(verification).post(save_verification),
        )
        .route(
            "/manage/verifications/{uid}/revoke",
            post(revoke_verification),
        )
        .route("/manage/links", get(manage_links).post(create_link))
        .route("/manage/links/{id}", get(manage_link).post(decide_link))
}

/// Window for counting application submissions (1 hour, constant); the per-account and per-IP counts are the
/// admin-area settings `settings.links.apply_per_hour` and `apply_ip_per_hour` (docs/settings-and-caching.md,
/// "Rate limits").
const APPLY_WINDOW: i64 = 3600;

/// Explanatory document: the one saved in the admin area if any, otherwise the built-in text (the same convention as
/// the terms and the contact page).
struct Explainer {
    title: String,
    body: String,
}

async fn explainer(
    state: &AppState,
    kind: &str,
    title: &str,
    body: &str,
) -> Result<Explainer, Response> {
    let doc = state
        .global
        .policy(&state.db, kind, state.cache_ttl())
        .await
        .map_err(|error| auth::identity_error(state, error))?;
    let (title, body) = if doc.updated_at == 0 {
        (title.to_owned(), body.to_owned())
    } else {
        (doc.title.clone(), doc.body.clone())
    };
    Ok(Explainer {
        title,
        body: crate::markdown::render(&body, &std::collections::BTreeMap::new(), None),
    })
}

/// The built-in explainer of the verified member directory (`policy.verified`), in this request's language.
fn verified_explainer() -> (&'static str, &'static str) {
    let t = crate::i18n::current();
    (t.policy_verified_title(), t.policy_verified_body())
}
/// Built-in text of the friend-link explanation: used when administrators have never saved `policy.links`, with
/// numbers filled in from the application rule settings.
fn links_body(rules: &crate::runtime::LinkSettings) -> String {
    crate::i18n::current().policy_links_body(
        rules.live_per_member,
        rules.reject_cooldown_days,
        rules.min_account_days,
    )
}

// ---------------------------------------------------------------------------
// Verified member directory

#[derive(Default, Deserialize)]
struct VerifiedQuery {
    category: Option<String>,
    page: Option<usize>,
}

struct MemberCard {
    username: String,
    avatar: AvatarView,
    title: String,
    site_host: String,
    site_href: String,
}

struct VerifiedSection {
    label: &'static str,
    slug: &'static str,
    members: Vec<MemberCard>,
    more: bool,
}

#[derive(Template)]
#[template(path = "site/verified.html")]
struct VerifiedTemplate<'a> {
    p: PageMeta<'a>,
    tabs: Vec<Tab>,
    sections: Vec<VerifiedSection>,
    /// Previous and next page when paging a single category; empty on the overview.
    previous: String,
    next: String,
    explainer: Explainer,
    contact_email: String,
}

fn verified_href(category: Option<Category>, page: usize) -> String {
    match (category, page) {
        (None, _) => "/verified".to_owned(),
        (Some(category), 1) => format!("/verified?category={}", category.slug()),
        (Some(category), page) => format!("/verified?category={}&page={page}", category.slug()),
    }
}

/// Verified member directory: visible to anonymous visitors; the overview has one section per
/// category with at most 50 people, and a single category can be paged.
async fn verified_page(
    Page(state): Page,
    query: Result<Query<VerifiedQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().site_verified_error_query(),
        );
    };
    let category = match query.category.as_deref() {
        None => None,
        Some(value) => match Category::parse(value) {
            Some(category) => Some(category),
            None => {
                return error_page(
                    &state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().site_verified_error_category(),
                );
            }
        },
    };
    let page = query.page.unwrap_or(1);
    if page == 0 || (category.is_none() && page != 1) {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    }
    let viewer = state.viewer.session.as_ref().map(|s| s.member.uid);
    let now = identity::now();
    let mut sections = Vec::new();
    let mut more_in_single = false;
    for item in category.map_or(Category::ALL.to_vec(), |c| vec![c]) {
        let (listed, more) = match directory::section(&state.db, item, viewer, now, page).await {
            Ok(value) => value,
            Err(error) => return auth::identity_error(&state, error),
        };
        if category.is_some() && page > 1 && listed.is_empty() {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().common_error_page_not_found(),
            );
        }
        more_in_single = more;
        sections.push(VerifiedSection {
            label: item.label(),
            slug: item.slug(),
            members: listed
                .into_iter()
                .map(|member| MemberCard {
                    avatar: avatar_view(
                        Some(member.uid),
                        member.avatar_aid.unwrap_or(0),
                        member.avatar,
                        &member.username,
                    ),
                    site_host: member.site_host(),
                    site_href: member.site_href().to_owned(),
                    username: member.username,
                    title: member.title,
                })
                .collect(),
            more,
        });
    }
    let (verified_title, verified_body) = verified_explainer();
    let explainer = match explainer(&state, "policy.verified", verified_title, verified_body).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let tabs = std::iter::once(Tab {
        label: crate::i18n::current().home_all(),
        href: verified_href(None, 1),
        active: category.is_none(),
    })
    .chain(Category::ALL.into_iter().map(|item| Tab {
        label: item.label(),
        href: verified_href(Some(item), 1),
        active: category == Some(item),
    }))
    .collect();
    let (previous, next) = match category {
        Some(item) => (
            if page > 1 {
                verified_href(Some(item), page - 1)
            } else {
                String::new()
            },
            if more_in_single {
                verified_href(Some(item), page + 1)
            } else {
                String::new()
            },
        ),
        None => (String::new(), String::new()),
    };
    let title = category.map_or(
        crate::i18n::current().frame_footer_verified().to_owned(),
        |item| crate::i18n::current().site_verified_title(item.label()),
    );
    let mut p = meta(
        &state,
        title,
        crate::i18n::current().site_verified_description().into(),
        &verified_href(category, page),
        "",
        "",
    );
    if let Some(error) = super::member_search::fill_sidebar(&state, &mut p).await {
        return error;
    }
    render(&VerifiedTemplate {
        contact_email: state.frame.settings.site.contact_email.clone(),
        p,
        tabs,
        sections,
        previous,
        next,
        explainer,
    })
}

// ---------------------------------------------------------------------------
// Friend-link directory and applications

struct LinkGroup {
    label: &'static str,
    links: Vec<links::Listed>,
}

#[derive(Template)]
#[template(path = "site/links.html")]
struct LinksTemplate<'a> {
    p: PageMeta<'a>,
    groups: Vec<LinkGroup>,
    explainer: Explainer,
    applications_open: bool,
}

/// Friend-link directory: visible to anonymous visitors, with all displayed links grouped by category.
async fn links_page(Page(state): Page) -> Response {
    let groups = match state
        .global
        .link_directory(&state.db, state.cache_ttl())
        .await
    {
        Ok(groups) => (*groups).clone(),
        Err(error) => return database_error(&state, error),
    };
    let body = links_body(&state.frame.settings.links);
    let explainer = match explainer(
        &state,
        "policy.links",
        crate::i18n::current().policy_links_title(),
        &body,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut p = meta(
        &state,
        crate::i18n::current().home_friend_links().into(),
        crate::i18n::current().site_links_lede().into(),
        "/links",
        "",
        "",
    );
    if let Some(error) = super::member_search::fill_sidebar(&state, &mut p).await {
        return error;
    }
    render(&LinksTemplate {
        applications_open: state.frame.settings.links.applications,
        p,
        groups: groups
            .into_iter()
            .map(|(category, links)| LinkGroup {
                label: category.label(),
                links,
            })
            .collect(),
        explainer,
    })
}

#[derive(Default, Deserialize)]
struct ApplyForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    backlink_url: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    request_key: String,
}
csrf_forms!(ApplyForm);

#[derive(Default, Deserialize)]
struct ApplyQuery {
    #[serde(default)]
    done: String,
}

struct CategoryOption {
    slug: &'static str,
    label: &'static str,
    selected: bool,
}

#[derive(Template)]
#[template(path = "site/link_apply.html")]
struct ApplyTemplate<'a> {
    p: PageMeta<'a>,
    explainer: Explainer,
    applications_open: bool,
    form: ApplyForm,
    categories: Vec<CategoryOption>,
    mine: Vec<links::Link>,
    /// The notice after a successful submission or withdrawal; mutually exclusive with `message` (the failure reason).
    notice: &'static str,
    message: String,
}

fn link_categories(selected: &str) -> Vec<CategoryOption> {
    LinkCategory::ALL
        .into_iter()
        .map(|item| CategoryOption {
            slug: item.slug(),
            label: item.label(),
            selected: item.slug() == selected,
        })
        .collect()
}

async fn apply_view(
    state: &AppState,
    mut form: ApplyForm,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let body = links_body(&state.frame.settings.links);
    let explainer = match explainer(
        state,
        "policy.links",
        crate::i18n::current().policy_links_title(),
        &body,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mine = match &state.viewer.session {
        Some(session) => match links::mine(&state.db, session.member.uid).await {
            Ok(mine) => mine,
            Err(error) => return auth::identity_error(state, error),
        },
        None => Vec::new(),
    };
    // A new form gets a new request key; refilling after a failure keeps the original key (nothing was written on
    // failure, so reusing it never hits an old row), and after success a 303 back to this page issues a new key.
    // Replaying the same submission only ever yields the same row.
    if form.request_key.is_empty() {
        form.request_key = identity::random_token();
    }
    let mut response = render(&ApplyTemplate {
        p: meta(
            state,
            crate::i18n::current().site_links_apply().into(),
            crate::i18n::current().site_links_apply_description().into(),
            "/links/apply",
            "",
            "",
        ),
        explainer,
        applications_open: state.frame.settings.links.applications,
        categories: link_categories(&form.category),
        form,
        mine,
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn apply_page(Page(state): Page, Query(query): Query<ApplyQuery>) -> Response {
    let notice = match query.done.as_str() {
        "submitted" => crate::i18n::current().site_links_submitted(),
        "withdrawn" => crate::i18n::current().site_links_withdrawn(),
        _ => "",
    };
    apply_view(
        &state,
        ApplyForm::default(),
        notice,
        String::new(),
        StatusCode::OK,
    )
    .await
}

/// Submits an application: requires login; checks origin and token first, then counts attempts.
async fn apply(Page(state): Page, form: CheckedForm<ApplyForm>) -> Response {
    let Some(session) = state.viewer.session.clone() else {
        return auth::redirect("/login?return_to=%2Flinks%2Fapply");
    };
    let form = match form {
        CheckedForm::Valid(form) => form,
        CheckedForm::Malformed => {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().common_error_form(),
            );
        }
        CheckedForm::StaleToken(form, reason) => {
            return apply_view(&state, form, "", reason, StatusCode::FORBIDDEN).await;
        }
    };
    match state
        .limit(&[
            Rule::new(
                "links-apply",
                session.member.uid,
                u32::try_from(state.frame.settings.links.apply_per_hour).unwrap_or(0),
                APPLY_WINDOW,
            ),
            Rule::new(
                "links-apply-ip",
                &state.viewer.client_ip,
                u32::try_from(state.frame.settings.links.apply_ip_per_hour).unwrap_or(0),
                APPLY_WINDOW,
            ),
        ])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return apply_view(
                &state,
                form,
                "",
                crate::i18n::current().site_links_error_rate().into(),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    let own = links::own_domain(state.site_url());
    let input = links::Application {
        name: form.name.clone(),
        category: form.category.clone(),
        url: form.url.clone(),
        backlink_url: form.backlink_url.clone(),
        note: form.note.clone(),
        request_key: form.request_key.clone(),
    };
    match links::apply(
        &state.db,
        session.member.actor(),
        input,
        own.as_deref(),
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect("/links/apply?done=submitted"),
        Err(IdentityError::Validation(message)) | Err(IdentityError::Conflict(message)) => {
            apply_view(
                &state,
                form,
                "",
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(IdentityError::Suspended(message)) => {
            apply_view(&state, form, "", message, StatusCode::FORBIDDEN).await
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

#[derive(Deserialize)]
struct PlainForm {
    _csrf: String,
}
csrf_forms!(PlainForm);

async fn withdraw(
    Page(state): Page,
    Path(id): Path<i64>,
    CsrfForm(_form): CsrfForm<PlainForm>,
) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Flinks%2Fapply");
    };
    match links::withdraw(
        &state.db,
        &state.global,
        session.member.actor(),
        id,
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect("/links/apply?done=withdrawn"),
        Err(error) => auth::identity_error(&state, error),
    }
}

// ---------------------------------------------------------------------------
// Admin: verified members

#[derive(Default, Deserialize)]
struct VerificationsQuery {
    #[serde(default)]
    filter: String,
    page: Option<usize>,
    uid: Option<i64>,
    #[serde(default)]
    done: String,
}

struct VerificationRow {
    uid: i64,
    username: String,
    label: &'static str,
    title: String,
    site_host: String,
    granted: String,
    reviewed: String,
    expires: String,
    state: &'static str,
}

#[derive(Template)]
#[template(path = "manage/verifications.html")]
struct VerificationsTemplate<'a> {
    p: PageMeta<'a>,
    filters: Vec<Tab>,
    rows: Vec<VerificationRow>,
    previous: String,
    next: String,
    notice: &'static str,
}

fn verifications_href(filter: Filter, page: usize) -> String {
    match (filter, page) {
        (Filter::All, 1) => "/manage/verifications".to_owned(),
        (filter, 1) => format!("/manage/verifications?filter={}", filter.slug()),
        (filter, page) => format!("/manage/verifications?filter={}&page={page}", filter.slug()),
    }
}

/// The expiry date is written in the form as "the last valid day"; what is stored is midnight of the next day (site
/// time zone).
fn expiry_text(expires_at: Option<i64>, clock: &crate::clock::TimeFormat) -> String {
    expires_at.map_or_else(
        || crate::i18n::current().site_manage_no_expiry().to_owned(),
        |at| crate::clock::date(at - 1, clock),
    )
}

async fn verifications(
    AdminPage(state): AdminPage,
    Query(query): Query<VerificationsQuery>,
) -> Response {
    if let Some(uid) = query.uid {
        if uid <= 0 {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().site_manage_error_no_member(),
            );
        }
        return auth::redirect(&format!("/manage/verifications/{uid}"));
    }
    let Some(filter) = Filter::parse(&query.filter) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().site_manage_error_filter(),
        );
    };
    let page = query.page.unwrap_or(1).max(1);
    let now = identity::now();
    let (list, more) = match directory::admin_list(&state.db, filter, page, now).await {
        Ok(value) => value,
        Err(error) => return auth::identity_error(&state, error),
    };
    let clock = crate::clock::current();
    let rows = list
        .into_iter()
        .map(|row| VerificationRow {
            state: row.state_label(now),
            uid: row.verification.uid,
            label: row.verification.category.label(),
            site_host: row
                .verification
                .site_url
                .as_deref()
                .map(directory::display_host)
                .unwrap_or_default(),
            granted: crate::clock::date(row.verification.granted_at, &clock),
            reviewed: crate::clock::date(row.verification.reviewed_at, &clock),
            expires: expiry_text(row.verification.expires_at, &clock),
            title: row.verification.title,
            username: row.username,
        })
        .collect();
    let filters = [Filter::All, Filter::Expiring, Filter::Expired]
        .into_iter()
        .chain(Category::ALL.into_iter().map(Filter::Category))
        .map(|item| Tab {
            label: match item {
                Filter::All => crate::i18n::current().site_manage_all(),
                Filter::Expiring => crate::i18n::current().site_manage_expiring(),
                Filter::Expired => crate::i18n::current().site_manage_expired(),
                Filter::Category(category) => category.label(),
            },
            href: verifications_href(item, 1),
            active: item == filter,
        })
        .collect();
    let notice = match query.done.as_str() {
        "revoked" => crate::i18n::current().site_manage_done_revoked(),
        _ => "",
    };
    render(&VerificationsTemplate {
        p: meta(
            &state,
            crate::i18n::current().site_manage_verified().into(),
            crate::i18n::current().site_manage_description().into(),
            "/manage/verifications",
            "",
            "",
        ),
        filters,
        rows,
        previous: if page > 1 {
            verifications_href(filter, page - 1)
        } else {
            String::new()
        },
        next: if more {
            verifications_href(filter, page + 1)
        } else {
            String::new()
        },
        notice,
    })
}

#[derive(Default, Deserialize)]
struct VerificationForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    site_url: String,
    /// `<input type="date">`: the last valid day; empty means no expiry.
    #[serde(default)]
    expires: String,
    /// `reviewed_at` of the verification the edit form was opened on; absent when there was none.
    #[serde(default)]
    reviewed_at: Option<i64>,
    #[serde(default)]
    reason: String,
}
csrf_forms!(VerificationForm);

struct CategoryChoice {
    slug: &'static str,
    label: &'static str,
    selected: bool,
}

#[derive(Template)]
#[template(path = "manage/verification.html")]
struct VerificationTemplate<'a> {
    p: PageMeta<'a>,
    uid: i64,
    username: String,
    member_state: &'static str,
    exists: bool,
    categories: Vec<CategoryChoice>,
    form: VerificationForm,
    granted: String,
    reviewed: String,
    shown: &'static str,
    notice: &'static str,
    message: String,
}

#[derive(Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
}

async fn verification_view(
    state: &AppState,
    uid: i64,
    form: Option<VerificationForm>,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let subject = match directory::subject(&state.db, uid).await {
        Ok(Some(subject)) => subject,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().site_manage_error_no_account(),
            );
        }
        Err(error) => return auth::identity_error(state, error),
    };
    let clock = crate::clock::current();
    let now = identity::now();
    let existing = subject.verification.as_ref();
    let form = form.unwrap_or_else(|| VerificationForm {
        category: existing
            .map(|v| v.category.slug().to_owned())
            .unwrap_or_default(),
        title: existing.map(|v| v.title.clone()).unwrap_or_default(),
        site_url: existing
            .and_then(|v| v.site_url.clone())
            .unwrap_or_default(),
        expires: existing
            .and_then(|v| v.expires_at)
            .map(|at| {
                crate::clock::input_value(at - 1, &clock)
                    .chars()
                    .take(10)
                    .collect()
            })
            .unwrap_or_default(),
        reviewed_at: existing.map(|v| v.reviewed_at),
        ..Default::default()
    });
    let shown = match existing {
        None => crate::i18n::current().site_manage_unverified(),
        Some(_) if subject.account_state != "active" => {
            crate::i18n::current().site_manage_hidden_deleting()
        }
        Some(_) if subject.freeze => crate::i18n::current().site_manage_hidden_frozen(),
        Some(v) if v.expires_at.is_some_and(|at| at <= now) => {
            crate::i18n::current().site_manage_hidden_expired()
        }
        Some(_) => crate::i18n::current().site_manage_shown(),
    };
    let mut response = render(&VerificationTemplate {
        p: meta(
            state,
            crate::i18n::current().site_manage_verified().into(),
            crate::i18n::current().site_manage_description().into(),
            &format!("/manage/verifications/{uid}"),
            "",
            "",
        ),
        uid,
        member_state: if subject.account_state != "active" {
            crate::i18n::current().site_manage_state_deleting()
        } else if subject.freeze {
            crate::i18n::current().site_manage_state_frozen()
        } else {
            crate::i18n::current().site_manage_state_normal()
        },
        username: subject.username,
        exists: existing.is_some(),
        categories: Category::ALL
            .into_iter()
            .map(|item| CategoryChoice {
                slug: item.slug(),
                label: item.label(),
                selected: item.slug() == form.category,
            })
            .collect(),
        granted: existing
            .map(|v| crate::clock::date(v.granted_at, &clock))
            .unwrap_or_default(),
        reviewed: existing
            .map(|v| crate::clock::date(v.reviewed_at, &clock))
            .unwrap_or_default(),
        form,
        shown,
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn verification(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    Query(query): Query<DoneQuery>,
) -> Response {
    let notice = match query.done.as_str() {
        "saved" => crate::i18n::current().site_manage_done_saved(),
        "unchanged" => crate::i18n::current().site_manage_done_unchanged(),
        _ => "",
    };
    verification_view(&state, uid, None, notice, String::new(), StatusCode::OK).await
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

async fn save_verification(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    CsrfForm(form): CsrfForm<VerificationForm>,
) -> Response {
    let clock = crate::clock::current();
    let expires_at = if form.expires.trim().is_empty() {
        None
    } else {
        match crate::clock::parse_input(&format!("{}T00:00", form.expires.trim()), &clock) {
            Some(start) => Some(start + 86_400),
            None => {
                return verification_view(
                    &state,
                    uid,
                    Some(form),
                    "",
                    crate::i18n::current().site_manage_error_date().into(),
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await;
            }
        }
    };
    let actor = state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .expect("AdminPage guarantees a signed-in member");
    let grant = directory::Grant {
        category: form.category.clone(),
        title: form.title.clone(),
        site_url: form.site_url.clone(),
        expires_at,
        reviewed_at: form.reviewed_at,
    };
    match directory::save(&state.db, actor, uid, grant, identity::now()).await {
        Ok(directory::Saved::Unchanged) => {
            auth::redirect(&format!("/manage/verifications/{uid}?done=unchanged"))
        }
        Ok(_) => auth::redirect(&format!("/manage/verifications/{uid}?done=saved")),
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                verification_view(&state, uid, Some(form), "", message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

async fn revoke_verification(
    AdminPage(state): AdminPage,
    Path(uid): Path<i64>,
    CsrfForm(form): CsrfForm<VerificationForm>,
) -> Response {
    let actor = state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .expect("AdminPage guarantees a signed-in member");
    match directory::revoke(&state.db, actor, uid, &form.reason, identity::now()).await {
        Ok(_) => auth::redirect("/manage/verifications?done=revoked"),
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                verification_view(&state, uid, None, "", message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

// ---------------------------------------------------------------------------
// Admin: friend links

#[derive(Default, Deserialize)]
struct ManageLinksQuery {
    #[serde(default)]
    status: String,
    page: Option<usize>,
    /// `saved` when returning to this page after the application rules block was saved.
    #[serde(default)]
    rules: String,
}

#[derive(Default, Deserialize)]
struct LinkForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    home: bool,
    #[serde(default)]
    displayorder: String,
    #[serde(default)]
    reason: String,
}
csrf_forms!(LinkForm);

impl LinkForm {
    fn input(&self) -> Result<links::LinkInput, &'static str> {
        let displayorder = if self.displayorder.trim().is_empty() {
            0
        } else {
            self.displayorder
                .trim()
                .parse()
                .map_err(|_| crate::i18n::current().site_manage_link_error_order())?
        };
        Ok(links::LinkInput {
            category: self.category.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            description: self.description.clone(),
            home: self.home,
            displayorder,
        })
    }

    fn from_link(link: &links::Link) -> Self {
        Self {
            category: link.category.slug().to_owned(),
            name: link.name.clone(),
            url: link.url.clone(),
            description: link.description.clone(),
            home: link.home,
            displayorder: link.displayorder.to_string(),
            ..Default::default()
        }
    }
}

struct LinkRow {
    link: links::Link,
    submitted: String,
}

#[derive(Template)]
#[template(path = "manage/links.html")]
struct ManageLinksTemplate<'a> {
    p: PageMeta<'a>,
    filters: Vec<Tab>,
    rows: Vec<LinkRow>,
    previous: String,
    next: String,
    form: LinkForm,
    categories: Vec<CategoryOption>,
    message: String,
    /// The "Application rules" block (`settings.links.*`, current values read from the database).
    rules: super::manage_settings::SectionView,
    rules_saved: bool,
}

fn manage_links_href(status: Option<Status>, page: usize) -> String {
    match (status, page) {
        (None, 1) => "/manage/links".to_owned(),
        (None, page) => format!("/manage/links?page={page}"),
        (Some(status), 1) => format!("/manage/links?status={}", status.slug()),
        (Some(status), page) => format!("/manage/links?status={}&page={page}", status.slug()),
    }
}

async fn manage_links_view(
    state: &AppState,
    query: ManageLinksQuery,
    form: LinkForm,
    message: String,
    status_code: StatusCode,
) -> Response {
    let status = if query.status.is_empty() {
        None
    } else {
        match Status::parse(&query.status) {
            Some(status) => Some(status),
            None => {
                return error_page(
                    state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().site_manage_links_error_status(),
                );
            }
        }
    };
    let page = query.page.unwrap_or(1).max(1);
    let rules = match crate::runtime::read(&state.db).await {
        Ok(settings) => super::manage_settings::link_rules_section(&settings),
        Err(error) => return auth::identity_error(state, error),
    };
    let (list, more) = match links::admin_list(&state.db, status, page).await {
        Ok(value) => value,
        Err(error) => return auth::identity_error(state, error),
    };
    let clock = crate::clock::current();
    let filters = std::iter::once(None)
        .chain(Status::ALL.into_iter().map(Some))
        .map(|item| Tab {
            label: item.map_or(
                crate::i18n::current().site_manage_links_all(),
                Status::label,
            ),
            href: manage_links_href(item, 1),
            active: item == status,
        })
        .collect();
    let mut response = render(&ManageLinksTemplate {
        p: meta(
            state,
            crate::i18n::current().manage_tab_links().into(),
            crate::i18n::current()
                .site_manage_links_description()
                .into(),
            "/manage/links",
            "",
            "",
        ),
        filters,
        rows: list
            .into_iter()
            .map(|link| LinkRow {
                submitted: crate::clock::date(link.dateline, &clock),
                link,
            })
            .collect(),
        previous: if page > 1 {
            manage_links_href(status, page - 1)
        } else {
            String::new()
        },
        next: if more {
            manage_links_href(status, page + 1)
        } else {
            String::new()
        },
        categories: link_categories(&form.category),
        form,
        message,
        rules,
        rules_saved: query.rules == "saved",
    });
    *response.status_mut() = status_code;
    response
}

async fn manage_links(
    AdminPage(state): AdminPage,
    Query(query): Query<ManageLinksQuery>,
) -> Response {
    manage_links_view(
        &state,
        query,
        LinkForm::default(),
        String::new(),
        StatusCode::OK,
    )
    .await
}

fn admin_actor(state: &AppState) -> identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .expect("AdminPage guarantees a signed-in member")
}

async fn create_link(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<LinkForm>) -> Response {
    let input = match form.input() {
        Ok(input) => input,
        Err(message) => {
            return manage_links_view(
                &state,
                ManageLinksQuery::default(),
                form,
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    match links::create(
        &state.db,
        &state.global,
        admin_actor(&state),
        input,
        identity::now(),
    )
    .await
    {
        Ok(id) => auth::redirect(&format!("/manage/links/{id}?done=saved")),
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                manage_links_view(&state, ManageLinksQuery::default(), form, message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

#[derive(Template)]
#[template(path = "manage/link.html")]
struct ManageLinkTemplate<'a> {
    p: PageMeta<'a>,
    link: links::Link,
    form: LinkForm,
    categories: Vec<CategoryOption>,
    submitted: String,
    handled: String,
    notice: &'static str,
    message: String,
}

async fn manage_link_view(
    state: &AppState,
    id: i64,
    form: Option<LinkForm>,
    notice: &'static str,
    message: String,
    status: StatusCode,
) -> Response {
    let link = match links::get(&state.db, id).await {
        Ok(Some(link)) => link,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().site_manage_link_not_found(),
            );
        }
        Err(error) => return auth::identity_error(state, error),
    };
    let clock = crate::clock::current();
    let form = form.unwrap_or_else(|| LinkForm::from_link(&link));
    let mut response = render(&ManageLinkTemplate {
        p: meta(
            state,
            crate::i18n::current().manage_tab_links().into(),
            crate::i18n::current()
                .site_manage_links_description()
                .into(),
            &format!("/manage/links/{id}"),
            "",
            "",
        ),
        submitted: crate::clock::datetime(link.dateline, &clock),
        handled: link
            .handled_at
            .map(|at| crate::clock::datetime(at, &clock))
            .unwrap_or_default(),
        categories: link_categories(&form.category),
        form,
        link,
        notice,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn manage_link(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    Query(query): Query<DoneQuery>,
) -> Response {
    let notice = match query.done.as_str() {
        "saved" => crate::i18n::current().site_manage_link_saved(),
        "unchanged" => crate::i18n::current().site_manage_link_unchanged(),
        _ => "",
    };
    manage_link_view(&state, id, None, notice, String::new(), StatusCode::OK).await
}

async fn decide_link(
    AdminPage(state): AdminPage,
    Path(id): Path<i64>,
    CsrfForm(form): CsrfForm<LinkForm>,
) -> Response {
    let decision = match form.action.as_str() {
        "approve" | "update" => match form.input() {
            Ok(input) if form.action == "approve" => links::Decision::Approve(input),
            Ok(input) => links::Decision::Update(input),
            Err(message) => {
                return manage_link_view(
                    &state,
                    id,
                    Some(form),
                    "",
                    message.into(),
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await;
            }
        },
        "reject" => links::Decision::Reject(form.reason.clone()),
        "remove" => links::Decision::Remove(form.reason.clone()),
        _ => {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().site_manage_link_error_action(),
            );
        }
    };
    match links::decide(
        &state.db,
        &state.global,
        admin_actor(&state),
        id,
        decision,
        identity::now(),
    )
    .await
    {
        Ok(links::Handled::Done) => auth::redirect(&format!("/manage/links/{id}?done=saved")),
        Ok(links::Handled::Unchanged) => {
            auth::redirect(&format!("/manage/links/{id}?done=unchanged"))
        }
        Err(error) => match form_message(error) {
            Ok((status, message)) => {
                manage_link_view(&state, id, Some(form), "", message, status).await
            }
            Err(other) => auth::identity_error(&state, other),
        },
    }
}
