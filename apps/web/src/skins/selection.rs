//! Which skin a site uses (docs/skins.md, "Choosing a skin"): the layout document `settings.layout.{body,title,
//! updated_at}` for the whole site and one override document per domain, `settings.domain_layout.<domain id>.…`,
//! stored like the navigation document and read by the page frame in the same single statement as the settings.
//!
//! The body is JSON: `{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.2.0"},
//! "places":null}`, with `"colors":{"primary":"#rrggbb","secondary":"#rrggbb"}` when the administrator chose seed
//! colors (docs/styles.md, "Seed colors"). `places` holds the blocks of each place the document changes
//! (docs/skins.md, "Places and blocks"), `null` while it changes none. A domain document leaves out what it inherits
//! from the site's, place by place; its `colors` may also be `"skin"`, the skin's own colors even where the site has
//! seeds.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::seed::Seeds;

use crate::cache::{Block, GlobalCache};
use crate::db::{self, Database, Tx};
use crate::identity::{Actor, IdentityError};

pub const LAYOUT_PREFIX: &str = "settings.layout.";
pub const DOMAIN_LAYOUT_PREFIX: &str = "settings.domain_layout.";

/// The key prefix of one domain's override document.
pub fn domain_prefix(domain_id: i64) -> String {
    format!("{DOMAIN_LAYOUT_PREFIX}{domain_id}.")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub v: u32,
    #[serde(default)]
    pub skin: Option<String>,
    #[serde(default)]
    pub palette: Option<String>,
    /// The skin and version this choice was made from.
    #[serde(default)]
    pub based_on: Option<BasedOn>,
    /// Blocks per place this document sets (`crate::layout::Places` as JSON); `null` when it sets none.
    #[serde(default)]
    pub places: serde_json::Value,
    /// Seed colors over the skin's palettes; left out when the document inherits them.
    #[serde(default, skip_serializing_if = "Colors::inherits")]
    pub colors: Colors,
    /// What the home page opens with (docs/skins.md, "Board directory home"); left out when the document inherits
    /// it (the site's for a domain, the skin's for the site).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<super::manifest::HomeView>,
}

/// The colors a document chooses: inherit (the key is absent: the site's for a domain, the skin's own for the site),
/// the skin's own colors (`"skin"`, only written in domain documents) or seed colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colors {
    #[default]
    Inherit,
    Skin,
    Seeds(Seeds),
}

impl Colors {
    pub fn inherits(&self) -> bool {
        *self == Self::Inherit
    }
}

impl Serialize for Colors {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Never written: the field is skipped.
            Self::Inherit => serializer.serialize_none(),
            Self::Skin => serializer.serialize_str("skin"),
            Self::Seeds(seeds) => seeds.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Colors {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Word(String),
            Seeds(Seeds),
        }
        match Written::deserialize(deserializer)? {
            Written::Word(word) if word == "skin" => Ok(Self::Skin),
            Written::Word(word) => Err(serde::de::Error::custom(format!(
                "unknown colors \"{word}\""
            ))),
            Written::Seeds(seeds) => Ok(Self::Seeds(seeds)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasedOn {
    pub skin: String,
    pub version: String,
}

impl Layout {
    /// An empty body is no document. Anything else must be a version 1 document.
    pub fn parse(body: &str) -> Result<Option<Self>, String> {
        if body.trim().is_empty() {
            return Ok(None);
        }
        let layout: Self = serde_json::from_str(body).map_err(|error| error.to_string())?;
        if layout.v != 1 {
            return Err(format!("unknown layout document version {}", layout.v));
        }
        if !(layout.places.is_null() || layout.places.is_object()) {
            return Err("places must be null or an object".into());
        }
        Ok(Some(layout))
    }

    /// The places the document sets, read leniently (docs/skins.md, "Places and blocks"): blocks that no longer
    /// pass their checks are skipped and logged; `None` when it sets none.
    pub fn places(&self, owner: &str) -> Option<crate::layout::Places> {
        if self.places.is_null() {
            return None;
        }
        let places = crate::layout::Places::read(&self.places, owner);
        (!places.is_empty()).then_some(places)
    }

    pub fn body(&self) -> String {
        serde_json::to_string(self).expect("a layout document always serializes")
    }
}

/// The skin and palette a page uses: the domain's override, then the site's choice, then the default skin.
pub fn choose<'a>(
    site: Option<&'a Layout>,
    domain: Option<&'a Layout>,
) -> (&'a str, Option<&'a str>) {
    for layout in [domain, site].into_iter().flatten() {
        if let Some(skin) = &layout.skin {
            return (skin, layout.palette.as_deref());
        }
    }
    (super::DEFAULT_ID, None)
}

/// The seed colors a page uses: the domain's choice unless it inherits, then the site's; `None` for the skin's own
/// colors. They apply over whichever skin the page uses.
pub fn colors(site: Option<&Layout>, domain: Option<&Layout>) -> Option<Seeds> {
    for layout in [domain, site].into_iter().flatten() {
        match layout.colors {
            Colors::Inherit => {}
            Colors::Skin => return None,
            Colors::Seeds(seeds) => return Some(seeds),
        }
    }
    None
}

/// Every skin and seed colors a page of the site uses: the site's and each domain's. The stylesheets of these are
/// the ones shared caches may keep.
pub fn in_use(site: Option<&Layout>, domains: &[(i64, Layout)]) -> Vec<(String, Option<Seeds>)> {
    let mut found = vec![(choose(site, None).0.to_owned(), colors(site, None))];
    for (_, domain) in domains {
        let used = (
            choose(site, Some(domain)).0.to_owned(),
            colors(site, Some(domain)),
        );
        if !found.contains(&used) {
            found.push(used);
        }
    }
    found
}

/// The site's document and every domain's override, from the rows the page frame read.
pub fn from_rows(rows: &crate::runtime::kv::Rows) -> (Option<Layout>, Vec<(i64, Layout)>) {
    let site = rows
        .get(&format!("{LAYOUT_PREFIX}body"))
        .and_then(|body| match Layout::parse(body) {
            Ok(layout) => layout,
            Err(error) => {
                tracing::error!(%error, "the site's layout document is invalid; the default skin is used; choose a skin again at /manage/skins");
                None
            }
        });
    let mut domains = Vec::new();
    for (key, body) in rows.with_prefix(DOMAIN_LAYOUT_PREFIX) {
        let Some(id) = key
            .strip_prefix(DOMAIN_LAYOUT_PREFIX)
            .and_then(|rest| rest.strip_suffix(".body"))
            .and_then(|id| id.parse::<i64>().ok())
        else {
            continue;
        };
        match Layout::parse(body) {
            Ok(Some(layout)) => domains.push((id, layout)),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(domain_id = id, %error, "a domain's layout document is invalid; the site's skin is used there");
            }
        }
    }
    (site, domains)
}

/// What a selection changes: the whole site or one domain, and the skin (`None` for a domain that follows the site
/// again).
pub struct Selection<'a> {
    pub domain_id: Option<i64>,
    pub skin: Option<&'a super::Skin>,
    pub palette: Option<&'a str>,
}

/// Saves a selection ([`edit`]): the skin and palette, and the skin and version they were chosen from; for a domain
/// that follows the site again (`skin: None`), its own skin is removed. Records `skin.select`.
pub async fn save(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    selection: Selection<'_>,
) -> Result<(), IdentityError> {
    let imported = selection
        .skin
        .filter(|skin| skin.source == super::Source::Imported)
        .map(super::Skin::id);
    edit(
        db,
        global,
        actor,
        selection.domain_id,
        imported,
        "skin.select",
        |before| {
            let previous = before.as_ref().and_then(|layout| layout.skin.clone());
            let mut layout = before.unwrap_or(Layout {
                v: 1,
                ..Layout::default()
            });
            match selection.skin {
                Some(skin) => {
                    layout.skin = Some(skin.id().to_owned());
                    layout.palette = Some(skin.palette(selection.palette).to_owned());
                    layout.based_on = Some(BasedOn {
                        skin: skin.id().to_owned(),
                        version: skin.manifest.version.clone(),
                    });
                }
                None => {
                    layout.skin = None;
                    layout.palette = None;
                    layout.based_on = None;
                }
            }
            let data = serde_json::json!({
                "domain_id": selection.domain_id,
                "skin": selection.skin.map(super::Skin::id),
                "palette": selection.skin.map(|skin| skin.palette(selection.palette)),
                "previous": previous,
            });
            (layout, data)
        },
    )
    .await
}

/// The audit form of a color choice: the seeds, `"skin"` or `null` (inherit).
fn colors_json(colors: Colors) -> serde_json::Value {
    serde_json::to_value(colors).unwrap_or_default()
}

/// Saves the seed colors of the site or a domain ([`edit`]); the skin and palette stay as they are. On the site's
/// document the skin's own colors are the absent key. Records `skin.colors`.
pub async fn save_colors(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: Option<i64>,
    colors: Colors,
) -> Result<(), IdentityError> {
    let colors = match (domain_id, colors) {
        (None, Colors::Skin) => Colors::Inherit,
        (_, colors) => colors,
    };
    edit(
        db,
        global,
        actor,
        domain_id,
        None,
        "skin.colors",
        |before| {
            let previous = before
                .as_ref()
                .map(|layout| layout.colors)
                .unwrap_or_default();
            let mut layout = before.unwrap_or(Layout {
                v: 1,
                ..Layout::default()
            });
            layout.colors = colors;
            let data = serde_json::json!({
                "domain_id": domain_id,
                "colors": colors_json(colors),
                "previous": colors_json(previous),
            });
            (layout, data)
        },
    )
    .await
}

/// Changes a layout document in its own transaction ([`edit_in`]) and clears the settings block after commit.
async fn edit(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: Option<i64>,
    imported: Option<&str>,
    kind: &str,
    change: impl FnOnce(Option<Layout>) -> (Layout, serde_json::Value),
) -> Result<(), IdentityError> {
    let mut tx = db.begin_write().await?;
    edit_in(db, &mut tx, actor, domain_id, imported, kind, change).await?;
    tx.commit().await?;
    global
        .announce(&[Block::Settings])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(())
}

/// The key prefix of the site's document or a domain's.
fn document_prefix(domain_id: Option<i64>) -> String {
    match domain_id {
        None => LAYOUT_PREFIX.to_owned(),
        Some(id) => domain_prefix(id),
    }
}

/// Changes a layout document in `tx`: locks its rows, checks the administrator, that the domain exists and, for an
/// imported skin, that its row still exists (a delete takes it under a lock); then writes what `change` makes of the
/// stored document and records `kind` with the data `change` returns. A document left with nothing of its own is
/// emptied (the site's rows are seeded and stay) or, for a domain, deleted, so the domain follows the site again. The
/// caller commits and clears the settings block.
async fn edit_in(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    domain_id: Option<i64>,
    imported: Option<&str>,
    kind: &str,
    change: impl FnOnce(Option<Layout>) -> (Layout, serde_json::Value),
) -> Result<(), IdentityError> {
    let prefix = document_prefix(domain_id);
    let keys = ["body", "title", "updated_at"].map(|field| format!("{prefix}{field}"));
    let rows = db::query(crate::runtime::DOCUMENT_LOCK_SQL)
        .bind(&keys[0])
        .bind(&keys[1])
        .bind(&keys[2])
        .fetch_all(db.observe(&mut **tx))
        .await?;
    crate::runtime::admin_locked(db, tx, actor).await?;
    if let Some(id) = domain_id {
        domain_exists(db, tx, id).await?;
    }
    if let Some(id) = imported {
        let found =
            db::query("SELECT skin_id FROM gonof_common_skin WHERE skin_id=? LOCK IN SHARE MODE")
                .bind(id)
                .fetch_optional(db.observe(&mut **tx))
                .await?;
        if found.is_none() {
            return Err(IdentityError::Validation(
                crate::i18n::current().skin_error_gone(),
            ));
        }
    }
    let mut before = None;
    let mut title = String::new();
    for row in &rows {
        let key = db::text(row, "skey")?;
        if key == keys[0] {
            before = Layout::parse(&db::text(row, "svalue")?).unwrap_or_default();
        } else if key == keys[1] {
            title = db::text(row, "svalue")?;
        }
    }
    let (layout, data) = change(before);
    let empty = layout.skin.is_none()
        && layout.colors.inherits()
        && layout.places.is_null()
        && layout.home.is_none();
    match (empty, domain_id) {
        (true, Some(_)) => delete_rows(db, tx, &prefix).await?,
        (true, None) => {
            crate::runtime::write_document(db, tx, &prefix, "", &title, crate::identity::now())
                .await?;
        }
        (false, _) => {
            crate::runtime::write_document(
                db,
                tx,
                &prefix,
                &layout.body(),
                &title,
                crate::identity::now(),
            )
            .await?;
        }
    }
    crate::runtime::audit(db, tx, actor, kind, data).await?;
    Ok(())
}

/// Checks, with a shared lock, that the domain still exists.
async fn domain_exists(db: &Database, tx: &mut Tx<'_>, id: i64) -> Result<(), IdentityError> {
    let found = db::query(
        "SELECT domain_id FROM gonof_common_site_domain WHERE domain_id=? LOCK IN SHARE MODE",
    )
    .bind(id)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    match found {
        Some(_) => Ok(()),
        None => Err(IdentityError::NotFound),
    }
}

/// Deletes the three rows of a document or draft.
async fn delete_rows(db: &Database, tx: &mut Tx<'_>, prefix: &str) -> Result<(), IdentityError> {
    db::query("DELETE FROM gonof_common_settings WHERE skey IN (?,?,?)")
        .bind(format!("{prefix}body"))
        .bind(format!("{prefix}title"))
        .bind(format!("{prefix}updated_at"))
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// A layout draft (docs/skins.md, "Editing a layout"): the places it sets and its choice of home page; what it
/// leaves out comes from the next layout down, as in a document. The layout editor works on the draft when there is
/// one, else on the published places and home page, in the same form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub places: crate::layout::Places,
    pub home: Option<super::manifest::HomeView>,
}

impl Draft {
    /// The places and home page of a stored document body, read leniently; `None` when it sets neither.
    pub fn of_document(body: &str) -> Option<Self> {
        let layout = Layout::parse(body).ok().flatten()?;
        let places = layout.places("the layout editor");
        (places.is_some() || layout.home.is_some()).then(|| Self {
            places: places.unwrap_or_default(),
            home: layout.home,
        })
    }

    /// A stored draft body, read leniently; `None` when it is no document.
    fn of_draft(body: &str) -> Option<Self> {
        Layout::parse(body).ok().flatten().map(|layout| Self {
            places: crate::layout::Places::read(&layout.places, "a layout draft"),
            home: layout.home,
        })
    }

    /// The revision token of a working layout: the first 16 hex digits of the SHA-256 of its places and home page as
    /// a document stores them. Every form of the layout editor carries the token of the layout it showed, and a
    /// change to a layout with another token is refused (docs/skins.md, "Editing a layout").
    pub fn revision(&self) -> String {
        use sha2::{Digest, Sha256};
        let text = serde_json::json!({
            "places": self.places.json(),
            "home": self.home.map(super::manifest::HomeView::name),
        })
        .to_string();
        Sha256::digest(text.as_bytes())
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// What [`save_places`] and [`publish_draft`] make of the document: the places and home page of `draft` (no place,
/// or `None`, changes none, so every place comes from the next layout again, and `home: None` inherits it), with the
/// `layout.save` data of the places and home page before and after.
fn places_change(
    domain_id: Option<i64>,
    draft: Option<&Draft>,
) -> impl FnOnce(Option<Layout>) -> (Layout, serde_json::Value) {
    let places = draft.map(|draft| &draft.places);
    let home = draft.and_then(|draft| draft.home);
    let value = places
        .filter(|places| !places.is_empty())
        .map_or(serde_json::Value::Null, crate::layout::Places::json);
    let names = |value: &serde_json::Value| -> Vec<String> {
        value
            .as_object()
            .map(|places| places.keys().cloned().collect())
            .unwrap_or_default()
    };
    move |before| {
        let mut layout = before.unwrap_or(Layout {
            v: 1,
            ..Layout::default()
        });
        let data = serde_json::json!({
            "domain_id": domain_id,
            "places": names(&value),
            "previous": names(&layout.places),
            "blocks": value
                .as_object()
                .map_or(0, |places| places.values().filter_map(|blocks| blocks.as_array()).map(Vec::len).sum::<usize>()),
            "home": home.map(super::manifest::HomeView::name),
            "previous_home": layout.home.map(super::manifest::HomeView::name),
        });
        layout.places = value;
        layout.home = home;
        (layout, data)
    }
}

/// Saves the places and the home page of the site's document or a domain's ([`edit`]): no place (or `None`)
/// changes none, so every place comes from the next layout again, and `home: None` inherits it. The skin, palette and
/// colors stay as they are. Records `layout.save` with the places and home page before and after.
pub async fn save_places(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: Option<i64>,
    draft: Option<&Draft>,
) -> Result<(), IdentityError> {
    edit(
        db,
        global,
        actor,
        domain_id,
        None,
        "layout.save",
        places_change(domain_id, draft),
    )
    .await
}

/// What publishing a layout draft came to.
#[derive(Debug)]
pub enum Published {
    /// The draft's places and home page are the document's, and the draft is gone.
    Done,
    /// There is no draft: it was published or discarded meanwhile.
    NoDraft,
    /// The draft no longer passes the strict checks of a saved layout; nothing changed.
    Refused(crate::layout::LayoutError),
}

/// Publishes the draft of the site's or a domain's places and home page in one transaction (docs/skins.md, "Editing
/// a layout"): locks the draft's rows and reads it, checks it strictly and, with `expected`, that it is still the
/// revision the administrator saw ([`Draft::revision`], `Conflict` otherwise); then writes it into the document under
/// [`edit_in`]'s locks and checks, records `layout.save` and deletes the draft. A draft saved meanwhile waits for the
/// lock, so it is neither published unseen nor deleted. The preview bar publishes with `expected: None`, the draft
/// as it is.
pub async fn publish_draft(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: Option<i64>,
    expected: Option<&str>,
) -> Result<Published, IdentityError> {
    let prefix = draft_prefix(domain_id);
    let mut tx = db.begin_write().await?;
    let body = locked_draft(db, &mut tx, &prefix).await?;
    let Some(draft) = body.as_deref().and_then(Draft::of_draft) else {
        return Ok(Published::NoDraft);
    };
    if expected.is_some_and(|expected| expected != draft.revision()) {
        return Err(IdentityError::Conflict(
            crate::i18n::current().layout_draft_changed(),
        ));
    }
    if let Err(problem) = crate::layout::Places::parse(&draft.places.json()) {
        return Ok(Published::Refused(problem));
    }
    edit_in(
        db,
        &mut tx,
        actor,
        domain_id,
        None,
        "layout.save",
        places_change(domain_id, Some(&draft)),
    )
    .await?;
    delete_rows(db, &mut tx, &prefix).await?;
    tx.commit().await?;
    global
        .announce(&[Block::Settings])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(Published::Done)
}

pub const DRAFT_PREFIX: &str = "settings.layout_draft.";

/// The key prefix of the layout draft of the site (`settings.layout_draft.site.`) or of a domain
/// (`settings.layout_draft.<domain id>.`): rows created by the first change in the layout editor and deleted when the
/// draft is published or discarded (docs/skins.md, "Editing a layout").
pub fn draft_prefix(domain_id: Option<i64>) -> String {
    match domain_id {
        None => format!("{DRAFT_PREFIX}site."),
        Some(id) => format!("{DRAFT_PREFIX}{id}."),
    }
}

/// Locks the three rows of the draft at `prefix` and returns its body, `None` without one. Every write of a draft
/// takes this lock first, then the document's rows, then the administrator's row.
async fn locked_draft(
    db: &Database,
    tx: &mut Tx<'_>,
    prefix: &str,
) -> Result<Option<String>, IdentityError> {
    let body = format!("{prefix}body");
    let rows = db::query(crate::runtime::DOCUMENT_LOCK_SQL)
        .bind(&body)
        .bind(format!("{prefix}title"))
        .bind(format!("{prefix}updated_at"))
        .fetch_all(db.observe(&mut **tx))
        .await?;
    for row in &rows {
        if db::text(row, "skey")? == body {
            return Ok(Some(db::text(row, "svalue")?));
        }
    }
    Ok(None)
}

/// The draft of the site's or a domain's places and home page, read leniently; `None` without one.
pub async fn read_draft(db: &Database, domain_id: Option<i64>) -> anyhow::Result<Option<Draft>> {
    let body: Option<String> =
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(format!("{}body", draft_prefix(domain_id)))
            .fetch_optional(db.executor())
            .await?;
    Ok(body.as_deref().and_then(Draft::of_draft))
}

/// Writes the draft of the site's or a domain's places and home page, whose places the caller checked strictly
/// (`layout::Places::parse`); `None` deletes it. `expected` is the revision of the layout the administrator changed
/// ([`Draft::revision`]): it is compared, under the draft's lock, with the working layout (the draft, else the
/// published places and home page, read with a shared lock), and a change to another layout is refused with
/// `Conflict`, so an editor page left open cannot change or discard a layout it did not show. Drafts are not seen by
/// visitors, so writing one records nothing in the action log; publishing does (`layout.save`).
pub async fn write_draft(
    db: &Database,
    actor: Actor,
    domain_id: Option<i64>,
    draft: Option<&Draft>,
    expected: &str,
) -> Result<(), IdentityError> {
    let prefix = draft_prefix(domain_id);
    let mut tx = db.begin_write().await?;
    let body = locked_draft(db, &mut tx, &prefix).await?;
    let working = match body.as_deref().and_then(Draft::of_draft) {
        Some(working) => working,
        None => {
            let document: Option<String> = db::query_scalar(
                "SELECT svalue FROM gonof_common_settings WHERE skey=? LOCK IN SHARE MODE",
            )
            .bind(format!("{}body", document_prefix(domain_id)))
            .fetch_optional(db.observe(&mut *tx))
            .await?;
            document
                .as_deref()
                .and_then(Draft::of_document)
                .unwrap_or_default()
        }
    };
    if working.revision() != expected {
        return Err(IdentityError::Conflict(
            crate::i18n::current().layout_draft_changed(),
        ));
    }
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    if let Some(id) = domain_id {
        domain_exists(db, &mut tx, id).await?;
    }
    match draft {
        Some(draft) => {
            let mut body = serde_json::json!({"v": 1, "places": draft.places.json()});
            if let Some(home) = draft.home {
                body["home"] = serde_json::json!(home.name());
            }
            let body = body.to_string();
            crate::runtime::write_document(db, &mut tx, &prefix, &body, "", crate::identity::now())
                .await?;
        }
        None => delete_rows(db, &mut tx, &prefix).await?,
    }
    tx.commit().await?;
    Ok(())
}

/// Deletes a domain's override document, in the transaction that deletes the domain.
pub async fn delete_domain(db: &Database, tx: &mut Tx<'_>, domain_id: i64) -> anyhow::Result<()> {
    // The domain's layout draft goes with its document.
    for prefix in [domain_prefix(domain_id), draft_prefix(Some(domain_id))] {
        db::query("DELETE FROM gonof_common_settings WHERE skey IN (?,?,?)")
            .bind(format!("{prefix}body"))
            .bind(format!("{prefix}title"))
            .bind(format!("{prefix}updated_at"))
            .execute(db.observe(&mut **tx))
            .await?;
    }
    Ok(())
}

/// Where a skin id is chosen: the site's document and the domains whose override names it. Read with shared locks,
/// so a delete that checks this waits for a selection being saved.
pub async fn uses(db: &Database, tx: &mut Tx<'_>, id: &str) -> anyhow::Result<(bool, Vec<i64>)> {
    let rows = db::query(
        "SELECT skey,svalue FROM gonof_common_settings WHERE skey=? OR (skey>=? AND skey<?) LOCK IN SHARE MODE",
    )
    .bind(format!("{LAYOUT_PREFIX}body"))
    .bind(DOMAIN_LAYOUT_PREFIX)
    .bind(crate::runtime::kv::range(DOMAIN_LAYOUT_PREFIX).1)
    .fetch_all(db.observe(&mut **tx))
    .await?;
    let mut site = false;
    let mut domains = Vec::new();
    for row in rows {
        let key = db::text(&row, "skey")?;
        let Ok(Some(layout)) = Layout::parse(&db::text(&row, "svalue")?) else {
            continue;
        };
        if layout.skin.as_deref() != Some(id) {
            continue;
        }
        if key == format!("{LAYOUT_PREFIX}body") {
            site = true;
        } else if let Some(domain) = key
            .strip_prefix(DOMAIN_LAYOUT_PREFIX)
            .and_then(|rest| rest.strip_suffix(".body"))
            .and_then(|id| id.parse::<i64>().ok())
        {
            domains.push(domain);
        }
    }
    Ok((site, domains))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_parse_strictly() {
        assert_eq!(Layout::parse("  "), Ok(None));
        let layout = Layout::parse(
            r#"{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.2.0"},"places":null}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(layout.skin.as_deref(), Some("ledger"));
        assert_eq!(
            layout.body(),
            r#"{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.2.0"},"places":null}"#
        );
        assert!(Layout::parse(r#"{"v":2}"#).is_err());
        assert!(Layout::parse(r#"{"v":1,"places":[]}"#).is_err());
        assert!(
            Layout::parse(r#"{"v":1,"places":{}}"#)
                .unwrap()
                .unwrap()
                .places("test")
                .is_none()
        );
        assert!(Layout::parse(r#"{"v":1,"colour":"red"}"#).is_err());
        assert!(Layout::parse("not json").is_err());
    }

    #[test]
    fn colors_are_written_only_when_chosen() {
        let layout = Layout::parse(
            r##"{"v":1,"skin":"ledger","colors":{"primary":"#4F46E5","secondary":"#57637a"},"places":null}"##,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            layout.colors,
            Colors::Seeds(Seeds::parse("#4f46e5", "#57637a").unwrap())
        );
        assert_eq!(
            layout.body(),
            r##"{"v":1,"skin":"ledger","palette":null,"based_on":null,"places":null,"colors":{"primary":"#4f46e5","secondary":"#57637a"}}"##
        );
        let skin = Layout::parse(r#"{"v":1,"colors":"skin"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(skin.colors, Colors::Skin);
        assert!(skin.body().contains(r#""colors":"skin""#));
        let plain = Layout::parse(r#"{"v":1,"skin":"ledger"}"#)
            .unwrap()
            .unwrap();
        assert!(!plain.body().contains("colors"));
        for bad in [
            r#"{"v":1,"colors":"site"}"#,
            r##"{"v":1,"colors":{"primary":"red","secondary":"#000000"}}"##,
            r##"{"v":1,"colors":{"primary":"#000000"}}"##,
            r#"{"v":1,"colors":null}"#,
        ] {
            assert!(Layout::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_domain_inherits_the_site_colors_or_chooses_its_own() {
        let seeds = Seeds::parse("#0f766e", "#f97316").unwrap();
        let other = Seeds::parse("#e11d48", "#7c3aed").unwrap();
        let site = Layout {
            v: 1,
            skin: Some("ledger".into()),
            colors: Colors::Seeds(seeds),
            ..Layout::default()
        };
        let follow = Layout {
            v: 1,
            skin: Some("harbor".into()),
            ..Layout::default()
        };
        let own_skin_colors = Layout {
            v: 1,
            colors: Colors::Skin,
            ..Layout::default()
        };
        let own = Layout {
            v: 1,
            colors: Colors::Seeds(other),
            ..Layout::default()
        };
        assert_eq!(colors(None, None), None);
        assert_eq!(colors(Some(&site), None), Some(seeds));
        assert_eq!(colors(Some(&site), Some(&follow)), Some(seeds));
        assert_eq!(colors(Some(&site), Some(&own_skin_colors)), None);
        assert_eq!(colors(Some(&site), Some(&own)), Some(other));
        assert_eq!(colors(None, Some(&own)), Some(other));
        // A domain's own colors leave its skin to the site.
        assert_eq!(choose(Some(&site), Some(&own)).0, "ledger");
        assert_eq!(
            in_use(
                Some(&site),
                &[
                    (2, follow.clone()),
                    (3, own.clone()),
                    (4, own_skin_colors.clone())
                ]
            ),
            [
                ("ledger".to_owned(), Some(seeds)),
                ("harbor".to_owned(), Some(seeds)),
                ("ledger".to_owned(), Some(other)),
                ("ledger".to_owned(), None),
            ]
        );
    }

    #[test]
    fn a_domain_override_wins_and_unset_keys_inherit() {
        let site = Layout {
            v: 1,
            skin: Some("ledger".into()),
            palette: Some("night".into()),
            ..Layout::default()
        };
        let follow = Layout {
            v: 1,
            ..Layout::default()
        };
        let own = Layout {
            v: 1,
            skin: Some("harbor".into()),
            ..Layout::default()
        };
        assert_eq!(choose(None, None), ("default", None));
        assert_eq!(choose(Some(&site), None), ("ledger", Some("night")));
        assert_eq!(
            choose(Some(&site), Some(&follow)),
            ("ledger", Some("night"))
        );
        assert_eq!(choose(Some(&site), Some(&own)), ("harbor", None));
        assert_eq!(choose(None, Some(&own)), ("harbor", None));
    }
}
