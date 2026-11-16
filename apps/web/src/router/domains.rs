//! The admin area's "Multiple domains": the switch, the domain list, adding and editing
//! (including the second confirmation for path changes), and deleting.
//!
//! Administrators only, and only reachable on the primary domain (`/manage` on other domains is 301-redirected to
//! the primary domain by the entry layer). All writes are in [`crate::domains::write`]; this only handles forms and
//! pages.
use std::collections::HashMap;

use super::extract::{AdminPage, CsrfForm, Page, csrf_forms};
use super::*;
use crate::domains::{
    Canonical, Domain, DomainMap,
    write::{DomainError, DomainInput},
};
use axum::{extract::Path as AxumPath, routing::post};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/domains", get(list).post(save))
        .route("/manage/domains/switch", post(switch))
        .route("/manage/domains/new", get(new_form))
        .route("/manage/domains/{id}", get(edit_form))
        .route("/manage/domains/{id}/delete", post(delete))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024))
}

/// One row in the list.
struct RowView {
    id: i64,
    version: i64,
    url: String,
    name: String,
    main: bool,
    canonical: &'static str,
    enabled: bool,
    forums: String,
}

#[derive(Template)]
#[template(path = "manage/domains.html")]
struct ListTemplate<'a> {
    p: PageMeta<'a>,
    enabled: bool,
    forum_url: String,
    rows: Vec<RowView>,
    /// Top-level forums that no enabled domain shows.
    unshown: Vec<String>,
    message: String,
    notice: String,
}

/// The second confirmation for a path change (the same mechanism as for the forum URL's path prefix, see
/// `manage_settings::PrefixConfirm`).
struct ConfirmView {
    /// The URL and path prefix before the change, carried back unchanged: on save the path prefix is checked under
    /// the lock, and when confirm is mistyped this section is redrawn from the URL.
    from_url: String,
    from_prefix: String,
    from_label: String,
    to_label: String,
    rows: Vec<(&'static str, String, String)>,
    nginx: String,
    redirect: String,
}

#[derive(Template)]
#[template(path = "manage/domain.html")]
struct FormTemplate<'a> {
    p: PageMeta<'a>,
    domain_id: i64,
    version: i64,
    main: bool,
    site_url: String,
    site_name: String,
    canonical: String,
    logo_aid: i64,
    favicon_aid: i64,
    verification: String,
    /// Sign-in methods and participation conditions: whether customized (not customized means following the
    /// site-wide settings), and each item's value, name and whether it is checked.
    login_custom: bool,
    login_items: Vec<(&'static str, &'static str, bool)>,
    participation_custom: bool,
    participation_items: Vec<(&'static str, &'static str, bool)>,
    /// The interface language: the site language's name for "Same as the site", and each compiled-in language's tag,
    /// name and whether it is chosen.
    site_language: &'static str,
    languages: Vec<(&'static str, &'static str, bool)>,
    /// The row's own language tag; empty follows the site.
    row_language: &'static str,
    enabled: bool,
    /// Top-level forums: id, name, whether checked.
    forums: Vec<(i64, String, bool)>,
    /// Images the member uploaded that passed inspection: id and file name.
    images: Vec<(i64, String)>,
    message: String,
    confirm: Option<ConfirmView>,
    /// The domain's own skin (docs/skins.md, "Choosing a skin"): only for saved domains other than the main one.
    skin: Option<DomainSkin>,
}

/// The appearance section of a domain's page: the `id:palette` choices (the domain's own selected), the site's skin,
/// and the domain's colors (docs/styles.md, "Seed colors").
struct DomainSkin {
    choices: Vec<(String, String, bool)>,
    follows: bool,
    site_name: String,
    saved: bool,
    colors: super::colors::DomainColors,
}

/// What the appearance section of a domain's page reports: a saved skin or colors, or refused colors to show again.
#[derive(Default)]
struct Appearance {
    skin_saved: bool,
    colors_saved: bool,
    colors_entered: Option<(&'static str, String, String)>,
}

impl ListTemplate<'_> {
    /// The unshown forums as an inline list in this language.
    fn unshown_names(&self) -> String {
        self.unshown.join(self.p.t.common_list_separator())
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ListQuery {
    saved: String,
}

/// Read the domain table from the database (the admin edit page shows current values, bypassing the cache).
async fn current(state: &AppState) -> anyhow::Result<DomainMap> {
    DomainMap::load(&state.db).await
}

/// Top-level forums (the admin area sees all of them, regardless of visibility): id and name, in sort order.
async fn top_forums(state: &AppState) -> anyhow::Result<Vec<(i64, String)>> {
    let rows = crate::db::query(
        "SELECT fid,name FROM gonof_forum_forum WHERE parent_fid IS NULL ORDER BY displayorder,fid LIMIT 10000",
    )
    .fetch_all(state.db.executor())
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("fid")?, crate::db::text(row, "name")?)))
        .collect()
}

async fn list(AdminPage(state): AdminPage, Query(query): Query<ListQuery>) -> Response {
    let t = crate::i18n::current();
    let notice = match query.saved.as_str() {
        "1" => t.domain_saved(),
        "deleted" => t.domain_deleted(),
        "on" => t.domain_switched_on(),
        "off" => t.domain_switched_off(),
        _ => "",
    };
    list_page(&state, String::new(), notice.into(), StatusCode::OK).await
}

async fn list_page(
    state: &AppState,
    message: String,
    notice: String,
    status: StatusCode,
) -> Response {
    let settings = match crate::runtime::read(&state.db).await {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(state, error),
    };
    let (map, forums) = match tokio::try_join!(current(state), top_forums(state)) {
        Ok(found) => found,
        Err(error) => return database_error(state, error),
    };
    let forum_url = settings.site.url.clone();
    let name_of = |fid: &i64| {
        forums
            .iter()
            .find(|(id, _)| id == fid)
            .map_or_else(|| format!("#{fid}"), |(_, name)| name.clone())
    };
    let main = map.main(&forum_url).map(|main| main.domain_id);
    let mut rows: Vec<RowView> = map
        .rows()
        .iter()
        .map(|row| RowView {
            id: row.domain_id,
            version: row.version,
            url: row.site_url.clone(),
            name: row
                .site_name
                .clone()
                .unwrap_or_else(|| settings.site.name.clone()),
            main: Some(row.domain_id) == main,
            canonical: row.canonical.label(),
            enabled: row.enabled,
            forums: if row.forums.is_empty() {
                crate::i18n::current().domain_list_no_forums().into()
            } else {
                row.forums
                    .iter()
                    .map(name_of)
                    .collect::<Vec<_>>()
                    .join(crate::i18n::current().common_list_separator())
            },
        })
        .collect();
    rows.sort_by_key(|row| (!row.main, row.id));
    let unshown = forums
        .iter()
        .filter(|(fid, _)| !map.rows().iter().any(|row| row.enabled && row.shows(*fid)))
        .map(|(_, name)| name.clone())
        .collect();
    let mut response = render(&ListTemplate {
        p: meta(
            state,
            crate::i18n::current().domain_title().into(),
            String::new(),
            "/manage/domains",
            "",
            "",
        ),
        enabled: settings.domains.enabled,
        forum_url,
        rows,
        unshown,
        message,
        notice,
    });
    *response.status_mut() = status;
    response
}

#[derive(Deserialize)]
struct SwitchForm {
    _csrf: String,
    enabled: String,
}
csrf_forms!(SwitchForm);

/// The switch: when turning on, the forum URL must be filled in, and the primary domain row is created if the
/// table lacks it (`domains::write::follow_settings`).
async fn switch(Page(state): Page, CsrfForm(form): CsrfForm<SwitchForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/domains");
    };
    let on = form.enabled == "on";
    match crate::runtime::update(
        &state.db,
        &state.global,
        session.member.actor(),
        "site.domains.switch",
        |settings| settings.domains.enabled = on,
    )
    .await
    {
        Ok(_) => auth::redirect(if on {
            "/manage/domains?saved=on"
        } else {
            "/manage/domains?saved=off"
        }),
        Err(crate::identity::IdentityError::Validation(message)) => {
            list_page(
                &state,
                message.into(),
                String::new(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

async fn new_form(AdminPage(state): AdminPage) -> Response {
    let blank = Domain {
        domain_id: 0,
        site_url: String::new(),
        site_name: None,
        canonical: Canonical::Own,
        logo_aid: None,
        favicon_aid: None,
        verification: None,
        login_methods: None,
        participation: None,
        language: None,
        enabled: true,
        version: 0,
        dateline: 0,
        forums: Default::default(),
    };
    form_page(
        &state,
        &blank,
        false,
        String::new(),
        None,
        StatusCode::OK,
        Appearance::default(),
    )
    .await
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct EditQuery {
    skin: String,
    colors: String,
}

async fn edit_form(
    AdminPage(state): AdminPage,
    AxumPath(id): AxumPath<i64>,
    Query(query): Query<EditQuery>,
) -> Response {
    let (map, settings) = match tokio::try_join!(current(&state), async {
        crate::runtime::read(&state.db)
            .await
            .map_err(anyhow::Error::new)
    }) {
        Ok(found) => found,
        Err(error) => return database_error(&state, error),
    };
    let Some(row) = map.get(id).cloned() else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().domain_error_not_found(),
        );
    };
    let main = row.site_url == settings.site.url;
    form_page(
        &state,
        &row,
        main,
        String::new(),
        None,
        StatusCode::OK,
        Appearance {
            skin_saved: query.skin == "saved",
            colors_saved: query.colors == "saved",
            colors_entered: None,
        },
    )
    .await
}

/// A domain's colors form that was refused (a field that is not `#rrggbb`): the page again with what was entered,
/// 422.
pub(super) async fn colors_refused(
    state: &AppState,
    id: i64,
    entered: (&'static str, String, String),
) -> Response {
    let (map, settings) = match tokio::try_join!(current(state), async {
        crate::runtime::read(&state.db)
            .await
            .map_err(anyhow::Error::new)
    }) {
        Ok(found) => found,
        Err(error) => return database_error(state, error),
    };
    let Some(row) = map.get(id).cloned() else {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().domain_error_not_found(),
        );
    };
    let main = row.site_url == settings.site.url;
    form_page(
        state,
        &row,
        main,
        String::new(),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
        Appearance {
            colors_entered: Some(entered),
            ..Appearance::default()
        },
    )
    .await
}

/// The appearance section of a saved domain other than the main one.
async fn domain_skin(
    state: &AppState,
    domain_id: i64,
    appearance: Appearance,
) -> anyhow::Result<DomainSkin> {
    let t = crate::i18n::current();
    let block = state.global.settings(&state.db).await?;
    let catalog = state
        .global
        .skins(&state.db, &state.skins_dir, state.cache_ttl())
        .await?;
    let own = block
        .domain_layouts
        .iter()
        .find(|(id, _)| *id == domain_id)
        .map(|(_, layout)| layout)
        .filter(|layout| layout.skin.is_some());
    let (site_skin, _) = crate::skins::selection::choose(block.layout.as_ref(), None);
    let site_name = catalog
        .get(site_skin)
        .map_or_else(|| site_skin.to_owned(), |skin| skin.name(t));
    let current = own.and_then(|layout| {
        let skin = catalog.get(layout.skin.as_deref()?)?;
        Some((skin.id(), skin.palette(layout.palette.as_deref())))
    });
    Ok(DomainSkin {
        choices: super::skins::choices(&catalog, current, t),
        follows: own.is_none(),
        site_name,
        saved: appearance.skin_saved,
        colors: super::colors::domain_colors(
            state,
            domain_id,
            appearance.colors_saved,
            appearance.colors_entered,
        )
        .await?,
    })
}

/// The form page: `row` holds the values to show (the current values, or just-submitted values that were not
/// saved).
async fn form_page(
    state: &AppState,
    row: &Domain,
    main: bool,
    message: String,
    confirm: Option<ConfirmView>,
    status: StatusCode,
    appearance: Appearance,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/domains");
    };
    let forums = match top_forums(state).await {
        Ok(forums) => forums,
        Err(error) => return database_error(state, error),
    };
    let images = match state.media.uploads(session.member.uid).await {
        Ok(uploads) => uploads
            .into_iter()
            .filter(|upload| upload.is_ready() && upload.mime_type.starts_with("image/"))
            .map(|upload| (upload.aid, upload.filename))
            .collect(),
        Err(error) => return super::media_failure(state, error),
    };
    let skin = if row.domain_id > 0 && !main {
        match domain_skin(state, row.domain_id, appearance).await {
            Ok(skin) => Some(skin),
            Err(error) => return database_error(state, error),
        }
    } else {
        None
    };
    let t = crate::i18n::current();
    let title = if row.domain_id == 0 {
        t.domain_add().to_owned()
    } else if main {
        t.domain_main().to_owned()
    } else {
        t.domain_edit_title(&row.site_url)
    };
    let mut response = render(&FormTemplate {
        p: meta(state, title, String::new(), "/manage/domains", "", ""),
        domain_id: row.domain_id,
        version: row.version,
        main,
        site_url: row.site_url.clone(),
        site_name: row.site_name.clone().unwrap_or_default(),
        canonical: row.canonical.as_str().into(),
        logo_aid: row.logo_aid.unwrap_or(0),
        favicon_aid: row.favicon_aid.unwrap_or(0),
        verification: row.verification.clone().unwrap_or_default(),
        login_custom: row.login_methods.is_some(),
        login_items: crate::login::method_labels()
            .iter()
            .map(|(value, label)| {
                let checked = row
                    .login_methods
                    .as_deref()
                    .is_some_and(|set| crate::runtime::settings::set_contains(set, value));
                (*value, *label, checked)
            })
            .collect(),
        participation_custom: row.participation.is_some(),
        participation_items: crate::login::participation_labels()
            .iter()
            .map(|(value, label)| {
                let checked = row
                    .participation
                    .as_deref()
                    .is_some_and(|set| crate::runtime::settings::set_contains(set, value));
                (*value, *label, checked)
            })
            .collect(),
        site_language: state.frame.settings.site.lang().name(),
        row_language: row.language.map_or("", crate::i18n::Lang::tag),
        languages: crate::i18n::Lang::all()
            .map(|lang| (lang.tag(), lang.name(), row.language == Some(lang)))
            .collect(),
        enabled: row.enabled,
        forums: forums
            .into_iter()
            .map(|(fid, name)| (fid, name, row.forums.contains(&fid)))
            .collect(),
        images,
        message,
        confirm,
        skin,
    });
    *response.status_mut() = status;
    response
}

/// One row from the form: field names are fixed, and forum checkboxes are `forum_{fid}=1` (repeated names with
/// multiple values would be lost in a `HashMap`).
fn input_of(form: &HashMap<String, String>) -> Result<DomainInput, &'static str> {
    let number = |key: &str| -> Result<i64, &'static str> {
        match form.get(key).map(|value| value.trim()) {
            None | Some("") => Ok(0),
            Some(value) => value
                .parse()
                .map_err(|_| crate::i18n::current().domain_error_form_number()),
        }
    };
    let text = |key: &str| form.get(key).cloned().unwrap_or_default();
    let mut forums: Vec<i64> = form
        .iter()
        .filter(|(_, value)| value.as_str() == "1")
        .filter_map(|(key, _)| key.strip_prefix("forum_")?.parse().ok())
        .collect();
    forums.sort_unstable();
    // Sign-in methods and participation conditions: without "Customize" checked, follow the site-wide settings (store
    // NULL); checked items are `login_{value}=1` and `participation_{value}=1`.
    let picked = |custom: &str, prefix: &str| -> Option<Vec<String>> {
        form.get(custom).is_some_and(|value| value == "1").then(|| {
            form.iter()
                .filter(|(_, value)| value.as_str() == "1")
                .filter_map(|(key, _)| key.strip_prefix(prefix).map(str::to_owned))
                .filter(|item| item != "custom")
                .collect()
        })
    };
    let login_methods = picked("login_custom", "login_");
    let participation = picked("participation_custom", "participation_");
    let confirmed_from = form
        .get("confirmed_from")
        .filter(|_| {
            form.get("confirm")
                .is_some_and(|word| word.trim() == "confirm")
        })
        .cloned();
    Ok(DomainInput {
        domain_id: number("domain_id")?,
        version: number("version")?,
        site_url: text("site_url"),
        site_name: text("site_name"),
        canonical: text("canonical"),
        logo_aid: number("logo_aid")?,
        favicon_aid: number("favicon_aid")?,
        verification: text("verification"),
        login_methods,
        participation,
        language: form.get("language").cloned(),
        enabled: form.get("enabled").is_some_and(|value| value == "1"),
        forums,
        confirmed_from,
    })
}

/// Refill the form with the submitted values (used when the save did not go through).
fn echo(input: &DomainInput) -> Domain {
    Domain {
        domain_id: input.domain_id,
        site_url: input.site_url.clone(),
        site_name: Some(input.site_name.clone()),
        canonical: Canonical::parse(&input.canonical).unwrap_or(Canonical::Own),
        logo_aid: (input.logo_aid > 0).then_some(input.logo_aid),
        favicon_aid: (input.favicon_aid > 0).then_some(input.favicon_aid),
        verification: Some(input.verification.clone()),
        login_methods: input.login_methods.as_ref().map(|items| items.join(",")),
        participation: input.participation.as_ref().map(|items| items.join(",")),
        language: input.language.as_deref().and_then(crate::i18n::Lang::parse),
        enabled: input.enabled,
        version: input.version,
        dateline: 0,
        forums: input.forums.iter().copied().collect(),
    }
}

async fn save(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/domains");
    };
    let input = match input_of(&form) {
        Ok(input) => input,
        Err(message) => return error_page(&state, StatusCode::BAD_REQUEST, message),
    };
    let main = form.get("main").is_some_and(|value| value == "1");
    // The confirmation box was submitted but not typed correctly: do not save, go back to the confirmation section.
    let typed_wrong = form.contains_key("confirmed_from") && input.confirmed_from.is_none();
    let result = if typed_wrong {
        Err(DomainError::NeedsConfirm {
            from: form.get("confirmed_url").cloned().unwrap_or_default(),
            to: input.site_url.clone(),
        })
    } else {
        crate::domains::write::save(
            &state.db,
            &state.global,
            session.member.actor(),
            &input,
            state.config.media.orphan_seconds,
        )
        .await
    };
    match result {
        Ok(saved) => {
            if saved.unshown.is_empty() {
                auth::redirect("/manage/domains?saved=1")
            } else {
                list_page(
                    &state,
                    String::new(),
                    crate::i18n::current().domain_saved_unshown(
                        saved
                            .unshown
                            .join(crate::i18n::current().common_list_separator()),
                    ),
                    StatusCode::OK,
                )
                .await
            }
        }
        Err(DomainError::NeedsConfirm { from, to }) => {
            let message = if typed_wrong {
                crate::i18n::current().domain_confirm_mistyped().to_owned()
            } else {
                crate::i18n::current().domain_confirm_message(
                    path_label(crate::site::prefix_of(&from)),
                    path_label(crate::site::prefix_of(&to)),
                )
            };
            let confirm = confirm_view(&state, &from, &to);
            form_page(
                &state,
                &echo(&input),
                main,
                message,
                Some(confirm),
                StatusCode::OK,
                Appearance::default(),
            )
            .await
        }
        Err(DomainError::Moved) => {
            form_page(
                &state,
                &echo(&input),
                main,
                DomainError::Moved.to_string(),
                None,
                StatusCode::CONFLICT,
                Appearance::default(),
            )
            .await
        }
        Err(DomainError::Invalid(message)) => {
            form_page(
                &state,
                &echo(&input),
                main,
                message,
                None,
                StatusCode::UNPROCESSABLE_ENTITY,
                Appearance::default(),
            )
            .await
        }
        Err(DomainError::Conflict(message)) => {
            form_page(
                &state,
                &echo(&input),
                main,
                message.into(),
                None,
                StatusCode::CONFLICT,
                Appearance::default(),
            )
            .await
        }
        Err(error) => failure(&state, error),
    }
}

fn path_label(prefix: &str) -> String {
    if prefix.is_empty() {
        crate::i18n::current().domain_root_path().into()
    } else {
        prefix.into()
    }
}

/// The confirmation section: old and new common URLs side by side, the nginx snippet for the new path, and the
/// snippet for the old path's 301.
fn confirm_view(state: &AppState, from_url: &str, to_url: &str) -> ConfirmView {
    let from = crate::site::prefix_of(from_url).to_owned();
    let to = crate::site::prefix_of(to_url).to_owned();
    let t = crate::i18n::current();
    let rows = [
        (t.domain_page_home(), "/"),
        (t.domain_page_login(), "/login"),
        (t.domain_page_feed(), "/feed.xml"),
        (t.domain_page_sitemap(), "/sitemap.xml"),
        (t.domain_page_health(), "/healthz"),
    ]
    .into_iter()
    .map(|(label, path)| {
        (
            label,
            format!("{from_url}{path}"),
            format!("{to_url}{path}"),
        )
    })
    .collect();
    let listen = state.config.listen;
    let location = if to.is_empty() {
        "/".to_owned()
    } else {
        to.clone()
    };
    let nginx = format!(
        "location {location} {{\n    proxy_pass http://{listen};\n    proxy_set_header Host $host;\n    proxy_set_header X-Forwarded-Proto $scheme;\n    proxy_set_header X-Real-IP $remote_addr;\n}}"
    );
    let redirect = if from.is_empty() {
        String::new()
    } else {
        format!("location ^~ {from} {{\n    rewrite ^{from}(?:/(.*))?$ {to}/$1 permanent;\n}}")
    };
    ConfirmView {
        from_url: from_url.to_owned(),
        from_label: path_label(&from),
        to_label: path_label(&to),
        from_prefix: from,
        rows,
        nginx,
        redirect,
    }
}

#[derive(Deserialize)]
struct DeleteForm {
    _csrf: String,
    version: i64,
}
csrf_forms!(DeleteForm);

async fn delete(
    Page(state): Page,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<DeleteForm>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/domains");
    };
    match crate::domains::write::delete(
        &state.db,
        &state.global,
        session.member.actor(),
        id,
        form.version,
        state.config.media.orphan_seconds,
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/domains?saved=deleted"),
        Err(DomainError::Invalid(message)) => {
            list_page(
                &state,
                message,
                String::new(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(DomainError::Conflict(message)) => {
            list_page(&state, message.into(), String::new(), StatusCode::CONFLICT).await
        }
        Err(error) => failure(&state, error),
    }
}

fn failure(state: &AppState, error: DomainError) -> Response {
    match error {
        DomainError::Forbidden => {
            auth::identity_error(state, crate::identity::IdentityError::Forbidden)
        }
        DomainError::NotFound => error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().domain_error_not_found(),
        ),
        DomainError::Internal(error) => database_error(state, error),
        other => error_page(state, StatusCode::UNPROCESSABLE_ENTITY, &other.to_string()),
    }
}
