//! Skins (docs/skins.md): the stylesheet of each usable skin at `/skin/<id>.css`, the admin page Appearance › Skins
//! (`/manage/skins`: list, choose, preview, import, delete, rescan) and a domain's own choice on its page. Every
//! action is a plain form, so the page works without JavaScript. Administrators only (docs/roles.md, "Staff roles").
use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::*;
use crate::css_check::Finding;
use crate::skins::{self, Listed, Source, Status};
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath};
use axum::routing::post;
use sha2::{Digest, Sha256};

/// The cookie holding an administrator's preview (sealed, see `identity::skin_preview`).
pub(super) const PREVIEW_COOKIE: &str = "gonof_skin_preview";
const PATH: &str = "/manage/skins";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/skin/{file}", get(stylesheet))
        .route(PATH, get(page))
        .route("/manage/skins/select", post(select))
        .route("/manage/skins/rescan", post(rescan))
        .route("/manage/skins/preview", post(preview))
        .route("/manage/skins/preview/exit", post(exit_preview))
        .route("/manage/skins/preview/publish", post(publish))
        .route("/manage/skins/{id}/delete", post(delete))
        .route("/manage/domains/{id}/skin", post(domain_select))
        // A pack is at most 1 MiB; form-encoded for the confirmation step it can grow about threefold.
        .route(
            "/manage/skins/import",
            post(import).layer(DefaultBodyLimit::max(skins::MAX_PACK_BYTES + 64 * 1024)),
        )
        .route(
            "/manage/skins/import/confirm",
            post(confirm_import).layer(DefaultBodyLimit::max(4 * skins::MAX_PACK_BYTES)),
        )
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct StylesheetQuery {
    v: String,
    /// The seed colors of a colored variant (docs/styles.md, "Seed colors"), as twelve hex digits.
    c: String,
}

/// A skin's stylesheet, or with `c=` its variant with those seed colors. The address carries the stylesheet's hash,
/// so the matching version is cached for a year; `public` only for skins and colors a page of the site uses, so a
/// skin or colors being previewed stay out of shared caches. An old hash gets the current stylesheet uncached. Served
/// without a session: it is the same for everyone.
///
/// A colored variant is only compiled for seed colors that the site or a domain uses (and kept for the next
/// request), or for the preview of the administrator asking, recognised by their sealed preview cookie and their
/// session (compiled off the async workers and not kept). Any other `c=` gets the skin's own stylesheet with
/// `no-cache` and compiles nothing, so made-up addresses cost no work; a page cached from before a color change still
/// gets a stylesheet.
async fn stylesheet(
    State(state): State<Arc<AppState>>,
    AxumPath(file): AxumPath<String>,
    Query(query): Query<StylesheetQuery>,
    request: axum::extract::Request,
) -> Response {
    let Some(id) = file.strip_suffix(".css") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let seeds = match query.c.as_str() {
        "" => None,
        code => match skins::seed::Seeds::from_code(code) {
            Some(seeds) => Some(seeds),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let block = state.global.settings(&state.db).await.ok();
    let ttl = block.as_ref().map_or(0, |block| block.settings.cache_ttl());
    let catalog = match state.global.skins(&state.db, &state.skins_dir, ttl).await {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            tracing::error!(%error, "the skins could not be loaded; serving the built-in default only");
            None
        }
    };
    let in_use = block.as_ref().map_or_else(Vec::new, |block| {
        skins::selection::in_use(block.layout.as_ref(), &block.domain_layouts)
    });
    let active = match seeds {
        None => {
            id == skins::DEFAULT_ID
                || block.as_ref().is_some_and(|block| {
                    block
                        .layout
                        .iter()
                        .chain(block.domain_layouts.iter().map(|(_, layout)| layout))
                        .any(|layout| layout.skin.as_deref() == Some(id))
                })
        }
        Some(seeds) => in_use
            .iter()
            .any(|(skin, colors)| skin == id && *colors == Some(seeds)),
    };
    let base = match catalog.as_ref().and_then(|catalog| catalog.get(id)) {
        Some(skin) => skin.clone(),
        None if id == skins::DEFAULT_ID => skins::DEFAULT_SKIN.clone(),
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let (skin, uncolored) = match (seeds, &catalog) {
        (None, _) => (base, false),
        (Some(seeds), Some(catalog)) if active => {
            (catalog.colored(&base, Some(seeds), true), false)
        }
        (Some(seeds), Some(catalog)) => {
            // The preview's own seeds, or for a preview of a skin the seeds a page has anyway over that skin.
            let previewed = super::auth::own_skin_preview(
                &state,
                request.headers(),
                request.extensions(),
                |preview| {
                    preview.skin == id
                        && match preview.colors {
                            skins::selection::Colors::Seeds(own) => own == seeds,
                            skins::selection::Colors::Inherit => {
                                in_use.iter().any(|(_, colors)| *colors == Some(seeds))
                            }
                            skins::selection::Colors::Skin => false,
                        }
                },
            )
            .await;
            match previewed {
                Some(_) => {
                    let (catalog, skin) = (catalog.clone(), base.clone());
                    match tokio::task::spawn_blocking(move || {
                        catalog.colored(&skin, Some(seeds), false)
                    })
                    .await
                    {
                        Ok(variant) => (variant, false),
                        Err(error) => {
                            tracing::error!(%error, "compiling a previewed colored skin failed");
                            (base, true)
                        }
                    }
                }
                None => (base, true),
            }
        }
        (Some(_), None) => (base, true),
    };
    let cache = match (uncolored, query.v == skin.version_hash, active) {
        (true, _, _) | (false, false, _) => "no-cache",
        (false, true, true) => "public, max-age=31536000, immutable",
        (false, true, false) => "private, max-age=31536000, immutable",
    };
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        skin.stylesheet.to_string(),
    )
        .into_response()
}

pub(super) fn actor(state: &AppState) -> crate::identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .expect("AdminPage requires a session")
        .member
        .actor()
}

/// One row of the list.
struct Row {
    id: String,
    name: String,
    version: String,
    source: &'static str,
    state: String,
    usable: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
    imported: String,
    /// An imported row: its version, for the delete form.
    delete: Option<i64>,
}

/// One choice of the skin selects: a usable skin with one of its palettes, as `id:palette`.
struct Choice {
    value: String,
    label: String,
    selected: bool,
}

/// The confirmation step of an import.
struct Confirm {
    name: String,
    version: String,
    warnings: Vec<String>,
    replaces: i64,
    replaces_version: String,
    pack: String,
    digest: String,
}

#[derive(Template)]
#[template(path = "manage/skins.html")]
struct SkinsPage<'a> {
    p: PageMeta<'a>,
    rows: Vec<Row>,
    choices: Vec<Choice>,
    message: String,
    success: bool,
    refused: Vec<String>,
    confirm: Option<Confirm>,
    dir: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SavedQuery {
    saved: String,
}

/// The `id:palette` choices of every usable skin, the current one selected.
pub(super) fn choices(
    catalog: &skins::Catalog,
    current: Option<(&str, &str)>,
    t: crate::i18n::Lang,
) -> Vec<(String, String, bool)> {
    let mut choices = Vec::new();
    for skin in catalog.usable() {
        let name = skin.name(t);
        for palette in &skin.palettes {
            let label = if skin.palettes.len() > 1 {
                format!("{name} · {palette}")
            } else {
                name.clone()
            };
            let selected = current == Some((skin.id(), palette.as_str()));
            choices.push((format!("{}:{palette}", skin.id()), label, selected));
        }
    }
    choices
}

pub(super) async fn catalog(state: &AppState) -> anyhow::Result<Arc<skins::Catalog>> {
    state
        .global
        .skins(&state.db, &state.skins_dir, state.cache_ttl())
        .await
}

async fn view(
    state: &AppState,
    message: String,
    success: bool,
    refused: Vec<String>,
    confirm: Option<Confirm>,
    status: StatusCode,
) -> Response {
    let t = crate::i18n::current();
    let (catalog, block, domains) = match tokio::try_join!(
        catalog(state),
        async {
            state
                .global
                .settings(&state.db)
                .await
                .map_err(anyhow::Error::new)
        },
        crate::domains::DomainMap::load(&state.db),
    ) {
        Ok(found) => found,
        Err(error) => return database_error(state, error),
    };
    let (site_skin, site_palette) = skins::selection::choose(block.layout.as_ref(), None);
    let site_palette = catalog
        .get(site_skin)
        .map_or(String::new(), |skin| skin.palette(site_palette).to_owned());
    let rows = catalog
        .listed
        .iter()
        .map(|entry| row(entry, &block, &domains, site_skin, t))
        .collect();
    let choices = choices(&catalog, Some((site_skin, site_palette.as_str())), t)
        .into_iter()
        .map(|(value, label, selected)| Choice {
            value,
            label,
            selected,
        })
        .collect();
    let mut response = render(&SkinsPage {
        p: meta(
            state,
            t.skin_page_title().into(),
            t.skin_page_description().into(),
            PATH,
            "",
            "",
        ),
        rows,
        choices,
        message,
        success,
        refused,
        confirm,
        // Shown resolved: in a source build the instance root is written relative to the crate.
        dir: std::fs::canonicalize(&state.skins_dir)
            .or_else(|_| {
                state
                    .skins_dir
                    .parent()
                    .map(std::fs::canonicalize)
                    .unwrap_or_else(|| Ok(state.skins_dir.to_path_buf()))
                    .map(|parent| parent.join("skins"))
            })
            .unwrap_or_else(|_| state.skins_dir.to_path_buf())
            .display()
            .to_string(),
    });
    *response.status_mut() = status;
    response
}

/// The texts in order, each once (a token overridden in light and dark mode reads the same twice).
fn distinct(texts: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for text in texts {
        if !seen.contains(&text) {
            seen.push(text);
        }
    }
    seen
}

fn row(
    entry: &Listed,
    block: &crate::cache::SettingsBlock,
    domains: &crate::domains::DomainMap,
    site_skin: &str,
    t: crate::i18n::Lang,
) -> Row {
    let state = match entry.status {
        Status::Available => {
            let used_by: Vec<String> = block
                .domain_layouts
                .iter()
                .filter(|(_, layout)| layout.skin.as_deref() == Some(entry.id.as_str()))
                .filter_map(|(id, _)| domains.get(*id).map(|domain| domain.site_url.clone()))
                .collect();
            if entry.id == site_skin {
                t.skin_state_current().to_owned()
            } else if !used_by.is_empty() {
                t.skin_state_domains(used_by.join(t.common_list_separator()))
            } else {
                t.skin_state_usable().to_owned()
            }
        }
        Status::Shadowed(by) => t.skin_state_shadowed(by.label(t)),
        Status::Invalid => t.skin_state_invalid().to_owned(),
    };
    let format = crate::clock::current();
    Row {
        id: entry.id.clone(),
        name: entry.name(t),
        version: entry.version().to_owned(),
        source: entry.source.label(t),
        state,
        usable: entry.status == Status::Available,
        errors: distinct(entry.errors().map(|finding| finding.text(t))),
        warnings: distinct(entry.warnings().map(|finding| finding.text(t))),
        imported: entry.imported.as_ref().map_or(String::new(), |imported| {
            t.skin_imported_at(crate::clock::datetime(imported.at, &format))
        }),
        delete: entry.imported.as_ref().map(|imported| imported.row_version),
    }
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    let t = crate::i18n::current();
    let message = match query.saved.as_str() {
        "imported" => t.skin_saved_imported(),
        "deleted" => t.skin_saved_deleted(),
        "selected" => t.skin_saved_selected(),
        "rescanned" => t.skin_saved_rescanned(),
        _ => "",
    };
    view(
        &state,
        message.into(),
        !message.is_empty(),
        Vec::new(),
        None,
        StatusCode::OK,
    )
    .await
}

async fn failed(state: &AppState, error: crate::identity::IdentityError) -> Response {
    use crate::identity::IdentityError;
    match error {
        IdentityError::Validation(message) => {
            view(
                state,
                message.into(),
                false,
                Vec::new(),
                None,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        IdentityError::Conflict(message) => {
            view(
                state,
                message.into(),
                false,
                Vec::new(),
                None,
                StatusCode::CONFLICT,
            )
            .await
        }
        IdentityError::NotFound => {
            view(
                state,
                crate::i18n::current().skin_error_unknown().into(),
                false,
                Vec::new(),
                None,
                StatusCode::NOT_FOUND,
            )
            .await
        }
        error => auth::identity_error(state, error),
    }
}

#[derive(Deserialize)]
struct ChoiceForm {
    _csrf: String,
    #[serde(default)]
    choice: String,
}
csrf_forms!(ChoiceForm);

/// The usable skin and palette of an `id:palette` choice.
async fn chosen(
    state: &AppState,
    choice: &str,
) -> Result<(Arc<skins::Skin>, String), crate::identity::IdentityError> {
    let (id, palette) = choice.split_once(':').unwrap_or((choice, ""));
    let catalog = catalog(state).await?;
    let skin = catalog
        .get(id)
        .cloned()
        .ok_or(crate::identity::IdentityError::Validation(
            crate::i18n::current().skin_error_unknown(),
        ))?;
    let palette = skin.palette(Some(palette)).to_owned();
    Ok((skin, palette))
}

async fn select(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<ChoiceForm>) -> Response {
    let saved = async {
        let (skin, palette) = chosen(&state, &form.choice).await?;
        skins::selection::save(
            &state.db,
            &state.global,
            actor(&state),
            skins::selection::Selection {
                domain_id: None,
                skin: Some(&skin),
                palette: Some(&palette),
            },
        )
        .await
    }
    .await;
    match saved {
        Ok(()) => auth::redirect("/manage/skins?saved=selected"),
        Err(error) => failed(&state, error).await,
    }
}

#[derive(Deserialize)]
struct CsrfOnly {
    _csrf: String,
}
csrf_forms!(CsrfOnly);

/// Reads the instance directory again, here and, through the broadcast, in every other process.
async fn rescan(AdminPage(state): AdminPage, CsrfForm(_): CsrfForm<CsrfOnly>) -> Response {
    match state.global.announce(&[crate::cache::Block::Skins]).await {
        Ok(()) => auth::redirect("/manage/skins?saved=rescanned"),
        Err(error) => auth::identity_error(
            &state,
            crate::identity::IdentityError::Internal(anyhow::anyhow!("{error}")),
        ),
    }
}

pub(super) fn preview_cookie(response: &mut Response, value: &str, seconds: i64) {
    auth::auxiliary_set_cookie(response, PREVIEW_COOKIE, value, seconds);
}

/// Starts a preview: the skin is shown on every page to this administrator only, until they exit or publish it.
async fn preview(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<ChoiceForm>) -> Response {
    let (skin, palette) = match chosen(&state, &form.choice).await {
        Ok(found) => found,
        Err(error) => return failed(&state, error).await,
    };
    let actor = actor(&state);
    match state.identity.seal_skin_preview(
        actor.uid,
        skin.id(),
        &palette,
        skins::selection::Colors::Inherit,
    ) {
        Ok(sealed) => {
            let mut response = auth::redirect("/");
            preview_cookie(&mut response, &sealed, crate::identity::SKIN_PREVIEW_TTL);
            response
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

async fn exit_preview(AdminPage(_): AdminPage, CsrfForm(_): CsrfForm<CsrfOnly>) -> Response {
    let mut response = auth::redirect(PATH);
    preview_cookie(&mut response, "", 0);
    response
}

/// Makes the previewed skin the site's, or saves the previewed colors as the site's, and ends the preview.
async fn publish(AdminPage(state): AdminPage, CsrfForm(_): CsrfForm<CsrfOnly>) -> Response {
    let actor = actor(&state);
    let Some(preview) = state
        .identity
        .open_skin_preview(&state.viewer.skin_preview, actor.uid)
    else {
        let mut response = auth::redirect(PATH);
        preview_cookie(&mut response, "", 0);
        return response;
    };
    if !preview.colors.inherits() {
        return match skins::selection::save_colors(
            &state.db,
            &state.global,
            actor,
            None,
            preview.colors,
        )
        .await
        {
            Ok(()) => {
                let mut response = auth::redirect(&format!("{}?saved=saved", super::colors::PATH));
                preview_cookie(&mut response, "", 0);
                response
            }
            Err(error) => failed(&state, error).await,
        };
    }
    let saved = async {
        let (skin, palette) =
            chosen(&state, &format!("{}:{}", preview.skin, preview.palette)).await?;
        skins::selection::save(
            &state.db,
            &state.global,
            actor,
            skins::selection::Selection {
                domain_id: None,
                skin: Some(&skin),
                palette: Some(&palette),
            },
        )
        .await
    }
    .await;
    match saved {
        Ok(()) => {
            let mut response = auth::redirect("/manage/skins?saved=selected");
            preview_cookie(&mut response, "", 0);
            response
        }
        Err(error) => failed(&state, error).await,
    }
}

#[derive(Deserialize)]
struct DeleteForm {
    _csrf: String,
    #[serde(default)]
    version: i64,
}
csrf_forms!(DeleteForm);

async fn delete(
    AdminPage(state): AdminPage,
    AxumPath(id): AxumPath<String>,
    CsrfForm(form): CsrfForm<DeleteForm>,
) -> Response {
    match skins::store::delete(&state.db, &state.global, actor(&state), &id, form.version).await {
        Ok(()) => auth::redirect("/manage/skins?saved=deleted"),
        Err(error) => failed(&state, error).await,
    }
}

/// The warnings of a check, sorted, as stored and as a digest the confirmation form carries.
fn digest(warnings: &[Finding]) -> String {
    let text = serde_json::to_string(warnings).unwrap_or_default();
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Checks a pack and either imports it at once (no warnings, a new id) or asks for confirmation.
async fn check_pack(state: &AppState, text: String, confirmed: Option<(i64, String)>) -> Response {
    let t = crate::i18n::current();
    let refuse = |findings: Vec<Finding>| {
        findings
            .iter()
            .filter(|finding| finding.severity == crate::css_check::Severity::Error)
            .map(|finding| finding.text(t))
            .collect::<Vec<_>>()
    };
    let draft = match skins::from_pack(&text) {
        Ok(draft) => draft,
        Err(findings) => {
            return view(
                state,
                t.skin_import_refused().into(),
                false,
                refuse(findings),
                None,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    let checked = skins::check(&draft, Source::Imported, None);
    if checked.skin.is_none() {
        return view(
            state,
            t.skin_import_refused().into(),
            false,
            refuse(checked.findings),
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    let warnings: Vec<Finding> = checked.warnings().cloned().collect();
    let current = match skins::store::row_version(&state.db, &draft.manifest.id).await {
        Ok(current) => current,
        Err(error) => return database_error(state, error),
    };
    let digest = digest(&warnings);
    let ready = match &confirmed {
        Some((replaces, seen)) => *seen == digest && *replaces == current.unwrap_or(0),
        None => warnings.is_empty() && current.is_none(),
    };
    if !ready {
        let replaces_version = match current {
            Some(_) => match catalog(state).await {
                Ok(catalog) => catalog
                    .listed
                    .iter()
                    .find(|entry| entry.id == draft.manifest.id && entry.source == Source::Imported)
                    .map(|entry| entry.version().to_owned())
                    .unwrap_or_default(),
                Err(error) => return database_error(state, error),
            },
            None => String::new(),
        };
        let confirm = Confirm {
            name: draft.manifest.name_in(t).to_owned(),
            version: draft.manifest.version.clone(),
            warnings: warnings.iter().map(|finding| finding.text(t)).collect(),
            replaces: current.unwrap_or(0),
            replaces_version,
            pack: text,
            digest,
        };
        return view(
            state,
            String::new(),
            false,
            Vec::new(),
            Some(confirm),
            StatusCode::OK,
        )
        .await;
    }
    let replaces = confirmed.map_or(0, |(replaces, _)| replaces);
    match skins::store::import(
        &state.db,
        &state.global,
        actor(state),
        &draft,
        &warnings,
        replaces,
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/skins?saved=imported"),
        Err(error) => failed(state, error).await,
    }
}

/// The upload: `_csrf` first, then the pack file.
async fn import(
    AdminPage(state): AdminPage,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if let Some(reason) = auth::origin_failure(&headers) {
        return auth::csrf_error(&state, &reason);
    }
    let t = crate::i18n::current();
    let mut csrf_checked = false;
    let mut pack: Option<Vec<u8>> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => {
                return view(
                    &state,
                    t.skin_error_file().into(),
                    false,
                    Vec::new(),
                    None,
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await;
            }
        };
        let name = field.name().unwrap_or("").to_owned();
        let mut field = field;
        let mut bytes = Vec::new();
        let limit = if name == "pack" {
            skins::MAX_PACK_BYTES
        } else {
            256
        };
        let mut too_long = false;
        while let Ok(Some(chunk)) = field.chunk().await {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > limit {
                too_long = true;
                break;
            }
        }
        match name.as_str() {
            "_csrf" if !csrf_checked => {
                let value = String::from_utf8(bytes).unwrap_or_default();
                if let Some(reason) = auth::token_failure(&state, &value) {
                    return auth::csrf_error(&state, &reason);
                }
                csrf_checked = true;
            }
            "pack" if csrf_checked && !too_long => pack = Some(bytes),
            "pack" if too_long => {
                return view(
                    &state,
                    t.skin_import_refused().into(),
                    false,
                    vec![t.skin_pack_too_long(skins::MAX_PACK_BYTES / 1024)],
                    None,
                    StatusCode::UNPROCESSABLE_ENTITY,
                )
                .await;
            }
            "pack" => {
                return auth::csrf_error(&state, t.settings_error_upload_token_order());
            }
            _ => {}
        }
    }
    let Some(pack) = pack.filter(|pack| !pack.is_empty()) else {
        return view(
            &state,
            t.skin_error_file().into(),
            false,
            Vec::new(),
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let Ok(text) = String::from_utf8(pack) else {
        return view(
            &state,
            t.skin_import_refused().into(),
            false,
            vec![t.skin_pack_format().into()],
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    check_pack(&state, text, None).await
}

#[derive(Deserialize)]
struct ConfirmForm {
    _csrf: String,
    #[serde(default)]
    pack: String,
    #[serde(default)]
    replaces: i64,
    #[serde(default)]
    digest: String,
}
csrf_forms!(ConfirmForm);

/// The confirmation: the pack comes back in the form and is checked again; it is imported when the warnings and the
/// row it replaces are still those the administrator saw.
async fn confirm_import(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<ConfirmForm>,
) -> Response {
    check_pack(&state, form.pack, Some((form.replaces, form.digest))).await
}

/// A domain's own skin, or `""` to follow the site again.
async fn domain_select(
    AdminPage(state): AdminPage,
    AxumPath(domain_id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ChoiceForm>,
) -> Response {
    let saved = async {
        let chosen = if form.choice.is_empty() {
            None
        } else {
            Some(chosen(&state, &form.choice).await?)
        };
        skins::selection::save(
            &state.db,
            &state.global,
            actor(&state),
            skins::selection::Selection {
                domain_id: Some(domain_id),
                skin: chosen.as_ref().map(|(skin, _)| skin.as_ref()),
                palette: chosen.as_ref().map(|(_, palette)| palette.as_str()),
            },
        )
        .await
    }
    .await;
    match saved {
        Ok(()) => auth::redirect(&format!("/manage/domains/{domain_id}?skin=saved")),
        Err(crate::identity::IdentityError::NotFound) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_error_not_found(),
        ),
        Err(error) => failed(&state, error).await,
    }
}
