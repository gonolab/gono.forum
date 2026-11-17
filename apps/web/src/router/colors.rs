//! Appearance › Colors (`/manage/appearance/colors`, docs/styles.md, "Seed colors"): the site's two seed colors, the
//! colors they derive for the site's skin and the contrast of every pair, a preview for this administrator only, and
//! the way back to the skin's own colors; and a domain's own choice on its page. Every action is a plain form, so the
//! page works without JavaScript; `<input type="color">` falls back to a text field, which is checked the same way.
//! Administrators only (docs/roles.md, "Staff roles").
use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::skins::{actor, catalog, preview_cookie};
use super::*;
use crate::skins::seed::{self, Seeds};
use crate::skins::selection::{self, Colors};
use crate::skins::{self as skin_layer, Skin};
use axum::extract::Path as AxumPath;
use axum::routing::post;

pub(super) const PATH: &str = "/manage/appearance/colors";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PATH, get(page).post(save))
        .route("/manage/appearance/colors/show", post(show))
        .route("/manage/appearance/colors/preview", post(preview))
        .route("/manage/appearance/colors/restore", post(restore))
        .route("/manage/domains/{id}/colors", post(domain_save))
}

/// A token's value in one mode: as written in the stylesheet, and the opaque color the swatch paints.
struct Swatch {
    value: String,
    paint: String,
}

struct DerivedRow {
    name: String,
    light: Option<Swatch>,
    dark: Option<Swatch>,
}

impl DerivedRow {
    /// The light and dark cells, in column order.
    fn cells(&self) -> [Option<&Swatch>; 2] {
        [self.light.as_ref(), self.dark.as_ref()]
    }
}

/// One pair's contrast in one mode, with the two painted colors of its sample.
struct Ratio {
    text: String,
    front: String,
    back: String,
    low: bool,
}

struct PairRow {
    front: &'static str,
    back: &'static str,
    min: String,
    light: Ratio,
    dark: Ratio,
}

impl PairRow {
    fn cells(&self) -> [&Ratio; 2] {
        [&self.light, &self.dark]
    }
}

#[derive(Template)]
#[template(path = "manage/colors.html")]
struct ColorsPage<'a> {
    p: PageMeta<'a>,
    message: String,
    success: bool,
    /// What the site uses now.
    current: String,
    /// The site has seed colors, so it can go back to the skin's own.
    seeded: bool,
    primary: String,
    secondary: String,
    derived_lead: String,
    derived: Vec<DerivedRow>,
    pairs: Vec<PairRow>,
}

/// The site's skin and palette as written, and its seed colors.
pub(super) struct SiteColors {
    pub skin: Arc<Skin>,
    pub palette: String,
    pub seeds: Option<Seeds>,
}

pub(super) async fn site_colors(state: &AppState) -> anyhow::Result<SiteColors> {
    let block = state.global.settings(&state.db).await?;
    let catalog = catalog(state).await?;
    let (id, palette) = selection::choose(block.layout.as_ref(), None);
    let skin = catalog
        .get(id)
        .cloned()
        .unwrap_or_else(|| skin_layer::DEFAULT_SKIN.clone());
    let palette = skin.palette(palette).to_owned();
    Ok(SiteColors {
        palette,
        seeds: selection::colors(block.layout.as_ref(), None),
        skin,
    })
}

/// The seeds that leave a skin's palette closest to itself: its light primary fill and its light muted text, whose
/// hue the neutrals already have. What the fields show before any seeds were chosen.
pub(super) fn skin_seeds(skin: &Skin, palette: &str) -> (String, String) {
    let merged = skin.merged(palette);
    let pick = |name: &str, fallback: &str| {
        seed::painted(&merged.light, name, false).map_or_else(|| fallback.to_owned(), |c| c.hex())
    };
    (
        pick("--primary-bg", "#4f46e5"),
        pick("--text-muted", "#57637a"),
    )
}

fn swatch(
    value: Option<&String>,
    tokens: &crate::skins::palette::Tokens,
    name: &str,
    dark: bool,
) -> Option<Swatch> {
    let value = value?;
    Some(Swatch {
        value: value.clone(),
        paint: seed::painted(tokens, name, dark).map_or_else(String::new, |c| c.hex()),
    })
}

fn ratio(
    t: crate::i18n::Lang,
    measured: Option<f64>,
    min: f64,
    tokens: &crate::skins::palette::Tokens,
    front: &str,
    back: &str,
    dark: bool,
) -> Ratio {
    let paint =
        |name: &str| seed::painted(tokens, name, dark).map_or_else(String::new, |c| c.hex());
    let (text, low) = match measured {
        // Shown rounded down, like the skin checks, so a pair that reads 4.50 really has 4.5:1.
        Some(value) => {
            let shown = format!("{:.2}", (value * 100.0).floor() / 100.0);
            if value + 1e-9 < min {
                (t.skin_colors_ratio_low(shown), true)
            } else {
                (t.skin_colors_ratio(shown), false)
            }
        }
        None => (t.skin_colors_ratio_unknown().to_owned(), true),
    };
    Ratio {
        text,
        front: paint(front),
        back: paint(back),
        low,
    }
}

/// Renders the page for `shown` (entered, saved, or the skin's own as a starting point).
async fn view(
    state: &AppState,
    shown: Option<(String, String)>,
    message: String,
    success: bool,
    status: StatusCode,
) -> Response {
    let t = crate::i18n::current();
    let site = match site_colors(state).await {
        Ok(site) => site,
        Err(error) => return database_error(state, error),
    };
    let name = site.skin.name(t);
    let current = match site.seeds {
        Some(seeds) => t.skin_colors_state_seeds(
            seed::hex(seeds.primary),
            seed::hex(seeds.secondary),
            name.clone(),
        ),
        None => t.skin_colors_state_skin(name.clone()),
    };
    let (primary, secondary) = shown.unwrap_or_else(|| match site.seeds {
        Some(seeds) => (seed::hex(seeds.primary), seed::hex(seeds.secondary)),
        None => skin_seeds(&site.skin, &site.palette),
    });
    let mut derived = Vec::new();
    let mut pairs = Vec::new();
    let mut derived_lead = String::new();
    // Entered values that do not parse are shown as entered, without a derivation.
    if let Some(seeds) = Seeds::parse(&primary, &secondary) {
        derived_lead = t.skin_colors_derived_lead(
            seed::hex(seeds.primary),
            seed::hex(seeds.secondary),
            name,
            site.palette.clone(),
        );
        let variant = site.skin.colored(seeds);
        let before = site.skin.merged(&site.palette);
        let after = variant.merged(&site.palette);
        for changed in seed::changed(before, after) {
            derived.push(DerivedRow {
                light: swatch(changed.light.as_ref(), &after.light, &changed.name, false),
                dark: swatch(changed.dark.as_ref(), &after.dark, &changed.name, true),
                name: changed.name,
            });
        }
        for pair in seed::measure(after) {
            pairs.push(PairRow {
                front: pair.front,
                back: pair.back,
                min: t.skin_colors_ratio(format!("{}", pair.min)),
                light: ratio(
                    t,
                    pair.light,
                    pair.min,
                    &after.light,
                    pair.front,
                    pair.back,
                    false,
                ),
                dark: ratio(
                    t,
                    pair.dark,
                    pair.min,
                    &after.dark,
                    pair.front,
                    pair.back,
                    true,
                ),
            });
        }
    }
    let mut response = render(&ColorsPage {
        p: meta(
            state,
            t.skin_colors_page_title().into(),
            t.skin_colors_page_description().into(),
            PATH,
            "",
            "",
        ),
        message,
        success,
        current,
        seeded: site.seeds.is_some(),
        primary,
        secondary,
        derived_lead,
        derived,
        pairs,
    });
    *response.status_mut() = status;
    response
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SavedQuery {
    saved: String,
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    let t = crate::i18n::current();
    let message = match query.saved.as_str() {
        "saved" => t.skin_colors_saved(),
        "restored" => t.skin_colors_restored(),
        _ => "",
    };
    view(
        &state,
        None,
        message.into(),
        !message.is_empty(),
        StatusCode::OK,
    )
    .await
}

#[derive(Deserialize)]
struct SeedsForm {
    _csrf: String,
    #[serde(default)]
    primary: String,
    #[serde(default)]
    secondary: String,
}
csrf_forms!(SeedsForm);

impl SeedsForm {
    fn entered(&self) -> (String, String) {
        (
            self.primary.trim().to_owned(),
            self.secondary.trim().to_owned(),
        )
    }
}

/// The page again with the entered values and the format error, 422.
async fn refused(state: &AppState, form: &SeedsForm) -> Response {
    view(
        state,
        Some(form.entered()),
        crate::i18n::current().skin_colors_error_format().into(),
        false,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await
}

async fn failed(state: &AppState, error: crate::identity::IdentityError) -> Response {
    use crate::identity::IdentityError;
    match error {
        IdentityError::Validation(message) => {
            view(
                state,
                None,
                message.into(),
                false,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        IdentityError::Conflict(message) => {
            view(state, None, message.into(), false, StatusCode::CONFLICT).await
        }
        error => auth::identity_error(state, error),
    }
}

async fn save(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<SeedsForm>) -> Response {
    let Some(seeds) = Seeds::parse(&form.primary, &form.secondary) else {
        return refused(&state, &form).await;
    };
    match selection::save_colors(
        &state.db,
        &state.global,
        actor(&state),
        None,
        Colors::Seeds(seeds),
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("{PATH}?saved=saved")),
        Err(error) => failed(&state, error).await,
    }
}

/// The derivation of the entered colors, without saving anything.
async fn show(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<SeedsForm>) -> Response {
    if Seeds::parse(&form.primary, &form.secondary).is_none() {
        return refused(&state, &form).await;
    }
    view(
        &state,
        Some(form.entered()),
        String::new(),
        false,
        StatusCode::OK,
    )
    .await
}

/// Shows the entered colors on every page to this administrator only, over the site's skin, until they publish or
/// exit the preview (docs/skins.md, "Preview").
async fn preview(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<SeedsForm>) -> Response {
    let Some(seeds) = Seeds::parse(&form.primary, &form.secondary) else {
        return refused(&state, &form).await;
    };
    let site = match site_colors(&state).await {
        Ok(site) => site,
        Err(error) => return database_error(&state, error),
    };
    match state.identity.seal_skin_preview(
        actor(&state).uid,
        site.skin.id(),
        &site.palette,
        Colors::Seeds(seeds),
    ) {
        Ok(sealed) => {
            let mut response = auth::redirect("/");
            preview_cookie(&mut response, &sealed, crate::identity::SKIN_PREVIEW_TTL);
            response
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

#[derive(Deserialize)]
struct CsrfOnly {
    _csrf: String,
}
csrf_forms!(CsrfOnly);

async fn restore(AdminPage(state): AdminPage, CsrfForm(_): CsrfForm<CsrfOnly>) -> Response {
    match selection::save_colors(
        &state.db,
        &state.global,
        actor(&state),
        None,
        Colors::Inherit,
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("{PATH}?saved=restored")),
        Err(error) => failed(&state, error).await,
    }
}

/// The colors section of a domain's page.
pub(super) struct DomainColors {
    /// `site`, `skin` or `own`.
    pub mode: &'static str,
    pub primary: String,
    pub secondary: String,
    /// What the site uses, for the "Same as the whole site" choice.
    pub site_state: String,
    pub saved: bool,
    pub error: String,
}

/// The colors section of a saved domain other than the main one: its own choice, with the fields showing its own
/// seeds, else the site's, else seeds close to its skin's own colors. `entered` redisplays a refused form.
pub(super) async fn domain_colors(
    state: &AppState,
    domain_id: i64,
    saved: bool,
    entered: Option<(&'static str, String, String)>,
) -> anyhow::Result<DomainColors> {
    let t = crate::i18n::current();
    let block = state.global.settings(&state.db).await?;
    let site = site_colors(state).await?;
    let own = block
        .domain_layouts
        .iter()
        .find(|(id, _)| *id == domain_id)
        .map_or(Colors::Inherit, |(_, layout)| layout.colors);
    let site_state = match site.seeds {
        Some(seeds) => t.skin_colors_state_seeds(
            seed::hex(seeds.primary),
            seed::hex(seeds.secondary),
            site.skin.name(t),
        ),
        None => t.skin_colors_state_skin(site.skin.name(t)),
    };
    let (mode, primary, secondary, error) = match entered {
        Some((mode, primary, secondary)) => (
            mode,
            primary,
            secondary,
            t.skin_colors_error_format().to_owned(),
        ),
        None => {
            let mode = match own {
                Colors::Inherit => "site",
                Colors::Skin => "skin",
                Colors::Seeds(_) => "own",
            };
            let (primary, secondary) = match (own, site.seeds) {
                (Colors::Seeds(seeds), _) | (_, Some(seeds)) => {
                    (seed::hex(seeds.primary), seed::hex(seeds.secondary))
                }
                _ => skin_seeds(&site.skin, &site.palette),
            };
            (mode, primary, secondary, String::new())
        }
    };
    Ok(DomainColors {
        mode,
        primary,
        secondary,
        site_state,
        saved,
        error,
    })
}

#[derive(Deserialize)]
struct DomainForm {
    _csrf: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    primary: String,
    #[serde(default)]
    secondary: String,
    /// `preview` shows the choice on this domain to the administrator alone instead of saving it.
    #[serde(default)]
    operation: String,
}
csrf_forms!(DomainForm);

/// A domain's colors: the site's (`site`), its skin's own (`skin`) or its own seeds (`own`).
/// Shows a domain's colors to this administrator alone, on that domain's address (docs/skins.md, "Preview"), over the
/// skin and palette the domain uses. Following the site previews the site's colors there.
async fn domain_preview(state: &AppState, domain_id: i64, colors: Colors) -> Response {
    let block = match state.global.settings(&state.db).await {
        Ok(block) => block,
        Err(error) => return auth::identity_error(state, error),
    };
    let found: Option<i64> = match crate::db::query_scalar(
        "SELECT domain_id FROM gonof_common_site_domain WHERE domain_id=?",
    )
    .bind(domain_id)
    .fetch_optional(state.db.executor())
    .await
    {
        Ok(found) => found,
        Err(error) => return database_error(state, error.into()),
    };
    if found.is_none() {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_error_not_found(),
        );
    }
    let layout = block
        .domain_layouts
        .iter()
        .find(|(id, _)| *id == domain_id)
        .map(|(_, layout)| layout);
    let (skin, palette) = selection::choose(block.layout.as_ref(), layout);
    let colors = match colors {
        Colors::Inherit => match selection::colors(block.layout.as_ref(), None) {
            Some(seeds) => Colors::Seeds(seeds),
            None => Colors::Skin,
        },
        own => own,
    };
    match state.identity.seal_domain_colors_preview(
        actor(state).uid,
        (skin, palette.unwrap_or("")),
        colors,
        domain_id,
    ) {
        Ok(sealed) => {
            let mut response = auth::redirect("/");
            preview_cookie(&mut response, &sealed, crate::identity::SKIN_PREVIEW_TTL);
            response
        }
        Err(error) => auth::identity_error(state, error),
    }
}

async fn domain_save(
    AdminPage(state): AdminPage,
    AxumPath(domain_id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<DomainForm>,
) -> Response {
    let colors = match form.mode.as_str() {
        "site" => Colors::Inherit,
        "skin" => Colors::Skin,
        "own" => match Seeds::parse(&form.primary, &form.secondary) {
            Some(seeds) => Colors::Seeds(seeds),
            None => {
                return super::domains::colors_refused(
                    &state,
                    domain_id,
                    (
                        "own",
                        form.primary.trim().to_owned(),
                        form.secondary.trim().to_owned(),
                    ),
                )
                .await;
            }
        },
        _ => {
            return error_page(
                &state,
                StatusCode::UNPROCESSABLE_ENTITY,
                crate::i18n::current().skin_colors_error_format(),
            );
        }
    };
    if form.operation == "preview" {
        return domain_preview(&state, domain_id, colors).await;
    }
    match selection::save_colors(
        &state.db,
        &state.global,
        actor(&state),
        Some(domain_id),
        colors,
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("/manage/domains/{domain_id}?colors=saved")),
        Err(crate::identity::IdentityError::NotFound) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().account_error_not_found(),
        ),
        Err(error) => auth::identity_error(&state, error),
    }
}
