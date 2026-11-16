//! Which skin a site uses (docs/skins.md, "Choosing a skin"): the layout document `settings.layout.{body,title,
//! updated_at}` for the whole site and one override document per domain, `settings.domain_layout.<domain id>.…`,
//! stored like the navigation document and read by the page frame in the same single statement as the settings.
//!
//! The body is JSON: `{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.2.0"},
//! "places":null}`, with `"colors":{"primary":"#rrggbb","secondary":"#rrggbb"}` when the administrator chose seed
//! colors (docs/styles.md, "Seed colors"). `places` (the blocks of each place on a page) stays `null` until layouts
//! arrive. A domain document leaves out what it inherits from the site's; its `colors` may also be `"skin"`, the
//! skin's own colors even where the site has seeds.
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
    /// Blocks per place; `null` until layouts are edited (phase 2).
    #[serde(default)]
    pub places: serde_json::Value,
    /// Seed colors over the skin's palettes; left out when the document inherits them.
    #[serde(default, skip_serializing_if = "Colors::inherits")]
    pub colors: Colors,
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
        if !layout.places.is_null() {
            return Err("places are not supported yet".into());
        }
        Ok(Some(layout))
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

/// Changes a layout document: locks its rows, checks the administrator, that the domain exists and, for an imported
/// skin, that its row still exists (a delete takes it under a lock); then writes what `change` makes of the stored
/// document, records `kind` with the data `change` returns and clears the settings block after commit. A document
/// left with nothing of its own is emptied (the site's rows are seeded and stay) or, for a domain, deleted, so the
/// domain follows the site again.
async fn edit(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: Option<i64>,
    imported: Option<&str>,
    kind: &str,
    change: impl FnOnce(Option<Layout>) -> (Layout, serde_json::Value),
) -> Result<(), IdentityError> {
    let prefix = match domain_id {
        None => LAYOUT_PREFIX.to_owned(),
        Some(id) => domain_prefix(id),
    };
    let keys = ["body", "title", "updated_at"].map(|field| format!("{prefix}{field}"));
    let mut tx = db.begin_write().await?;
    let rows = db::query(crate::runtime::DOCUMENT_LOCK_SQL)
        .bind(&keys[0])
        .bind(&keys[1])
        .bind(&keys[2])
        .fetch_all(db.observe(&mut *tx))
        .await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    if let Some(id) = domain_id {
        let found = db::query(
            "SELECT domain_id FROM gonof_common_site_domain WHERE domain_id=? LOCK IN SHARE MODE",
        )
        .bind(id)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
        if found.is_none() {
            return Err(IdentityError::NotFound);
        }
    }
    if let Some(id) = imported {
        let found =
            db::query("SELECT skin_id FROM gonof_common_skin WHERE skin_id=? LOCK IN SHARE MODE")
                .bind(id)
                .fetch_optional(db.observe(&mut *tx))
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
    let empty = layout.skin.is_none() && layout.colors.inherits() && layout.places.is_null();
    match (empty, domain_id) {
        (true, Some(_)) => {
            db::query("DELETE FROM gonof_common_settings WHERE skey IN (?,?,?)")
                .bind(&keys[0])
                .bind(&keys[1])
                .bind(&keys[2])
                .execute(db.observe(&mut *tx))
                .await?;
        }
        (true, None) => {
            crate::runtime::write_document(
                db,
                &mut tx,
                &prefix,
                "",
                &title,
                crate::identity::now(),
            )
            .await?;
        }
        (false, _) => {
            crate::runtime::write_document(
                db,
                &mut tx,
                &prefix,
                &layout.body(),
                &title,
                crate::identity::now(),
            )
            .await?;
        }
    }
    crate::runtime::audit(db, &mut tx, actor, kind, data).await?;
    tx.commit().await?;
    global
        .announce(&[Block::Settings])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(())
}

/// Deletes a domain's override document, in the transaction that deletes the domain.
pub async fn delete_domain(db: &Database, tx: &mut Tx<'_>, domain_id: i64) -> anyhow::Result<()> {
    let prefix = domain_prefix(domain_id);
    db::query("DELETE FROM gonof_common_settings WHERE skey IN (?,?,?)")
        .bind(format!("{prefix}body"))
        .bind(format!("{prefix}title"))
        .bind(format!("{prefix}updated_at"))
        .execute(db.observe(&mut **tx))
        .await?;
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
        assert!(Layout::parse(r#"{"v":1,"places":{}}"#).is_err());
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
