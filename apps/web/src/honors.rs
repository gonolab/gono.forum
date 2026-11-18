//! Honor groups (docs/roles.md, "Honor groups"): administrators create, rename and disable honor groups in the admin
//! area (`gonof_common_honor`), then grant them to members (`gonof_common_member_honor`; a member can have several
//! groups, at most one row per group, with an optional expiry). An honor group is only a small badge on floors and
//! profile pages that shows the group description on hover; it carries no permissions, cannot be bought with credits,
//! is separate from verified members ([`crate::directory`]), and is not listed in `/verified`.
//!
//! Display conditions are the same as for verification: not expired, member not frozen, account in good standing;
//! the group itself must also be enabled. The authors of one page of floors are fetched in a batch by primary key and
//! are not put in the shared cache.
//! Only administrators write: gate (the admin area works as usual during read-only mode) → administrator recheck →
//! lock the target member row → lock the grant row. Logs `identity.honor.*` are recorded under the member and are
//! deleted with the member's own `identity.%` logs when account deletion runs; creating and editing groups is logged
//! as `site.honor.*`.
use crate::db::Tx;
use std::collections::{BTreeSet, HashMap};

use crate::{
    db::{self, Database, DbRow},
    identity::{Actor, IdentityError},
};

type Result<T> = std::result::Result<T, IdentityError>;

/// Group names are 1–10 characters (floor badges must be short), descriptions 1–60 characters.
pub const NAME_MAX: usize = 10;
pub const DESCRIPTION_MAX: usize = 60;
/// Range of a group's sort value.
pub const ORDER_MAX: i64 = 9999;
/// Number of holders listed per page on the admin group page.
pub const HOLDERS_PAGE_SIZE: i64 = 50;

/// One honor group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Honor {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub displayorder: i64,
    pub enabled: bool,
    pub version: i64,
}

/// One honor badge on a floor or profile page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HonorBadge {
    pub name: String,
    pub description: String,
}

/// Group content submitted by the admin form.
#[derive(Clone, Debug, Default)]
pub struct HonorInput {
    pub name: String,
    pub description: String,
    pub displayorder: i64,
    pub enabled: bool,
}

/// Result of a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Created,
    Updated,
    Unchanged,
}

/// One grant on the admin member page (including disabled groups and expired grants).
#[derive(Clone, Debug)]
pub struct MemberGrant {
    pub honor_id: i64,
    pub name: String,
    pub enabled: bool,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
}

/// One holder on the admin group page.
#[derive(Clone, Debug)]
pub struct Holder {
    pub uid: i64,
    pub username: String,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
}

const COLUMNS: &str = "id,name,description,displayorder,status,version";

fn honor_of(row: &DbRow) -> Result<Honor> {
    Ok(Honor {
        id: row.try_get("id")?,
        name: db::text(row, "name")?,
        description: db::text(row, "description")?,
        displayorder: row.try_get("displayorder")?,
        enabled: row.try_get::<i64, _>("status")? == 1,
        version: row.try_get("version")?,
    })
}

/// Whether one grant is shown right now: group enabled, not expired, member not frozen and account in good standing
/// (not shown while account deletion is running).
fn shown(
    enabled: bool,
    expires_at: Option<i64>,
    freeze: i64,
    account_state: &str,
    now: i64,
) -> bool {
    enabled && expires_at.is_none_or(|at| at > now) && freeze == 0 && account_state == "active"
}

/// One line of plain text: trimmed, 1 to `max` characters, no control characters.
fn plain_line(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn checked(input: &HonorInput) -> Result<(String, String)> {
    let name = plain_line(&input.name, NAME_MAX).ok_or(IdentityError::Validation(
        crate::i18n::current().profile_manage_error_name(),
    ))?;
    let description = plain_line(&input.description, DESCRIPTION_MAX).ok_or(
        IdentityError::Validation(crate::i18n::current().profile_manage_error_description()),
    )?;
    if !(0..=ORDER_MAX).contains(&input.displayorder) {
        return Err(IdentityError::Validation(
            crate::i18n::current().profile_manage_error_order(),
        ));
    }
    Ok((name, description))
}

/// All honor groups, by sort value and id (there are few groups, so the whole table is read).
pub async fn honors(db: &Database) -> Result<Vec<Honor>> {
    db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_honor ORDER BY displayorder,id"
    ))
    .fetch_all(db.executor())
    .await?
    .iter()
    .map(honor_of)
    .collect()
}

pub async fn honor(db: &Database, id: i64) -> Result<Option<Honor>> {
    db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_honor WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(db.executor())
    .await?
    .map(|row| honor_of(&row))
    .transpose()
}

/// The honor badges a set of members shows right now, in group order. Fetched in a batch by primary key (the authors
/// of one page of floors), not put in the shared cache.
pub async fn badges(
    db: &Database,
    uids: &[i64],
    now: i64,
) -> Result<HashMap<i64, Vec<HonorBadge>>> {
    let uids: BTreeSet<i64> = uids.iter().copied().filter(|uid| *uid > 0).collect();
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql = format!(
        "SELECT g.uid,g.expires_at,h.id,h.name,h.description,h.displayorder,h.status,m.freeze,m.account_state \
         FROM gonof_common_member_honor g JOIN gonof_common_honor h ON h.id=g.honor_id JOIN gonof_common_member m ON m.uid=g.uid \
         WHERE g.uid IN ({placeholders})"
    );
    let mut query = db::query(&sql);
    for uid in &uids {
        query = query.bind(*uid);
    }
    // Each badge carries its group's (sort value, id), and the list is sorted by it once filled.
    type Ordered = Vec<((i64, i64), HonorBadge)>;
    let mut found: HashMap<i64, Ordered> = HashMap::new();
    for row in query.fetch_all(db.executor()).await? {
        let account_state = db::text(&row, "account_state")?;
        if !shown(
            row.try_get::<i64, _>("status")? == 1,
            row.try_get("expires_at")?,
            row.try_get("freeze")?,
            &account_state,
            now,
        ) {
            continue;
        }
        found.entry(row.try_get("uid")?).or_default().push((
            (row.try_get("displayorder")?, row.try_get("id")?),
            HonorBadge {
                name: db::text(&row, "name")?,
                description: db::text(&row, "description")?,
            },
        ));
    }
    Ok(found
        .into_iter()
        .map(|(uid, mut list)| {
            list.sort_by_key(|(order, _)| *order);
            (uid, list.into_iter().map(|(_, badge)| badge).collect())
        })
        .collect())
}

/// Like [`badges`], but the authors' freeze and account state have already been read (the thread page's
/// [`crate::growth::page_authors`]) and the group catalog comes from the process cache
/// ([`crate::cache::GlobalCache::honors`]): grants are read only for members in `active`, from one table by primary
/// key.
pub async fn badges_of_active(
    db: &Database,
    catalog: &[Honor],
    active: &BTreeSet<i64>,
    now: i64,
) -> Result<HashMap<i64, Vec<HonorBadge>>> {
    if active.is_empty() || !catalog.iter().any(|honor| honor.enabled) {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; active.len()].join(",");
    let sql = format!(
        "SELECT uid,honor_id,expires_at FROM gonof_common_member_honor WHERE uid IN ({placeholders})"
    );
    let mut query = db::query(&sql);
    for uid in active {
        query = query.bind(*uid);
    }
    type Ordered = Vec<((i64, i64), HonorBadge)>;
    let mut found: HashMap<i64, Ordered> = HashMap::new();
    for row in query.fetch_all(db.executor()).await? {
        let honor_id: i64 = row.try_get("honor_id")?;
        // A group not in the catalog (just created within the cache lifetime) is not shown this time.
        let Some(honor) = catalog.iter().find(|honor| honor.id == honor_id) else {
            continue;
        };
        if !shown(honor.enabled, row.try_get("expires_at")?, 0, "active", now) {
            continue;
        }
        found.entry(row.try_get("uid")?).or_default().push((
            (honor.displayorder, honor.id),
            HonorBadge {
                name: honor.name.clone(),
                description: honor.description.clone(),
            },
        ));
    }
    Ok(found
        .into_iter()
        .map(|(uid, mut list)| {
            list.sort_by_key(|(order, _)| *order);
            (uid, list.into_iter().map(|(_, badge)| badge).collect())
        })
        .collect())
}

/// The honor badges one member shows right now.
pub async fn member_badges(db: &Database, uid: i64, now: i64) -> Result<Vec<HonorBadge>> {
    Ok(badges(db, &[uid], now)
        .await?
        .remove(&uid)
        .unwrap_or_default())
}

/// Admin member page: all grants of this member (including disabled groups and expired grants), in group order.
pub async fn member_grants(db: &Database, uid: i64) -> Result<Vec<MemberGrant>> {
    db::query(
        "SELECT g.honor_id,g.granted_at,g.expires_at,h.name,h.status FROM gonof_common_member_honor g \
         JOIN gonof_common_honor h ON h.id=g.honor_id WHERE g.uid=? ORDER BY h.displayorder,h.id",
    )
    .bind(uid)
    .fetch_all(db.executor())
    .await?
    .iter()
    .map(|row| {
        Ok(MemberGrant {
            honor_id: row.try_get("honor_id")?,
            name: db::text(row, "name")?,
            enabled: row.try_get::<i64, _>("status")? == 1,
            granted_at: row.try_get("granted_at")?,
            expires_at: row.try_get("expires_at")?,
        })
    })
    .collect()
}

/// Admin group page: holders by grant time, newest first, [`HOLDERS_PAGE_SIZE`] per page; one extra is fetched to
/// tell whether there is a next page.
pub async fn holders(db: &Database, honor_id: i64, page: i64) -> Result<(Vec<Holder>, bool)> {
    if !(1..=10_000).contains(&page) {
        return Err(IdentityError::NotFound);
    }
    let mut rows = db::query(
        "SELECT g.uid,g.granted_at,g.expires_at,m.username FROM gonof_common_member_honor g \
         JOIN gonof_common_member m ON m.uid=g.uid WHERE g.honor_id=? \
         ORDER BY g.granted_at DESC,g.uid DESC LIMIT ? OFFSET ?",
    )
    .bind(honor_id)
    .bind(HOLDERS_PAGE_SIZE + 1)
    .bind((page - 1) * HOLDERS_PAGE_SIZE)
    .fetch_all(db.executor())
    .await?;
    let more = rows.len() as i64 > HOLDERS_PAGE_SIZE;
    rows.truncate(HOLDERS_PAGE_SIZE as usize);
    let holders = rows
        .iter()
        .map(|row| {
            Ok(Holder {
                uid: row.try_get("uid")?,
                username: db::text(row, "username")?,
                granted_at: row.try_get("granted_at")?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .collect::<Result<_>>()?;
    Ok((holders, more))
}

/// One member shown with an honor group outside the admin area.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Listed {
    pub uid: i64,
    pub username: String,
}

/// The newest holders of an honor group for the public (the `members.honors` data source, docs/data.md): grants
/// that have not expired, of active members who are not frozen and whose space the visitor's class may see
/// (`signed_in` adds "Logged-in members"), newest grant first, at most `limit`. The viewer's own blocks are removed
/// by the caller, so one result serves every visitor of a class. Reads `gonof_member_honor_group` (`honor_id`,
/// `granted_at`, `uid`) backwards from the group's newest grant and the member row by primary key.
pub async fn listed_holders(
    db: &Database,
    honor_id: i64,
    signed_in: bool,
    now: i64,
    limit: i64,
) -> anyhow::Result<Vec<Listed>> {
    let space = if signed_in {
        "m.space_visibility IN ('public','members')"
    } else {
        "m.space_visibility='public'"
    };
    let rows = db::query(&format!(
        "SELECT g.uid,m.username FROM gonof_common_member_honor g FORCE INDEX (gonof_member_honor_group) \
         JOIN gonof_common_member m ON m.uid=g.uid \
         WHERE g.honor_id=? AND (g.expires_at IS NULL OR g.expires_at>?) \
         AND m.freeze=0 AND m.account_state='active' AND {space} \
         ORDER BY g.granted_at DESC,g.uid DESC LIMIT ?"
    ))
    .bind(honor_id)
    .bind(now)
    .bind(limit)
    .fetch_all(db.executor())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(Listed {
                uid: row.try_get("uid")?,
                username: db::text(row, "username")?,
            })
        })
        .collect()
}

async fn log(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    operator: i64,
    kind: &str,
    data: serde_json::Value,
    now: i64,
) -> Result<()> {
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
        .bind(uid)
        .bind(operator)
        .bind(kind)
        .bind(data.to_string())
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// A group name must not duplicate another group's (the unique key is the backstop; this gives a readable message
/// first).
async fn name_taken(db: &Database, tx: &mut Tx<'_>, name: &str, except: i64) -> Result<bool> {
    Ok(db::query_scalar::<i64>(
        "SELECT id FROM gonof_common_honor WHERE name=? AND id<>? FOR UPDATE",
    )
    .bind(name)
    .bind(except)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .is_some())
}

/// Creates an honor group (administrators only) and returns its id.
pub async fn create(db: &Database, actor: Actor, input: &HonorInput, now: i64) -> Result<i64> {
    let (name, description) = checked(input)?;
    let mut tx = db.begin_write().await?;
    crate::runtime::guard_write(db, &mut tx, true).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    if name_taken(db, &mut tx, &name, 0).await? {
        return Err(IdentityError::Conflict(
            crate::i18n::current().profile_manage_error_name_taken(),
        ));
    }
    let id = db::query(
        "INSERT INTO gonof_common_honor(name,description,displayorder,status) VALUES(?,?,?,?)",
    )
    .bind(&name)
    .bind(&description)
    .bind(input.displayorder)
    .bind(i64::from(input.enabled))
    .execute(db.observe(&mut *tx))
    .await?
    .last_insert_id() as i64;
    log(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "site.honor.create",
        serde_json::json!({"honor_id": id, "name": name, "enabled": input.enabled}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Edits an honor group (administrators only): rename, change the description or sort value, enable or disable.
/// Compares against the version the form read, returning a conflict if someone else changed it first;
/// writes nothing if the content is unchanged. A disabled group is no longer shown, but existing grants are kept and
/// come back when it is re-enabled. Groups cannot be deleted.
pub async fn update(
    db: &Database,
    actor: Actor,
    id: i64,
    expected_version: i64,
    input: &HonorInput,
    now: i64,
) -> Result<Saved> {
    let (name, description) = checked(input)?;
    let mut tx = db.begin_write().await?;
    crate::runtime::guard_write(db, &mut tx, true).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    let current = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_honor WHERE id=? FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .map(|row| honor_of(&row))
    .transpose()?
    .ok_or(IdentityError::NotFound)?;
    if current.version != expected_version {
        return Err(IdentityError::Conflict(
            crate::i18n::current().profile_manage_error_changed(),
        ));
    }
    let mut changed = Vec::new();
    if current.name != name {
        changed.push("name");
    }
    if current.description != description {
        changed.push("description");
    }
    if current.displayorder != input.displayorder {
        changed.push("displayorder");
    }
    if current.enabled != input.enabled {
        changed.push("status");
    }
    if changed.is_empty() {
        return Ok(Saved::Unchanged);
    }
    if current.name != name && name_taken(db, &mut tx, &name, id).await? {
        return Err(IdentityError::Conflict(
            crate::i18n::current().profile_manage_error_name_taken(),
        ));
    }
    db::query("UPDATE gonof_common_honor SET name=?,description=?,displayorder=?,status=?,version=version+1 WHERE id=? AND version=?")
        .bind(&name)
        .bind(&description)
        .bind(input.displayorder)
        .bind(i64::from(input.enabled))
        .bind(id)
        .bind(expected_version)
        .execute(db.observe(&mut *tx))
        .await?;
    log(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "site.honor.update",
        serde_json::json!({"honor_id": id, "name": name, "enabled": input.enabled, "changed": changed}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(Saved::Updated)
}

/// Gate (admin moderation is let through during read-only mode) → administrator recheck → lock the target member row,
/// returning the account state. A missing member is `NotFound`.
async fn lock_subject(db: &Database, tx: &mut Tx<'_>, actor: Actor, uid: i64) -> Result<String> {
    crate::runtime::guard_write(db, tx, true).await?;
    crate::runtime::admin_locked(db, tx, actor).await?;
    let state: String =
        db::query_scalar("SELECT account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .ok_or(IdentityError::NotFound)?;
    Ok(state)
}

/// Grants or renews (administrators only). `expires_at` of `None` means permanent, otherwise it is the expiry time
/// (midnight at the start of the day after a given day in the site time zone).
/// Creates the grant if there is none; an existing one only has its expiry changed, and nothing is written if nothing
/// changed. A disabled group cannot be newly granted; a frozen member can be granted
/// (not shown while frozen), but not one whose account deletion is running.
///
/// `renewing` is the `granted_at` of the grant a renewal form was opened on. When given, that grant must still be
/// there with the same `granted_at`; otherwise it was revoked (and possibly granted again) since the form was read,
/// and the renewal is refused with a conflict instead of granting the group anew.
pub async fn grant(
    db: &Database,
    actor: Actor,
    uid: i64,
    honor_id: i64,
    expires_at: Option<i64>,
    renewing: Option<i64>,
    now: i64,
) -> Result<Saved> {
    let mut tx = db.begin_write().await?;
    if lock_subject(db, &mut tx, actor, uid).await? != "active" {
        return Err(IdentityError::Validation(
            crate::i18n::current().profile_manage_error_deleting(),
        ));
    }
    let enabled: i64 =
        db::query_scalar("SELECT status FROM gonof_common_honor WHERE id=? LOCK IN SHARE MODE")
            .bind(honor_id)
            .fetch_optional(db.observe(&mut *tx))
            .await?
            .ok_or(IdentityError::Validation(
                crate::i18n::current().profile_manage_error_choose(),
            ))?;
    let current: Option<(i64, Option<i64>)> = db::query(
        "SELECT granted_at,expires_at FROM gonof_common_member_honor WHERE uid=? AND honor_id=? FOR UPDATE",
    )
    .bind(uid)
    .bind(honor_id)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .map(|row| -> Result<_> { Ok((row.try_get("granted_at")?, row.try_get("expires_at")?)) })
    .transpose()?;
    if renewing.is_some() && renewing != current.map(|(granted_at, _)| granted_at) {
        return Err(IdentityError::Conflict(
            crate::i18n::current().profile_manage_error_changed(),
        ));
    }
    let existing = current.map(|(_, expires_at)| expires_at);
    // A newly entered expiry date must be in the future; an already expired date submitted back unchanged is not
    // blocked.
    if existing != Some(expires_at) && expires_at.is_some_and(|at| at <= now) {
        return Err(IdentityError::Validation(
            crate::i18n::current().profile_manage_error_past(),
        ));
    }
    let saved = match existing {
        None => {
            if enabled != 1 {
                return Err(IdentityError::Validation(
                    crate::i18n::current().profile_manage_error_disabled(),
                ));
            }
            db::query("INSERT INTO gonof_common_member_honor(uid,honor_id,granted_by,granted_at,expires_at) VALUES(?,?,?,?,?)")
                .bind(uid)
                .bind(honor_id)
                .bind(actor.uid)
                .bind(now)
                .bind(expires_at)
                .execute(db.observe(&mut *tx))
                .await?;
            log(
                db,
                &mut tx,
                uid,
                actor.uid,
                "identity.honor.grant",
                serde_json::json!({"honor_id": honor_id, "expires_at": expires_at}),
                now,
            )
            .await?;
            Saved::Created
        }
        Some(old) if old == expires_at => Saved::Unchanged,
        Some(_) => {
            db::query(
                "UPDATE gonof_common_member_honor SET expires_at=? WHERE uid=? AND honor_id=?",
            )
            .bind(expires_at)
            .bind(uid)
            .bind(honor_id)
            .execute(db.observe(&mut *tx))
            .await?;
            log(
                db,
                &mut tx,
                uid,
                actor.uid,
                "identity.honor.renew",
                serde_json::json!({"honor_id": honor_id, "expires_at": expires_at}),
                now,
            )
            .await?;
            Saved::Updated
        }
    };
    tx.commit().await?;
    Ok(saved)
}

/// Revokes one grant (administrators only, a reason is required). Writes nothing and returns `false` if there is no
/// such grant.
pub async fn revoke(
    db: &Database,
    actor: Actor,
    uid: i64,
    honor_id: i64,
    reason: &str,
    now: i64,
) -> Result<bool> {
    let reason = crate::administration::reason(reason)?.to_owned();
    let mut tx = db.begin_write().await?;
    lock_subject(db, &mut tx, actor, uid).await?;
    let found: Option<i64> = db::query_scalar(
        "SELECT honor_id FROM gonof_common_member_honor WHERE uid=? AND honor_id=? FOR UPDATE",
    )
    .bind(uid)
    .bind(honor_id)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if found.is_none() {
        return Ok(false);
    }
    db::query("DELETE FROM gonof_common_member_honor WHERE uid=? AND honor_id=?")
        .bind(uid)
        .bind(honor_id)
        .execute(db.observe(&mut *tx))
        .await?;
    log(
        db,
        &mut tx,
        uid,
        actor.uid,
        "identity.honor.revoke",
        serde_json::json!({"honor_id": honor_id, "reason": reason}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// When account deletion runs (the caller already holds the member row lock): deletes all of the member's grants.
pub async fn forget_member(db: &Database, tx: &mut Tx<'_>, uid: i64) -> anyhow::Result<()> {
    db::query("DELETE FROM gonof_common_member_honor WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_needs_enabled_unexpired_unfrozen_active() {
        assert!(shown(true, None, 0, "active", 100));
        assert!(shown(true, Some(101), 0, "active", 100));
        assert!(
            !shown(true, Some(100), 0, "active", 100),
            "hidden from the expiry time on"
        );
        assert!(
            !shown(false, None, 0, "active", 100),
            "a disabled group is hidden"
        );
        assert!(!shown(true, None, 1, "active", 100), "hidden while frozen");
        assert!(
            !shown(true, None, 0, "deleting", 100),
            "hidden while the account is being deleted"
        );
    }

    #[test]
    fn group_fields_are_checked() {
        let input = |name: &str, description: &str, order| HonorInput {
            name: name.into(),
            description: description.into(),
            displayorder: order,
            enabled: true,
        };
        assert_eq!(
            checked(&input(" Helpers ", "About", 1)).unwrap(),
            ("Helpers".to_owned(), "About".to_owned())
        );
        assert!(checked(&input("", "About", 1)).is_err());
        assert!(checked(&input(&"x".repeat(NAME_MAX + 1), "About", 1)).is_err());
        assert!(checked(&input("Group", &"x".repeat(DESCRIPTION_MAX + 1), 1)).is_err());
        assert!(checked(&input("Group\nname", "About", 1)).is_err());
        assert!(checked(&input("Group", "About", -1)).is_err());
        assert!(checked(&input("Group", "About", 10_000)).is_err());
    }
}
