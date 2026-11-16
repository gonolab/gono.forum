//! Which skin a site uses (docs/skins.md, "Choosing a skin"): the layout document `settings.layout.{body,title,
//! updated_at}` for the whole site and one override document per domain, `settings.domain_layout.<domain id>.…`,
//! stored like the navigation document and read by the page frame in the same single statement as the settings.
//!
//! The body is JSON: `{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.2.0"},
//! "places":null}`. Only `skin`, `palette` and `based_on` are used so far; `places` (the blocks of each place on a
//! page) stays `null` until layouts arrive. A domain document leaves out what it inherits from the site's.
use serde::{Deserialize, Serialize};

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

/// Saves a selection: locks the document's rows, checks the administrator and, for an imported skin, that its row
/// still exists (a delete takes it under a lock), writes the document, records `skin.select` and clears the settings
/// block after commit.
pub async fn save(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    selection: Selection<'_>,
) -> Result<(), IdentityError> {
    let prefix = match selection.domain_id {
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
    if let Some(id) = selection.domain_id {
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
    if let Some(skin) = selection.skin
        && skin.source == super::Source::Imported
    {
        let found =
            db::query("SELECT skin_id FROM gonof_common_skin WHERE skin_id=? LOCK IN SHARE MODE")
                .bind(skin.id())
                .fetch_optional(db.observe(&mut *tx))
                .await?;
        if found.is_none() {
            return Err(IdentityError::Validation(
                crate::i18n::current().skin_error_gone(),
            ));
        }
    }
    let mut before = None;
    for row in &rows {
        if db::text(row, "skey")? == keys[0] {
            before = Layout::parse(&db::text(row, "svalue")?).unwrap_or_default();
        }
    }
    let previous = before.as_ref().and_then(|layout| layout.skin.clone());
    let now = crate::identity::now();
    match selection.skin {
        Some(skin) => {
            let mut layout = before.unwrap_or(Layout {
                v: 1,
                ..Layout::default()
            });
            layout.skin = Some(skin.id().to_owned());
            layout.palette = Some(skin.palette(selection.palette).to_owned());
            layout.based_on = Some(BasedOn {
                skin: skin.id().to_owned(),
                version: skin.manifest.version.clone(),
            });
            let title = rows
                .iter()
                .find(|row| db::text(row, "skey").ok().as_deref() == Some(keys[1].as_str()))
                .map(|row| db::text(row, "svalue"))
                .transpose()?
                .unwrap_or_default();
            crate::runtime::write_document(db, &mut tx, &prefix, &layout.body(), &title, now)
                .await?;
        }
        None => {
            db::query("DELETE FROM gonof_common_settings WHERE skey IN (?,?,?)")
                .bind(&keys[0])
                .bind(&keys[1])
                .bind(&keys[2])
                .execute(db.observe(&mut *tx))
                .await?;
        }
    }
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        "skin.select",
        serde_json::json!({
            "domain_id": selection.domain_id,
            "skin": selection.skin.map(super::Skin::id),
            "palette": selection.skin.map(|skin| skin.palette(selection.palette)),
            "previous": previous,
        }),
    )
    .await?;
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
