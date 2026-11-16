//! Imported skins in `gonof_common_skin` (docs/skins.md, "Importing"): one row per skin id with the normalized
//! manifest, the palettes, the stylesheet as written, the pack's SHA-256 and the warnings the administrator confirmed.
use super::catalog::{Imported, Listed};
use super::{Draft, Source};
use crate::cache::{Block, GlobalCache};
use crate::css_check::Finding;
use crate::db::{self, Database};
use crate::identity::{Actor, IdentityError};

const SELECT: &str = "SELECT skin_id,skin_version,manifest,palettes,css,content_hash,warnings,imported_at,imported_by,version FROM gonof_common_skin ORDER BY skin_id";

/// Every imported skin, read and checked again: rules may have changed since the import, and the row may have been
/// edited by hand. A row that no longer passes is listed as invalid and logged.
pub async fn listed(db: &Database) -> anyhow::Result<Vec<Listed>> {
    let rows = db::query(SELECT).fetch_all(db.executor()).await?;
    let mut listed = Vec::with_capacity(rows.len());
    // Logs are English, whatever language the request that loaded the catalog has.
    let t = crate::i18n::Lang::default();
    for row in rows {
        let id = db::text(&row, "skin_id")?;
        let manifest = serde_json::from_str::<super::Manifest>(&db::text(&row, "manifest")?);
        let palettes = serde_json::from_str::<super::Palettes>(&db::text(&row, "palettes")?);
        let confirmed: Vec<Finding> =
            serde_json::from_str(&db::text(&row, "warnings")?).unwrap_or_default();
        let imported = Imported {
            at: row.try_get("imported_at")?,
            by: row.try_get("imported_by")?,
            row_version: row.try_get("version")?,
            confirmed,
        };
        let css = db::text(&row, "css")?;
        let (checked, manifest) = match (manifest, palettes) {
            (Ok(manifest), Ok(palettes)) => {
                let draft = Draft {
                    manifest,
                    palettes,
                    css,
                };
                let checked = super::check(&draft, Source::Imported, None);
                (checked, Some(draft.manifest))
            }
            (manifest, palettes) => {
                let detail = manifest
                    .err()
                    .or(palettes.err())
                    .map(|error| error.to_string())
                    .unwrap_or_default();
                (
                    super::Checked {
                        skin: None,
                        findings: vec![Finding {
                            file: String::new(),
                            line: None,
                            severity: crate::css_check::Severity::Error,
                            problem: crate::css_check::Problem::PackSyntax { detail },
                        }],
                    },
                    None,
                )
            }
        };
        if let Some(manifest) = &manifest
            && manifest.id != id
        {
            tracing::error!(skin = %id, manifest = %manifest.id, "imported skin row's manifest names another id");
        }
        for finding in checked.errors() {
            tracing::error!(skin = %id, finding = %finding.text(t), "imported skin is unusable");
        }
        let mut entry = Listed::from_checked(&id, Source::Imported, checked, manifest);
        entry.imported = Some(imported);
        listed.push(entry);
    }
    Ok(listed)
}

/// The row version of an imported skin, `None` when there is none.
pub async fn row_version(db: &Database, id: &str) -> anyhow::Result<Option<i64>> {
    Ok(
        db::query_scalar("SELECT version FROM gonof_common_skin WHERE skin_id=?")
            .bind(id)
            .fetch_optional(db.executor())
            .await?,
    )
}

/// Imports a checked draft. `replaces` is the row version the administrator saw (0: none), so a skin imported or
/// deleted meanwhile is not overwritten unseen; `warnings` are the warnings they confirmed.
pub async fn import(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    draft: &Draft,
    warnings: &[Finding],
    replaces: i64,
) -> Result<(), IdentityError> {
    let id = &draft.manifest.id;
    let manifest = draft.manifest.to_json();
    let palettes = serde_json::to_string(&draft.palettes).map_err(anyhow::Error::from)?;
    let hash = super::content_hash(draft);
    let confirmed = serde_json::to_string(warnings).map_err(anyhow::Error::from)?;
    let mut tx = db.begin_write().await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    let current: Option<i64> =
        db::query_scalar("SELECT version FROM gonof_common_skin WHERE skin_id=? FOR UPDATE")
            .bind(id)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    if current.unwrap_or(0) != replaces {
        return Err(IdentityError::Conflict(
            crate::i18n::current().skin_error_changed(),
        ));
    }
    let now = crate::identity::now();
    match current {
        Some(version) => {
            db::query("UPDATE gonof_common_skin SET skin_version=?,manifest=?,palettes=?,css=?,content_hash=?,warnings=?,imported_at=?,imported_by=?,version=version+1 WHERE skin_id=? AND version=?")
                .bind(&draft.manifest.version)
                .bind(&manifest)
                .bind(&palettes)
                .bind(&draft.css)
                .bind(&hash)
                .bind(&confirmed)
                .bind(now)
                .bind(actor.uid)
                .bind(id)
                .bind(version)
                .execute(db.observe(&mut *tx))
                .await?;
        }
        None => {
            db::query("INSERT INTO gonof_common_skin(skin_id,skin_version,manifest,palettes,css,content_hash,warnings,imported_at,imported_by,version) VALUES(?,?,?,?,?,?,?,?,?,1)")
                .bind(id)
                .bind(&draft.manifest.version)
                .bind(&manifest)
                .bind(&palettes)
                .bind(&draft.css)
                .bind(&hash)
                .bind(&confirmed)
                .bind(now)
                .bind(actor.uid)
                .execute(db.observe(&mut *tx))
                .await?;
        }
    }
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        "skin.import",
        serde_json::json!({
            "skin": id,
            "version": draft.manifest.version,
            "content_hash": hash,
            "warnings": warnings.len(),
            "replaced": current.is_some(),
        }),
    )
    .await?;
    tx.commit().await?;
    global
        .announce(&[Block::Skins])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(())
}

/// Deletes an imported skin that no page uses: not chosen for the site or a domain, unless a built-in skin with the
/// same id is the one those pages use anyway.
pub async fn delete(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    id: &str,
    version: i64,
) -> Result<(), IdentityError> {
    let mut tx = db.begin_write().await?;
    let (site, domains) = super::selection::uses(db, &mut tx, id).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    let current: Option<i64> =
        db::query_scalar("SELECT version FROM gonof_common_skin WHERE skin_id=? FOR UPDATE")
            .bind(id)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    let Some(current) = current else {
        return Err(IdentityError::NotFound);
    };
    if current != version {
        return Err(IdentityError::Conflict(
            crate::i18n::current().skin_error_changed(),
        ));
    }
    let shadowed = super::BUILTIN_SKINS
        .iter()
        .any(|(builtin, checked)| builtin == id && checked.skin.is_some());
    if (site || !domains.is_empty()) && !shadowed {
        return Err(IdentityError::Validation(
            crate::i18n::current().skin_error_in_use(),
        ));
    }
    db::query("DELETE FROM gonof_common_skin WHERE skin_id=? AND version=?")
        .bind(id)
        .bind(version)
        .execute(db.observe(&mut *tx))
        .await?;
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        "skin.delete",
        serde_json::json!({"skin": id}),
    )
    .await?;
    tx.commit().await?;
    global
        .announce(&[Block::Skins])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(())
}
