//! Page frame snapshot: site settings, visible forums, brand assets and navigation.
//!
//! Site settings and navigation (fetched by the same SQL), brand images, and readable forums (one copy for anonymous
//! visitors and one for members) all come from the global process cache ([`crate::cache`];
//! docs/settings-and-caching.md, "Reading settings" and "The process cache"), with no database reads within the
//! lifetime; when `settings.cache.ttl_seconds` is 0 every request reads the database as usual.
//!
//! The page frame only decides page presentation and whether URLs resolve; it does not do access control: the
//! visibility of threads, posts and lists is judged on the spot by the `forum_readable_sql` / `readable_thread_sql`
//! predicates in their own SQL. Write decisions do not look at the page frame either: protected write transactions
//! still read the database with locks through the write gate `GATE_SQL`.
use std::sync::Arc;

use super::{AppState, NavigationLink};

use crate::{
    identity::IdentityError, interaction::forum_filter::ForumFilters, media::BrandAsset,
    organization::ForumRecord, runtime::SiteSettings,
};

#[derive(Clone, Debug)]
pub(super) struct SiteFrame {
    /// Whether the page frame has read the settings for this request. When it has not (an error in the middleware
    /// stage, broken settings) the page uses the simplified error page that does not depend on settings, see
    /// `router::error_page`.
    pub loaded: bool,
    pub settings: Arc<SiteSettings>,
    /// Site URL used for this request's outbound links, without the trailing slash: the forum URL, or derived from
    /// this request's `Host` when left empty ([`crate::runtime::site_origin`]). When neither is available the page
    /// frame fails to load and the page shows the settings error page.
    pub site_url: String,
    /// Forums readable by this visitor within this site's scope (the anonymous or member copy), by
    /// `displayorder, fid`. When multi-domain sites are off or the scope is unrestricted, this is the same copy as in
    /// the process cache.
    pub forums: Arc<Vec<ForumRecord>>,
    /// All forums readable by this visitor, ignoring the site scope: out-of-scope URLs use it to redirect to the
    /// canonical domain.
    pub all_forums: Arc<Vec<ForumRecord>>,
    /// Forum IDs of `all_forums`: the readable scope of read paths such as thread details, floor positioning and cards
    /// is judged by it (the thread row has no copy of its forum's visibility).
    pub readable_fids: Vec<i64>,
    /// When this site's scope is not everything: readable forums within the scope (aggregate lists and statistics use
    /// it); `None` when unrestricted.
    pub scope: Option<Scope>,
    pub brands: Arc<Vec<BrandAsset>>,
    pub navigation: Vec<NavigationLink>,
    /// "All forums" (`/forums`) at the end of the header navigation, lowest priority, see
    /// [`super::nav_fit::fit_tail`].
    pub directory: NavigationLink,
    /// The signed-in member's own forum hides and blocks, already expanded over parent-child links by `forums`; empty
    /// for anonymous visitors.
    pub forum_filters: ForumFilters,
    /// This request's reading and display preferences: the signed-in member's own choices; items not chosen (column
    /// NULL) and anonymous visitors take the site defaults `settings.reading.*`. Page handlers always use this copy
    /// instead of reading the database each on their own.
    pub reading: crate::reading::Reading,
    /// The skin of this request (docs/skins.md, "Choosing a skin"): the administrator's preview, the domain's choice,
    /// the site's, or the built-in default when the chosen one cannot be used.
    pub skin: Arc<crate::skins::Skin>,
    /// The palette of [`Self::skin`] this request uses.
    pub palette: String,
    /// The skin comes from the viewer's preview, not from the site's choice.
    pub preview: bool,
    /// The preview is one of unsaved colors (docs/styles.md, "Seed colors") rather than of a skin.
    pub preview_colors: bool,
    /// The preview is one of a layout draft (docs/skins.md, "Editing a layout"): its places replace the published ones.
    pub preview_layout: bool,
    /// The board stylesheet (docs/skins.md, "Board colors and glyphs"); empty when no board has a color or glyph.
    pub board_style: Arc<crate::organization::style::BoardStyle>,
    /// The places the site's layout document sets, or the administrator's draft of them while previewing it
    /// (docs/skins.md, "Places and blocks").
    pub site_places: Option<crate::layout::Places>,
    /// The places this domain's override document sets, or its draft while previewing.
    pub domain_places: Option<crate::layout::Places>,
}

/// Scope of multi-domain sites: forums readable by this visitor within the scope.
#[derive(Clone, Debug, Default)]
pub(super) struct Scope {
    pub fids: Vec<i64>,
}

/// Placeholder replacing an empty list when the scope is empty (the domain has no forums ticked, or this visitor can
/// read none of the ticked forums): an empty list means "the whole site" in statistics.
/// Forum IDs start at 1, so none can equal it.
const NO_FID: i64 = 0;

impl Default for SiteFrame {
    /// Placeholder before settings are read, see [`SiteFrame::loaded`].
    fn default() -> Self {
        Self {
            loaded: false,
            settings: Arc::new(SiteSettings::blank()),
            site_url: String::new(),
            forums: Arc::default(),
            all_forums: Arc::default(),
            readable_fids: Vec::new(),
            scope: None,
            brands: Arc::default(),
            navigation: Vec::new(),
            directory: directory_link(),
            forum_filters: ForumFilters::default(),
            reading: crate::reading::Reading::blank(),
            skin: crate::skins::DEFAULT_SKIN.clone(),
            palette: crate::skins::DEFAULT_SKIN.palettes[0].clone(),
            preview: false,
            preview_colors: false,
            preview_layout: false,
            board_style: Arc::default(),
            site_places: None,
            domain_places: None,
        }
    }
}

impl SiteFrame {
    /// Forum visibility has only two values, anonymous and signed in — `forum_readable_sql` does not bind a uid and
    /// only chooses the `visibility` set by `viewer_uid.is_some()`, so the process cache stores one copy for each.
    pub(super) async fn load(
        state: &AppState,
        viewer_uid: Option<i64>,
    ) -> Result<Self, IdentityError> {
        let block = state.global.settings(&state.db).await?;
        let mut settings = block.settings.clone();
        // Other domains use their own site name and search engine verification codes: the page frame swaps in a copy,
        // so the title suffix, `og:site_name`, structured data and verification codes change accordingly; the admin
        // edit page reads the database and is unaffected.
        if let Some((name, verification)) = crate::site::other(|row| {
            (
                row.site_name.clone().unwrap_or_default(),
                row.verification.clone().unwrap_or_default(),
            )
        }) {
            let own = Arc::make_mut(&mut settings);
            if !name.is_empty() {
                own.site.name = name;
            }
            own.seo.verification = verification;
        }
        // Values left empty show the product name or this language's words (docs/architecture.md, "Languages").
        // Sites that fill them all keep sharing the cached copy.
        if settings.needs_defaults() {
            Arc::make_mut(&mut settings).fill_defaults(crate::i18n::current());
        }
        let site_url = crate::runtime::site_origin(&settings, state.viewer.origin.as_deref())
            .map_err(|error| IdentityError::Internal(anyhow::Error::new(error)))?;
        let ttl = settings.cache_ttl();
        let mut brands = state
            .global
            .branding(&state.media, ttl)
            .await
            .map_err(IdentityError::Internal)?;
        // Other domains that set a Logo or favicon switch to their own (the URL carries the domain ID, see
        // `BrandAsset::path`).
        if let Some(own) = crate::site::other(|row| {
            (
                row.domain_id,
                [("logo", row.logo_aid), ("favicon", row.favicon_aid)],
            )
        }) && own.1.iter().any(|(_, aid)| aid.is_some())
        {
            let (domain_id, slots) = own;
            let mut assets = (*brands).clone();
            for asset in &mut assets {
                if let Some((_, Some(aid))) = slots.iter().find(|(slot, _)| *slot == asset.slot) {
                    asset.aid = Some(*aid);
                    asset.domain = Some(domain_id);
                    asset.width = None;
                    asset.height = None;
                }
            }
            brands = Arc::new(assets);
        }
        let all_forums = state
            .global
            .forums(&state.db, viewer_uid.is_some(), ttl)
            .await
            .map_err(IdentityError::Internal)?;
        // Scope of multi-domain sites: keep only those whose top-level forum is within the scope; if none
        // was removed, reuse the cached copy, with keys and queries the same as now.
        let (forums, scope, out_of_scope) = scoped(&all_forums);
        let (forum_filters, reading) = match viewer_uid {
            Some(uid) => {
                let data = crate::cache::member::load(
                    &state.db,
                    &state.cache,
                    crate::cache::Lifetimes::from_settings(&settings),
                    uid,
                )
                .await?;
                let (choices, version, css_version) = data.reading;
                let filters = ForumFilters::new(
                    data.filters,
                    forums.iter().map(|forum| (forum.fid, forum.parent_fid)),
                );
                let reading = crate::reading::Reading::resolve(
                    choices,
                    crate::reading::Values::site(&settings.reading),
                    version,
                    css_version,
                );
                (filters, reading)
            }
            None => (
                ForumFilters::default(),
                crate::reading::Reading::site_default(&settings.reading),
            ),
        };
        let forum_filters = forum_filters.with_scope(out_of_scope);
        // When the navigation document is empty, only top-level forums are listed: child forums are reached from the
        // tabs of the parent forum page and are not squeezed into the header.
        let mut navigation: Vec<NavigationLink> = match &block.navigation_entries {
            None => forums
                .iter()
                .filter(|forum| forum.parent_fid.is_none())
                .map(|forum| NavigationLink {
                    label: forum.name.clone(),
                    href: format!("/{}", forum.slug),
                    active: false,
                    fit: 0,
                })
                .collect(),
            Some(entries) => entries
                .iter()
                .filter_map(|entry| {
                    let href = if let Some(fid) = entry
                        .target
                        .strip_prefix("forum:")
                        .and_then(|value| value.parse::<i64>().ok())
                    {
                        format!("/{}", forums.iter().find(|forum| forum.fid == fid)?.slug)
                    } else {
                        entry.target.clone()
                    };
                    Some(NavigationLink {
                        label: entry.label.clone(),
                        href,
                        active: false,
                        fit: 0,
                    })
                })
                .collect(),
        };
        // The room the navigation gets depends on the brand text and buttons this page shows (docs/architecture.md,
        // "Languages"): another domain's site name or the brand text setting, and this language's words.
        let lang = crate::i18n::current();
        let brand = crate::site::other(|row| row.site_name.clone())
            .flatten()
            .unwrap_or_else(|| settings.site.header_brand().to_owned());
        let (log_in, register, more) = (
            lang.frame_log_in(),
            lang.frame_register(),
            lang.frame_more(),
        );
        let header = super::nav_fit::Header {
            brand: &brand,
            log_in,
            register,
            more,
        };
        super::nav_fit::fit(&mut navigation, &header);
        let mut directory = directory_link();
        super::nav_fit::fit_tail(&navigation, &mut directory, &header);
        let (skin, palette, preview, preview_colors, draft) =
            skin(state, &block, viewer_uid, ttl).await;
        let board_style = state
            .global
            .board_style(&state.db, ttl)
            .await
            .map_err(IdentityError::Internal)?;
        // The page frame is the only place this request fetches site settings, so the time basis is written into the
        // task-local storage along the way, sparing every call site that displays a time from passing it all the way
        // down. The whole site uses only the site time zone; members have no time zone of their own.
        crate::clock::set(crate::clock::TimeFormat::from(&*settings));
        let domain_id = crate::site::other(|row| row.domain_id);
        let mut site_places = block.places.clone();
        let mut domain_places = domain_id.and_then(|id| {
            block
                .domain_places
                .iter()
                .find(|(domain, _)| *domain == id)
                .map(|(_, places)| places.clone())
        });
        // A previewed draft replaces the places it is the draft of: the site's on every domain, a domain's on that
        // domain only. A draft that cannot be read shows the published places and is logged.
        let mut preview_layout = false;
        if let Some(draft) = draft
            && (draft.domain.is_none() || draft.domain == domain_id)
        {
            match crate::skins::selection::read_draft(&state.db, draft.domain).await {
                Ok(places) => {
                    preview_layout = true;
                    let places = places.filter(|places| !places.is_empty());
                    match draft.domain {
                        None => site_places = places,
                        Some(_) => domain_places = places,
                    }
                }
                Err(error) => tracing::error!(%error, "a previewed layout draft could not be read"),
            }
        }
        Ok(Self {
            loaded: true,
            settings,
            site_url,
            forums,
            readable_fids: all_forums.iter().map(|forum| forum.fid).collect(),
            all_forums,
            scope,
            brands,
            navigation,
            directory,
            forum_filters,
            reading,
            skin,
            palette,
            preview,
            preview_colors,
            preview_layout,
            board_style,
            site_places,
            domain_places,
        })
    }

    /// Finds a forum readable by this visitor by FID.
    pub(super) fn forum_by_fid(&self, fid: i64) -> Option<&ForumRecord> {
        self.forums.iter().find(|forum| forum.fid == fid)
    }

    /// The forum's parent forum (`None` for top-level forums). For a readable child forum, its parent forum is
    /// necessarily readable too, and is in the same list.
    pub(super) fn parent_of(&self, forum: &ForumRecord) -> Option<&ForumRecord> {
        forum
            .parent_fid
            .and_then(|parent| self.forum_by_fid(parent))
    }

    /// The forum is readable by anonymous visitors: forums in the page frame are all enabled, so check that it and its
    /// parent forum are both public. The signed-in member copy also contains member-visible forums; structured data
    /// (`BreadcrumbList`) is output only when this holds.
    pub(super) fn anonymous_readable(&self, forum: &ForumRecord) -> bool {
        forum.visibility == "public"
            && match forum.parent_fid {
                None => true,
                Some(_) => self
                    .parent_of(forum)
                    .is_some_and(|parent| parent.visibility == "public"),
            }
    }

    pub(super) fn forum(&self, slug: &str) -> Option<&ForumRecord> {
        if slug.is_empty() {
            return None;
        }
        self.forums.iter().find(|forum| forum.slug == slug)
    }

    /// Which forums' threads lists may show (fids are computed in code by forum permission): forums this visitor can
    /// read within this site's scope (the page frame takes them from the process forum cache, so disabled, private
    /// and member forums anonymous visitors cannot read are not in it), minus `excluded` (forums the member hid or
    /// blocked). Used by the home page, search and the right sidebar.
    pub(super) fn listable_forums(&self, excluded: &[i64]) -> Vec<i64> {
        self.forums
            .iter()
            .map(|forum| forum.fid)
            .filter(|fid| !excluded.contains(fid))
            .collect()
    }

    /// Which forums community statistics count: empty (the whole site) when the scope is unrestricted; a nonexistent
    /// forum ID when the scope is empty.
    pub(super) fn statistics_fids(&self) -> Vec<i64> {
        match &self.scope {
            None => Vec::new(),
            Some(scope) if scope.fids.is_empty() => vec![NO_FID],
            Some(scope) => scope.fids.clone(),
        }
    }

    /// An out-of-scope forum readable by this visitor: where to redirect. `None` when within the scope or unreadable.
    pub(super) fn elsewhere(&self, forum: &ForumRecord) -> Option<crate::site::Elsewhere> {
        crate::site::elsewhere(forum.parent_fid.unwrap_or(forum.fid))
    }

    /// Finds a forum by slug among all forums readable by this visitor, ignoring the scope.
    pub(super) fn any_forum(&self, slug: &str) -> Option<&ForumRecord> {
        if slug.is_empty() {
            return None;
        }
        self.all_forums.iter().find(|forum| forum.slug == slug)
    }

    pub(super) fn any_forum_by_fid(&self, fid: i64) -> Option<&ForumRecord> {
        self.all_forums.iter().find(|forum| forum.fid == fid)
    }
}

/// The skin (with the seed colors in use, docs/styles.md, "Seed colors"), its palette and whether it comes from a
/// preview of a skin or of colors. A broken skin never fails the page: the catalog failing to load, or the chosen
/// skin being unusable (deleted, shadowed, failing its checks after an upgrade), falls back to the built-in default
/// and logs an error (at most once a minute per skin, so a busy site does not flood the log); without a catalog the
/// default has no seed colors either.
async fn skin(
    state: &AppState,
    block: &crate::cache::SettingsBlock,
    viewer_uid: Option<i64>,
    ttl: u64,
) -> (
    Arc<crate::skins::Skin>,
    String,
    bool,
    bool,
    Option<crate::identity::LayoutDraft>,
) {
    let domain = crate::site::other(|row| row.domain_id).and_then(|id| {
        block
            .domain_layouts
            .iter()
            .find(|(domain, _)| *domain == id)
            .map(|(_, layout)| layout)
    });
    let (mut wanted, mut palette) = crate::skins::selection::choose(block.layout.as_ref(), domain);
    let mut colors = crate::skins::selection::colors(block.layout.as_ref(), domain);
    let preview = state
        .viewer
        .session
        .as_ref()
        .filter(|session| session.member.is_admin)
        .zip(viewer_uid)
        .and_then(|(_, uid)| {
            state
                .identity
                .open_skin_preview(&state.viewer.skin_preview, uid)
        });
    if let Some(preview) = &preview {
        wanted = &preview.skin;
        palette = Some(&preview.palette);
        match preview.colors {
            crate::skins::selection::Colors::Inherit => {}
            crate::skins::selection::Colors::Skin => colors = None,
            crate::skins::selection::Colors::Seeds(seeds) => colors = Some(seeds),
        }
    }
    let catalog = match state.global.skins(&state.db, &state.skins_dir, ttl).await {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            unusable(wanted, &format!("the skins could not be loaded: {error:#}"));
            None
        }
    };
    let skin = match &catalog {
        Some(catalog) => {
            let skin = match catalog.get(wanted) {
                Some(skin) => skin,
                None => {
                    unusable(wanted, "no usable skin has this id");
                    catalog
                        .get(crate::skins::DEFAULT_ID)
                        .unwrap_or(&*crate::skins::DEFAULT_SKIN)
                }
            };
            catalog.colored(skin, colors, true)
        }
        None => crate::skins::DEFAULT_SKIN.clone(),
    };
    let palette = skin.palette(palette).to_owned();
    let preview_colors = preview
        .as_ref()
        .is_some_and(|preview| !preview.colors.inherits());
    let draft = preview.as_ref().and_then(|preview| preview.layout);
    (skin, palette, preview.is_some(), preview_colors, draft)
}

/// Logs that a chosen skin cannot be used, at most once a minute per skin id.
fn unusable(id: &str, reason: &str) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LOGGED: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
    let mut logged = LOGGED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    logged.retain(|(_, at)| at.elapsed() < Duration::from_secs(60));
    if logged.iter().any(|(seen, _)| seen == id) {
        return;
    }
    logged.push((id.to_owned(), Instant::now()));
    tracing::error!(
        skin = id,
        reason,
        "the chosen skin cannot be used; pages use the built-in default skin"
    );
}

/// Filters forums by the scope of this request's site: returns the in-scope list, the scope (`None` when unrestricted)
/// and the out-of-scope forum IDs (ascending).
fn scoped(all: &Arc<Vec<ForumRecord>>) -> (Arc<Vec<ForumRecord>>, Option<Scope>, Vec<i64>) {
    let (inside, excluded) = crate::site::partition_forums(all);
    if excluded.is_empty() {
        return (all.clone(), None, Vec::new());
    }
    let scope = Scope {
        fids: inside.iter().map(|forum| forum.fid).collect(),
    };
    (
        Arc::new(inside.into_iter().cloned().collect()),
        Some(scope),
        excluded,
    )
}

/// For aggregate outputs that bypass the page frame (feeds): same as [`SiteFrame::listable_forums`]; `forums` are the
/// forums this visitor can read, filtered to those within the scope of this request's site.
pub(super) fn listable_forums_of(forums: &[ForumRecord]) -> Vec<i64> {
    let (inside, _) = crate::site::partition_forums(forums);
    inside.iter().map(|forum| forum.fid).collect()
}

/// The "All forums" item at the end of the header navigation.
fn directory_link() -> NavigationLink {
    NavigationLink {
        label: crate::i18n::current().frame_all_forums().to_owned(),
        href: "/forums".into(),
        active: false,
        fit: 0,
    }
}
