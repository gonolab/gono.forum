//! Appearance › Layout (`/manage/layout`, docs/skins.md, "Editing a layout"): the places and the home page of the
//! site, or with `?domain=<id>` of one domain, edited as a draft with plain forms (move up, move down, remove, add,
//! follow the next layout again, choose what the home page opens with), previewed by the administrator alone and
//! published with `layout.save`. Administrators only.
use std::collections::HashMap;

use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::*;
use crate::layout::{self, Block, Core, Kind, Place, Places};
use crate::skins::{manifest::HomeView, selection};
use axum::routing::post;
use serde_json::{Map, Value, json};

pub(super) const PATH: &str = "/manage/layout";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PATH, get(page))
        .route("/manage/layout/add", get(add_page).post(add))
        .route("/manage/layout/move", post(move_block))
        .route("/manage/layout/remove", post(remove))
        .route("/manage/layout/own", post(own))
        .route("/manage/layout/inherit", post(inherit))
        .route("/manage/layout/home", post(home))
        .route("/manage/layout/preview", post(preview))
        .route("/manage/layout/publish", post(publish))
        .route("/manage/layout/discard", post(discard))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PageQuery {
    /// 0 for the site.
    domain: i64,
    saved: String,
    place: String,
    source: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PlaceForm {
    _csrf: String,
    domain: i64,
    place: String,
    index: usize,
    /// `up` or `down`.
    direction: String,
}
csrf_forms!(PlaceForm);

fn target(domain: i64) -> Option<i64> {
    (domain > 0).then_some(domain)
}

fn address(domain: Option<i64>) -> String {
    match domain {
        None => PATH.to_owned(),
        Some(id) => format!("{PATH}?domain={id}"),
    }
}

/// The published places and home page of the site's document or a domain's, read from the database like every edit
/// page; `None` when the document sets neither.
pub(super) async fn published(
    state: &AppState,
    domain: Option<i64>,
) -> anyhow::Result<Option<selection::Draft>> {
    let key = match domain {
        None => format!("{}body", selection::LAYOUT_PREFIX),
        Some(id) => format!("{}body", selection::domain_prefix(id)),
    };
    let body: Option<String> =
        crate::db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(key)
            .fetch_optional(state.db.executor())
            .await?;
    Ok(body
        .and_then(|body| selection::Layout::parse(&body).ok().flatten())
        .and_then(|layout| {
            let places = layout.places("the layout editor");
            (places.is_some() || layout.home.is_some()).then(|| selection::Draft {
                places: places.unwrap_or_default(),
                home: layout.home,
            })
        }))
}

/// What the editor works on: the draft when there is one, else the published places and home page.
struct Working {
    places: Places,
    /// The home page this layout chooses; `None` follows the next layout down.
    home: Option<HomeView>,
    draft: bool,
    /// The places of the next layout down, which a place this one leaves out shows.
    below: Places,
    /// Where `below` comes from, per place.
    below_origin: Vec<(Place, &'static str)>,
    /// The home page of the next layout down and where it comes from.
    below_home: (HomeView, &'static str),
}

async fn working(state: &AppState, domain: Option<i64>) -> anyhow::Result<Working> {
    let draft = selection::read_draft(&state.db, domain).await?;
    let own = match &draft {
        Some(draft) => draft.clone(),
        None => published(state, domain).await?.unwrap_or_default(),
    };
    let block = state.global.settings(&state.db).await?;
    let (site_doc, domain_doc) = match domain {
        None => (block.layout.as_ref(), None),
        Some(id) => (
            block.layout.as_ref(),
            block
                .domain_layouts
                .iter()
                .find(|(own, _)| *own == id)
                .map(|(_, layout)| layout),
        ),
    };
    let skin_id = selection::choose(site_doc, domain_doc).0.to_owned();
    let catalog = state
        .global
        .skins(&state.db, &state.skins_dir, state.cache_ttl())
        .await?;
    let skin = catalog.get(&skin_id);
    let skin_places = skin.and_then(|skin| skin.places.clone());
    let site = match domain {
        None => None,
        Some(_) => published(state, None).await?,
    };
    let site_places = site
        .as_ref()
        .map(|site| site.places.clone())
        .filter(|places| !places.is_empty());
    let t = crate::i18n::current();
    let below_home = match site.as_ref().and_then(|site| site.home) {
        Some(home) => (home, t.layout_from_site()),
        None => (
            skin.map_or(HomeView::Threads, |skin| skin.manifest.frame.home),
            t.layout_from_skin(),
        ),
    };
    let mut below = Places::default();
    let mut below_origin = Vec::new();
    for place in Place::ALL {
        let (blocks, origin) =
            layout::resolve(place, None, site_places.as_ref(), skin_places.as_ref());
        below.set(place, Some(blocks.to_vec()));
        below_origin.push((
            place,
            match origin {
                layout::Taken::Site => t.layout_from_site(),
                layout::Taken::Skin => t.layout_from_skin(),
                _ => t.layout_from_core(),
            },
        ));
    }
    Ok(Working {
        places: own.places,
        home: own.home,
        draft: draft.is_some(),
        below,
        below_origin,
        below_home,
    })
}

struct BlockRow {
    index: usize,
    label: String,
    detail: String,
    show: &'static str,
}

struct PlaceView {
    name: &'static str,
    label: &'static str,
    note: &'static str,
    own: bool,
    origin: &'static str,
    blocks: Vec<BlockRow>,
    capacity: usize,
    /// What the add form offers here: `(source, label)`.
    sources: Vec<(&'static str, String)>,
}

#[derive(Template)]
#[template(path = "manage/layout.html")]
struct LayoutTemplate<'a> {
    p: PageMeta<'a>,
    domain: i64,
    /// The domain's name, or empty for the site.
    domain_name: String,
    domains: Vec<(i64, String)>,
    draft: bool,
    previewing: bool,
    notice: &'static str,
    error: String,
    places: Vec<PlaceView>,
    /// The home page this layout chooses, `None` when it follows the next layout down.
    home: Option<&'static str>,
    /// The home page of the next layout down, worded, and where it comes from.
    below_home: (&'static str, &'static str),
}

/// The words for a home page choice.
fn home_label(home: HomeView) -> &'static str {
    let t = crate::i18n::current();
    match home {
        HomeView::Threads => t.layout_home_threads(),
        HomeView::Boards => t.layout_home_boards(),
    }
}

fn kind_label(kind: Kind) -> String {
    let t = crate::i18n::current();
    match kind {
        Kind::Core(core) => match core {
            Core::MemberCard => t.layout_core_member_card(),
            Core::RecentLocal => t.layout_core_recent_local(),
            Core::SiteOverview => t.layout_core_site_overview(),
            Core::FriendLinks => t.layout_core_friend_links(),
            Core::BoardHead => t.layout_core_board_head(),
            Core::NavBoards => t.layout_core_nav_boards(),
            Core::NavLinks => t.layout_core_nav_links(),
            Core::Markdown => t.layout_core_markdown(),
        },
        Kind::Source(source) => match source {
            crate::data::Source::Threads => t.layout_source_threads(),
            crate::data::Source::Boards => t.layout_source_boards(),
            crate::data::Source::MembersRecent => t.layout_source_members_recent(),
            crate::data::Source::MembersRank => t.layout_source_members_rank(),
            crate::data::Source::MembersHonors => t.layout_source_members_honors(),
            crate::data::Source::MembersVerified => t.layout_source_members_verified(),
            crate::data::Source::Moderators => t.layout_source_moderators(),
            crate::data::Source::SiteStats => t.layout_source_site_stats(),
            crate::data::Source::SiteLinks => t.layout_source_site_links(),
            crate::data::Source::ThreadOutline => t.layout_source_thread_outline(),
            _ => source.name(),
        },
    }
    .to_owned()
}

fn view_label(view: &str) -> &'static str {
    let t = crate::i18n::current();
    match view {
        "mini-list" => t.layout_view_mini_list(),
        "list" => t.layout_view_list(),
        "cards" => t.layout_view_cards(),
        "tree" => t.layout_view_tree(),
        "grid" => t.layout_view_grid(),
        "directory" => t.layout_view_directory(),
        "avatars" => t.layout_view_avatars(),
        "table" => t.layout_view_table(),
        "figures" => t.layout_view_figures(),
        "compact" => t.layout_view_compact(),
        "links" => t.layout_view_links(),
        "timeline" => t.layout_view_timeline(),
        "progress" => t.layout_view_progress(),
        _ => t.layout_view_fixed(),
    }
}

fn place_label(place: Place) -> (&'static str, &'static str) {
    let t = crate::i18n::current();
    match place {
        Place::HomeAside => (
            t.layout_place_home_aside(),
            t.layout_place_home_aside_note(),
        ),
        Place::BoardAside => (
            t.layout_place_board_aside(),
            t.layout_place_board_aside_note(),
        ),
        Place::ThreadAside => (
            t.layout_place_thread_aside(),
            t.layout_place_thread_aside_note(),
        ),
        Place::HeadHome => (t.layout_place_head_home(), t.layout_place_head_note()),
        Place::HeadBoard => (t.layout_place_head_board(), t.layout_place_head_note()),
        Place::HeadThread => (t.layout_place_head_thread(), t.layout_place_head_note()),
        Place::Footer => (t.layout_place_footer(), t.layout_place_footer_note()),
        Place::SideNav => (t.layout_place_side_nav(), t.layout_place_side_nav_note()),
    }
}

/// The kinds a place takes, for the add form.
fn offered(place: Place) -> Vec<(&'static str, String)> {
    crate::data::Source::ALL
        .into_iter()
        .map(Kind::Source)
        .chain(Core::ALL.into_iter().map(Kind::Core))
        .filter(|kind| kind.views().iter().any(|view| place.accepts(*kind, view)))
        .map(|kind| (kind.name(), kind_label(kind)))
        .collect()
}

/// Every domain by id with its address, for the target choice.
async fn domains(state: &AppState) -> anyhow::Result<Vec<(i64, String)>> {
    crate::db::query("SELECT domain_id,site_url FROM gonof_common_site_domain ORDER BY domain_id")
        .fetch_all(state.db.executor())
        .await?
        .iter()
        .map(|row| Ok((row.try_get("domain_id")?, crate::db::text(row, "site_url")?)))
        .collect()
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<PageQuery>) -> Response {
    render_page(&state, target(query.domain), &query.saved, String::new()).await
}

async fn render_page(
    state: &AppState,
    domain: Option<i64>,
    saved: &str,
    error: String,
) -> Response {
    let working = match working(state, domain).await {
        Ok(working) => working,
        Err(error) => return database_error(state, error),
    };
    let domains = match domains(state).await {
        Ok(domains) => domains,
        Err(error) => return database_error(state, error),
    };
    let domain_name = match domain {
        None => String::new(),
        Some(id) => match domains.iter().find(|(own, _)| *own == id) {
            Some((_, host)) => host.clone(),
            None => {
                return error_page(
                    state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().common_error_page_not_found(),
                );
            }
        },
    };
    let t = crate::i18n::current();
    let places = Place::ALL
        .into_iter()
        .map(|place| {
            let (label, note) = place_label(place);
            let own = working.places.get(place);
            let blocks = own
                .or_else(|| working.below.get(place))
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .map(|(index, block)| BlockRow {
                    index,
                    label: block
                        .title
                        .as_ref()
                        .map(|title| title.text(t))
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| kind_label(block.kind)),
                    detail: format!("{} · {}", kind_label(block.kind), view_label(block.view)),
                    show: match block.show {
                        layout::Show::All => "",
                        layout::Show::Guests => t.layout_show_guests(),
                        layout::Show::Members => t.layout_show_members(),
                    },
                })
                .collect();
            PlaceView {
                name: place.name(),
                label,
                note,
                own: own.is_some(),
                origin: working
                    .below_origin
                    .iter()
                    .find(|(own, _)| *own == place)
                    .map_or("", |(_, origin)| origin),
                blocks,
                capacity: place.capacity(),
                sources: offered(place),
            }
        })
        .collect();
    let notice = match saved {
        "published" => t.layout_published(),
        "draft" => t.layout_draft_saved(),
        "discarded" => t.layout_draft_discarded(),
        _ => "",
    };
    render(&LayoutTemplate {
        p: meta(
            state,
            t.layout_page_title().into(),
            t.layout_page_description().into(),
            PATH,
            "",
            "",
        ),
        domain: domain.unwrap_or(0),
        domain_name,
        domains,
        draft: working.draft,
        previewing: state.frame.preview_layout,
        notice,
        error,
        places,
        home: working.home.map(HomeView::name),
        below_home: (home_label(working.below_home.0), working.below_home.1),
    })
}

/// Changes the working places of one place and saves them as the draft; a change the strict checks refuse shows
/// the editor with the problem.
async fn change(
    state: &AppState,
    domain: Option<i64>,
    place: Place,
    edit: impl FnOnce(&mut Vec<Block>) -> bool,
) -> Response {
    let working = match working(state, domain).await {
        Ok(working) => working,
        Err(error) => return database_error(state, error),
    };
    let mut places = working.places;
    let mut blocks = places
        .get(place)
        .or_else(|| working.below.get(place))
        .map(<[Block]>::to_vec)
        .unwrap_or_default();
    if !edit(&mut blocks) {
        return auth::redirect(&address(domain));
    }
    places.set(place, Some(blocks));
    save(state, domain, places, working.home).await
}

async fn save(
    state: &AppState,
    domain: Option<i64>,
    places: Places,
    home: Option<HomeView>,
) -> Response {
    let t = crate::i18n::current();
    if let Err(problem) = Places::parse(&places.json()) {
        let mut response = render_page(state, domain, "", problem.message(t)).await;
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return response;
    }
    let draft = selection::Draft { places, home };
    match selection::write_draft(&state.db, super::skins::actor(state), domain, Some(&draft)).await
    {
        Ok(()) => auth::redirect(&format!(
            "{}{}saved=draft",
            address(domain),
            if domain.is_some() { "&" } else { "?" }
        )),
        Err(error) => auth::identity_error(state, error),
    }
}

#[allow(clippy::result_large_err)]
fn place_of(state: &AppState, name: &str) -> Result<Place, Response> {
    Place::parse(name).ok_or_else(|| {
        error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        )
    })
}

async fn move_block(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<PlaceForm>) -> Response {
    let place = match place_of(&state, &form.place) {
        Ok(place) => place,
        Err(response) => return response,
    };
    let (index, up) = (form.index, form.direction == "up");
    change(&state, target(form.domain), place, |blocks| {
        let other = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        };
        match other.filter(|other| *other < blocks.len() && index < blocks.len()) {
            Some(other) => {
                blocks.swap(index, other);
                true
            }
            None => false,
        }
    })
    .await
}

async fn remove(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<PlaceForm>) -> Response {
    let place = match place_of(&state, &form.place) {
        Ok(place) => place,
        Err(response) => return response,
    };
    change(&state, target(form.domain), place, |blocks| {
        if form.index < blocks.len() {
            blocks.remove(form.index);
            true
        } else {
            false
        }
    })
    .await
}

/// Takes a place into this layout as it shows now, to change it here.
async fn own(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<PlaceForm>) -> Response {
    let place = match place_of(&state, &form.place) {
        Ok(place) => place,
        Err(response) => return response,
    };
    change(&state, target(form.domain), place, |_| true).await
}

/// Leaves a place to the next layout down again.
async fn inherit(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<PlaceForm>) -> Response {
    let place = match place_of(&state, &form.place) {
        Ok(place) => place,
        Err(response) => return response,
    };
    let domain = target(form.domain);
    let working = match working(&state, domain).await {
        Ok(working) => working,
        Err(error) => return database_error(&state, error),
    };
    let mut places = working.places;
    places.set(place, None);
    save(&state, domain, places, working.home).await
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HomeForm {
    _csrf: String,
    domain: i64,
    /// `threads`, `boards`, or `inherit` to follow the next layout down.
    home: String,
}
csrf_forms!(HomeForm);

/// Chooses what the home page opens with (docs/skins.md, "Board directory home"), in the draft.
async fn home(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<HomeForm>) -> Response {
    let home = match form.home.as_str() {
        "inherit" => None,
        name => match HomeView::parse(name) {
            Some(home) => Some(home),
            None => {
                return error_page(
                    &state,
                    StatusCode::BAD_REQUEST,
                    crate::i18n::current().common_error_form(),
                );
            }
        },
    };
    let domain = target(form.domain);
    let working = match working(&state, domain).await {
        Ok(working) => working,
        Err(error) => return database_error(&state, error),
    };
    save(&state, domain, working.places, home).await
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TargetForm {
    _csrf: String,
    domain: i64,
}
csrf_forms!(TargetForm);

async fn preview(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<TargetForm>) -> Response {
    let domain = target(form.domain);
    // The preview keeps the skin and palette the page has anyway.
    let (skin, palette) = (
        state.frame.skin.id().to_owned(),
        state.frame.palette.clone(),
    );
    match state.identity.seal_layout_preview(
        super::skins::actor(&state).uid,
        (&skin, &palette),
        domain,
    ) {
        Ok(sealed) => {
            let mut response = auth::redirect("/");
            super::skins::preview_cookie(&mut response, &sealed, crate::identity::SKIN_PREVIEW_TTL);
            response
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

/// Publishes a draft: its places become the document's (`layout.save`), and the draft and any preview of it end.
pub(super) async fn publish_draft(state: &AppState, domain: Option<i64>) -> Response {
    let actor = super::skins::actor(state);
    let draft = match selection::read_draft(&state.db, domain).await {
        Ok(Some(draft)) => draft,
        Ok(None) => return auth::redirect(&address(domain)),
        Err(error) => return database_error(state, error),
    };
    if let Err(problem) = Places::parse(&draft.places.json()) {
        let mut response =
            render_page(state, domain, "", problem.message(crate::i18n::current())).await;
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return response;
    }
    if let Err(error) =
        selection::save_places(&state.db, &state.global, actor, domain, Some(&draft)).await
    {
        return auth::identity_error(state, error);
    }
    if let Err(error) = selection::write_draft(&state.db, actor, domain, None).await {
        return auth::identity_error(state, error);
    }
    let mut response = auth::redirect(&format!(
        "{}{}saved=published",
        address(domain),
        if domain.is_some() { "&" } else { "?" }
    ));
    super::skins::preview_cookie(&mut response, "", 0);
    response
}

async fn publish(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<TargetForm>) -> Response {
    publish_draft(&state, target(form.domain)).await
}

async fn discard(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<TargetForm>) -> Response {
    let domain = target(form.domain);
    match selection::write_draft(&state.db, super::skins::actor(&state), domain, None).await {
        Ok(()) => {
            let mut response = auth::redirect(&format!(
                "{}{}saved=discarded",
                address(domain),
                if domain.is_some() { "&" } else { "?" }
            ));
            if state.frame.preview_layout {
                super::skins::preview_cookie(&mut response, "", 0);
            }
            response
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

/// One field of the add form, generated from the source's parameter lists.
struct Field {
    name: &'static str,
    label: &'static str,
    hint: &'static str,
    kind: FieldKind,
}

enum FieldKind {
    /// `(value, label)`, the first selected unless the posted value names another.
    Select(Vec<(String, String)>),
    Checkbox,
    Text {
        max: usize,
    },
    Area,
}

#[derive(Template)]
#[template(path = "manage/layout_add.html")]
struct AddTemplate<'a> {
    p: PageMeta<'a>,
    domain: i64,
    place: &'static str,
    place_label: &'static str,
    source: &'static str,
    source_label: String,
    fields: Vec<Field>,
    values: HashMap<String, String>,
    error: String,
}

impl AddTemplate<'_> {
    fn value(&self, name: &str) -> &str {
        self.values.get(name).map_or("", String::as_str)
    }
}

fn choices(pairs: impl IntoIterator<Item = (String, String)>) -> FieldKind {
    FieldKind::Select(pairs.into_iter().collect())
}

/// The fields of a block of `kind` in `place`; `honors` are the enabled honor groups.
fn fields(
    state: &AppState,
    place: Place,
    kind: Kind,
    honors: &[crate::honors::Honor],
) -> Vec<Field> {
    let t = crate::i18n::current();
    let mut fields = Vec::new();
    let views: Vec<(String, String)> = kind
        .views()
        .iter()
        .filter(|view| place.accepts(kind, view))
        .map(|view| ((*view).to_owned(), view_label(view).to_owned()))
        .collect();
    if views.len() > 1 {
        fields.push(Field {
            name: "view",
            label: t.layout_field_view(),
            hint: "",
            kind: choices(views),
        });
    }
    let boards = || -> Vec<(String, String)> {
        state
            .frame
            .forums
            .iter()
            .map(|forum| (format!("board:{}", forum.fid), forum.name.clone()))
            .collect()
    };
    let limit = |site: bool| {
        let mut options: Vec<(String, String)> = Vec::new();
        if site {
            options.push(("site".into(), t.layout_value_site().into()));
        }
        options
            .extend((1..=crate::data::params::MAX_LIMIT).map(|n| (n.to_string(), n.to_string())));
        Field {
            name: "limit",
            label: t.layout_field_limit(),
            hint: "",
            kind: FieldKind::Select(options),
        }
    };
    let scope = |label: &'static str| Field {
        name: "scope",
        label,
        hint: "",
        kind: choices(
            [
                ("current".to_owned(), t.layout_value_current().to_owned()),
                ("site".to_owned(), t.layout_value_whole_site().to_owned()),
            ]
            .into_iter()
            .chain(boards()),
        ),
    };
    match kind {
        Kind::Source(crate::data::Source::Threads) => {
            fields.push(scope(t.layout_field_scope()));
            fields.push(Field {
                name: "sort",
                label: t.layout_field_sort(),
                hint: "",
                kind: choices(crate::data::params::Sort::ALL.into_iter().map(|sort| {
                    (
                        sort.name().to_owned(),
                        match sort {
                            crate::data::params::Sort::Latest => t.layout_sort_latest(),
                            crate::data::params::Sort::Created => t.layout_sort_created(),
                            crate::data::params::Sort::Hot => t.layout_sort_hot(),
                            crate::data::params::Sort::Featured => t.layout_sort_featured(),
                            crate::data::params::Sort::Pinned => t.layout_sort_pinned(),
                        }
                        .to_owned(),
                    )
                })),
            });
            fields.push(Field {
                name: "days",
                label: t.layout_field_days(),
                hint: t.layout_field_days_hint(),
                kind: choices(
                    std::iter::once(("site".to_owned(), t.layout_value_site().to_owned())).chain(
                        crate::data::params::HOT_DAYS
                            .iter()
                            .map(|days| (days.to_string(), t.layout_value_days(*days))),
                    ),
                ),
            });
            fields.push(Field {
                name: "category",
                label: t.layout_field_category(),
                hint: t.layout_field_category_hint(),
                kind: choices(
                    std::iter::once((String::new(), t.layout_value_any().to_owned())).chain(
                        crate::data::params::KINDS
                            .iter()
                            .enumerate()
                            .map(|(index, kind)| {
                                (
                                    (*kind).to_owned(),
                                    crate::content::category_label(index as i64 + 1).to_owned(),
                                )
                            }),
                    ),
                ),
            });
            fields.push(limit(true));
        }
        Kind::Source(crate::data::Source::Boards) => {
            fields.push(Field {
                name: "parent",
                label: t.layout_field_parent(),
                hint: "",
                kind: choices(
                    std::iter::once(("top".to_owned(), t.layout_value_top().to_owned())).chain(
                        state
                            .frame
                            .forums
                            .iter()
                            .filter(|forum| forum.parent_fid.is_none())
                            .map(|forum| (forum.fid.to_string(), forum.name.clone())),
                    ),
                ),
            });
            fields.push(Field {
                name: "counts",
                label: t.layout_field_counts(),
                hint: "",
                kind: FieldKind::Checkbox,
            });
            fields.push(Field {
                name: "last_post",
                label: t.layout_field_last_post(),
                hint: "",
                kind: FieldKind::Checkbox,
            });
        }
        Kind::Source(crate::data::Source::MembersRecent) => fields.push(limit(false)),
        Kind::Source(crate::data::Source::MembersRank) => {
            fields.push(Field {
                name: "board",
                label: t.layout_field_rank_board(),
                hint: "",
                kind: choices(
                    crate::ranks::Board::ALL
                        .into_iter()
                        .map(|board| (board.slug().to_owned(), board.label().to_owned())),
                ),
            });
            fields.push(Field {
                name: "period",
                label: t.layout_field_period(),
                hint: "",
                kind: choices(
                    [
                        crate::ranks::Period::Month,
                        crate::ranks::Period::Day,
                        crate::ranks::Period::All,
                    ]
                    .into_iter()
                    .map(|period| (period.slug().to_owned(), period.label().to_owned())),
                ),
            });
            fields.push(limit(false));
        }
        Kind::Source(crate::data::Source::MembersHonors) => {
            fields.push(Field {
                name: "honor",
                label: t.layout_field_honor(),
                hint: "",
                kind: choices(
                    std::iter::once((String::new(), t.layout_value_every_honor().to_owned()))
                        .chain(
                            honors
                                .iter()
                                .filter(|honor| honor.enabled)
                                .map(|honor| (honor.id.to_string(), honor.name.clone())),
                        ),
                ),
            });
            fields.push(limit(false));
        }
        Kind::Source(crate::data::Source::MembersVerified) => {
            fields.push(Field {
                name: "kind",
                label: t.layout_field_verified_kind(),
                hint: "",
                kind: choices(
                    std::iter::once((String::new(), t.layout_value_any().to_owned())).chain(
                        crate::directory::Category::ALL.into_iter().map(|category| {
                            (category.slug().to_owned(), category.label().to_owned())
                        }),
                    ),
                ),
            });
            fields.push(limit(false));
        }
        Kind::Source(crate::data::Source::Moderators) => fields.push(Field {
            name: "board",
            label: t.layout_field_board(),
            hint: "",
            kind: choices(
                std::iter::once(("current".to_owned(), t.layout_value_current().to_owned())).chain(
                    state
                        .frame
                        .forums
                        .iter()
                        .map(|forum| (forum.fid.to_string(), forum.name.clone())),
                ),
            ),
        }),
        Kind::Source(crate::data::Source::SiteStats) => fields.push(scope(t.layout_field_scope())),
        Kind::Source(crate::data::Source::SiteLinks) => fields.push(Field {
            name: "links_place",
            label: t.layout_field_links_place(),
            hint: "",
            kind: choices([
                ("home".to_owned(), t.layout_value_links_home().to_owned()),
                (
                    "directory".to_owned(),
                    t.layout_value_links_directory().to_owned(),
                ),
            ]),
        }),
        _ => {}
    }
    if kind.titled() {
        fields.push(Field {
            name: "title_key",
            label: t.layout_field_title(),
            hint: t.layout_field_title_hint(),
            kind: choices(
                std::iter::once((String::new(), t.layout_value_own_title().to_owned())).chain(
                    layout::TITLE_KEYS
                        .iter()
                        .map(|key| ((*key).to_owned(), layout::title_text(key, t).to_owned())),
                ),
            ),
        });
        fields.push(Field {
            name: "title_en",
            label: t.layout_field_title_en(),
            hint: "",
            kind: FieldKind::Text {
                max: layout::TITLE_CHARS,
            },
        });
        fields.push(Field {
            name: "title_zh_cn",
            label: t.layout_field_title_zh(),
            hint: "",
            kind: FieldKind::Text {
                max: layout::TITLE_CHARS,
            },
        });
    }
    if kind == Kind::Core(Core::Markdown) {
        fields.push(Field {
            name: "body_en",
            label: t.layout_field_body_en(),
            hint: t.layout_field_body_hint(),
            kind: FieldKind::Area,
        });
        fields.push(Field {
            name: "body_zh_cn",
            label: t.layout_field_body_zh(),
            hint: "",
            kind: FieldKind::Area,
        });
    }
    fields.push(Field {
        name: "show",
        label: t.layout_field_show(),
        hint: "",
        kind: choices([
            ("all".to_owned(), t.layout_show_all().to_owned()),
            ("guests".to_owned(), t.layout_show_guests().to_owned()),
            ("members".to_owned(), t.layout_show_members().to_owned()),
        ]),
    });
    if place.aside() {
        fields.push(Field {
            name: "keep_on_phone",
            label: t.layout_field_keep(),
            hint: "",
            kind: FieldKind::Checkbox,
        });
    }
    fields
}

async fn add_page(AdminPage(state): AdminPage, Query(query): Query<PageQuery>) -> Response {
    render_add(
        &state,
        &query,
        HashMap::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}

async fn render_add(
    state: &AppState,
    query: &PageQuery,
    values: HashMap<String, String>,
    error: String,
    status: StatusCode,
) -> Response {
    let not_found = || {
        error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        )
    };
    let Some(place) = Place::parse(&query.place) else {
        return not_found();
    };
    let Some(kind) = Kind::parse(&query.source)
        .filter(|kind| kind.views().iter().any(|view| place.accepts(*kind, view)))
    else {
        return not_found();
    };
    let honors = match state.global.honors(&state.db, state.cache_ttl()).await {
        Ok(honors) => honors,
        Err(error) => return database_error(state, error),
    };
    let t = crate::i18n::current();
    let mut response = render(&AddTemplate {
        p: meta(
            state,
            t.layout_add_title().into(),
            t.layout_page_description().into(),
            PATH,
            "",
            "",
        ),
        domain: query.domain,
        place: place.name(),
        place_label: place_label(place).0,
        source: kind.name(),
        source_label: kind_label(kind),
        fields: fields(state, place, kind, &honors),
        values,
        error,
    });
    *response.status_mut() = status;
    response
}

/// The block the add form describes, as layout JSON: parameters from their fields, `""` and unticked boxes left out.
fn block_json(kind: Kind, id: &str, form: &HashMap<String, String>) -> Value {
    let get = |name: &str| form.get(name).map(String::as_str).unwrap_or("").trim();
    let mut block = Map::new();
    block.insert("id".into(), json!(id));
    block.insert("source".into(), json!(kind.name()));
    if !get("view").is_empty() {
        block.insert("view".into(), json!(get("view")));
    }
    if let Kind::Source(source) = kind {
        let number = |text: &str| {
            text.parse::<i64>()
                .map(Value::from)
                .unwrap_or_else(|_| json!(text))
        };
        let mut params = Map::new();
        let names: &[&str] = match source {
            crate::data::Source::Threads => &["scope", "sort", "days", "category", "limit"],
            crate::data::Source::Boards => &["parent", "counts", "last_post"],
            crate::data::Source::MembersRecent => &["limit"],
            crate::data::Source::MembersRank => &["board", "period", "limit"],
            crate::data::Source::MembersHonors => &["honor", "limit"],
            crate::data::Source::MembersVerified => &["kind", "limit"],
            crate::data::Source::Moderators => &["board"],
            crate::data::Source::SiteStats => &["scope"],
            crate::data::Source::SiteLinks => &["links_place"],
            _ => &[],
        };
        let sort = get("sort");
        for name in names {
            let value = get(name);
            // Days only apply to hot threads and a category to the two time sorts; the form shows both always.
            if (*name == "days" && sort != "hot")
                || (*name == "category" && !matches!(sort, "latest" | "created"))
            {
                continue;
            }
            match *name {
                "counts" | "last_post" => {
                    params.insert((*name).into(), json!(value == "1"));
                }
                _ if value.is_empty() => {}
                "links_place" => {
                    params.insert("place".into(), json!(value));
                }
                "days" | "limit" | "honor" | "parent" | "board"
                    if value.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    params.insert((*name).into(), number(value));
                }
                _ => {
                    params.insert((*name).into(), json!(value));
                }
            }
        }
        if kind == Kind::Source(crate::data::Source::ThreadOutline) {
            params.insert("tid".into(), json!("current"));
        }
        block.insert("params".into(), Value::Object(params));
    }
    if kind.titled() {
        let key = get("title_key");
        if !key.is_empty() {
            block.insert("title".into(), json!({"key": key}));
        } else if !get("title_en").is_empty() || !get("title_zh_cn").is_empty() {
            let mut text = Map::new();
            for (field, tag) in [("title_en", "en"), ("title_zh_cn", "zh-CN")] {
                if !get(field).is_empty() {
                    text.insert(tag.into(), json!(get(field)));
                }
            }
            block.insert("title".into(), json!({"text": text}));
        }
    }
    if kind == Kind::Core(Core::Markdown) {
        let mut body = Map::new();
        for (field, tag) in [("body_en", "en"), ("body_zh_cn", "zh-CN")] {
            if !get(field).is_empty() {
                body.insert(tag.into(), json!(get(field)));
            }
        }
        block.insert("body".into(), Value::Object(body));
    }
    if !get("show").is_empty() && get("show") != "all" {
        block.insert("show".into(), json!(get("show")));
    }
    if get("keep_on_phone") == "1" {
        block.insert("keep_on_phone".into(), json!(true));
    }
    Value::Object(block)
}

/// A fresh block id: the source's name with hyphens and the first free number.
fn fresh_id(kind: Kind, places: &Places, below: &[Block]) -> String {
    let base = kind.name().replace('.', "-");
    let taken: Vec<&str> = places
        .0
        .iter()
        .flat_map(|(_, blocks)| blocks)
        .chain(below)
        .map(|block| block.id.as_str())
        .collect();
    (1..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(&id.as_str()))
        .unwrap_or(base)
}

async fn add(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let get = |name: &str| form.get(name).cloned().unwrap_or_default();
    let query = PageQuery {
        domain: get("domain").parse().unwrap_or(0),
        place: get("place"),
        source: get("source"),
        saved: String::new(),
    };
    let (Some(place), Some(kind)) = (Place::parse(&query.place), Kind::parse(&query.source)) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    let domain = target(query.domain);
    let working = match working(&state, domain).await {
        Ok(working) => working,
        Err(error) => return database_error(&state, error),
    };
    let below = working.below.get(place).unwrap_or(&[]).to_vec();
    let id = fresh_id(kind, &working.places, &below);
    let path = format!("places.{}[new]", place.name());
    let block = match Block::parse(&block_json(kind, &id, &form), place, &path) {
        Ok(block) => block,
        Err(problem) => {
            let message = problem.message(crate::i18n::current());
            return render_add(
                &state,
                &query,
                form,
                message,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    let mut places = working.places;
    let mut blocks = places.get(place).map_or(below, <[Block]>::to_vec);
    blocks.push(block);
    places.set(place, Some(blocks));
    if let Err(problem) = Places::parse(&places.json()) {
        let message = problem.message(crate::i18n::current());
        return render_add(
            &state,
            &query,
            form,
            message,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    save(&state, domain, places, working.home).await
}
