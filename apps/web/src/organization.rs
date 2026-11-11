//! Source of truth for the two-level forums; all public entry points reuse the same access conditions.
use crate::db::Tx;
use std::collections::BTreeSet;

use serde::Deserialize;

use crate::{
    content::writes::ContentWriteError,
    db::{self, Database, DbRow},
    identity::{self, Actor},
};

type Result<T> = std::result::Result<T, ContentWriteError>;
/// Each parent forum has at most this many child forums: the parent forum page fetches its list in
/// segments of "visibility combination × forum count", so the number of segments must be bounded.
pub const MAX_CHILD_FORUMS: i64 = 10;

fn namespace_error(error: sqlx::Error) -> ContentWriteError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        ContentWriteError::Conflict(crate::i18n::current().forum_error_taken())
    } else {
        error.into()
    }
}

macro_rules! retry_transaction {
    ($attempt:expr) => {{
        let mut retry=0_u64;
        loop {
            let result=$attempt.await;
            if matches!(&result,Err(error) if retry<2 && error.retryable()) {
                retry+=1;
                tokio::time::sleep(std::time::Duration::from_millis(20*retry)).await;
            } else { break result; }
        }
    }};
}

/// viewer_uid may only come from an authenticated member context, never from request parameters.
pub fn forum_readable_sql(alias: &str, viewer_uid: Option<i64>) -> String {
    let visibility = if viewer_uid.is_some() {
        "IN ('public','members')"
    } else {
        "='public'"
    };
    format!(
        "{alias}.status=1 AND {alias}.visibility {visibility} AND ({alias}.parent_fid IS NULL OR EXISTS(SELECT 1 FROM gonof_forum_forum ancestor WHERE ancestor.fid={alias}.parent_fid AND ancestor.status=1 AND ancestor.visibility {visibility}))"
    )
}

/// Moderator scope: a moderator manages the forums they are appointed to and
/// their child forums. There are only two levels, so "manages X" means an appointment row on X or on X's parent forum.
/// The anti-spam exemption (`content::antispam::manages`) uses the same rule.
///
/// `uid` is an authenticated member UID expression (integer literal, placeholder or outer column); `forum` is the alias
/// of a forum row already joined in the SQL, whose `fid` and `parent_fid` are used.
pub fn moderates_sql(uid: &str, forum: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM gonof_forum_moderator fm WHERE fm.uid = {uid} AND fm.fid IN ({forum}.fid, {forum}.parent_fid))"
    )
}

/// Same as [`moderates_sql`], for when there is only a forum FID expression and no joined forum row (logs, overview
/// counts, report and restore-request queues). `uid` may be a column of the outer query: it goes in `WHERE`, since 5.7
/// does not accept references to outer columns inside `JOIN … ON` (1054).
pub fn moderates_fid_sql(uid: &str, fid: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM gonof_forum_forum mf JOIN gonof_forum_moderator fm ON fm.fid IN (mf.fid, mf.parent_fid) WHERE mf.fid = {fid} AND fm.uid = {uid})"
    )
}

/// Moderator write authorization: in the caller's transaction, takes exclusive locks on the appointment rows for this
/// forum and its parent forum; one row means the forum is managed.
///
/// One statement locks in ascending primary key `(uid, fid)` order, the same order as "forum paths in ascending FID".
/// The caller has already locked the forum row, and `parent` comes from the `parent_fid` read under the lock; changing
/// the parent forum first requires an exclusive lock on this forum row, so the hierarchy cannot change under the lock.
pub async fn moderator_locked(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    fid: i64,
    parent: Option<i64>,
) -> std::result::Result<bool, sqlx::Error> {
    let assigned: Vec<i64> = db::query_scalar(
        "SELECT fid FROM gonof_forum_moderator WHERE uid=? AND fid IN (?,?) FOR UPDATE",
    )
    .bind(uid)
    .bind(fid)
    .bind(parent.unwrap_or(fid))
    .fetch_all(db.observe(&mut **tx))
    .await?;
    Ok(!assigned.is_empty())
}

/// One row of the moderator roster (docs/settings-and-caching.md, "The process cache"; docs/roles.md, "Moderators and
/// forums"): one site-wide copy of `gonof_forum_moderator` joined with the members' usernames and roles. **Display and
/// read-only checks only** (the forum page header "Moderators: A, B", the moderator column of the admin forum list,
/// management links and attachment downloads through [`crate::identity::manages_forum`]); moderation actions recheck
/// the appointment row with a locking read ([`moderator_locked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    pub fid: i64,
    pub uid: i64,
    pub username: String,
    pub adminid: i64,
}

impl RosterEntry {
    /// Name in the moderator list: trainee moderators are marked "(trainee)" after the name.
    pub fn display_name(&self) -> String {
        if self.adminid == crate::identity::ROLE_INTERN {
            crate::i18n::current().forum_moderator_intern(&self.username)
        } else {
            self.username.clone()
        }
    }
}

/// Loads the moderator roster: appointment rows ordered by `fid, uid`; the row count is the number of appointments
/// (there are very few moderator appointments site-wide).
pub async fn moderator_roster(db: &Database) -> anyhow::Result<Vec<RosterEntry>> {
    db::query("SELECT fm.fid,fm.uid,m.username,m.adminid FROM gonof_forum_moderator fm JOIN gonof_common_member m ON m.uid=fm.uid ORDER BY fm.fid,fm.uid")
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| {
            Ok(RosterEntry {
                fid: row.try_get("fid")?,
                uid: row.try_get("uid")?,
                username: db::text(&row, "username")?,
                adminid: row.try_get("adminid")?,
            })
        })
        .collect()
}

/// A forum's moderators, for display: those appointed to this forum first, then those of the parent forum merged in
/// (parent forum moderators manage child forums), deduplicated by uid; only those whose role is still forum
/// moderator or trainee moderator (appointment rows left after the role changes to global moderator or ordinary
/// member are not shown). Frozen moderators and those under sanction are shown as usual. The display name uses
/// [`RosterEntry::display_name`]; trainee moderators carry "(trainee)".
pub fn forum_moderators(
    roster: &[RosterEntry],
    fid: i64,
    parent: Option<i64>,
) -> Vec<&RosterEntry> {
    let mut shown: Vec<&RosterEntry> = Vec::new();
    for forum in std::iter::once(fid).chain(parent) {
        for entry in roster
            .iter()
            .filter(|entry| entry.fid == forum && crate::identity::forum_role(entry.adminid))
        {
            if !shown.iter().any(|seen| seen.uid == entry.uid) {
                shown.push(entry);
            }
        }
    }
    shown
}

/// After finding the ancestors, locks in FID order, then confirms with a current read that the hierarchy has not
/// changed.
pub async fn lock_forum_path(db: &Database, tx: &mut Tx<'_>, fid: i64) -> Result<DbRow> {
    forum_path(db, tx, fid, "FOR UPDATE").await
}

/// Same as [`lock_forum_path`], but takes only shared locks, for writes that only need "the forum does not change
/// before commit".
///
/// Posting and interactions do not write forum rows; locking them only keeps the forum's status, visibility and
/// hierarchy unchanged until this transaction commits. Only forum management and thread moves change these, and they
/// take exclusive locks on forum rows, which still wait for shared locks to be released; meanwhile posting, likes,
/// favorites and follows do not queue behind each other. A transaction holding a shared lock must not later take an
/// exclusive lock on the same forum row, or two transactions each holding a shared lock and waiting to upgrade will
/// deadlock.
pub async fn share_forum_path(db: &Database, tx: &mut Tx<'_>, fid: i64) -> Result<DbRow> {
    forum_path(db, tx, fid, "LOCK IN SHARE MODE").await
}

async fn forum_path(db: &Database, tx: &mut Tx<'_>, fid: i64, mode: &str) -> Result<DbRow> {
    let parent: Option<i64> =
        db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
            .bind(fid)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
    let mut ids = BTreeSet::from([fid]);
    ids.extend(parent);
    for id in ids {
        db::query(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE fid=? {mode}"
        ))
        .bind(id)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    }
    let row = db::query(&format!("SELECT f.*,CASE WHEN {} THEN 1 ELSE 0 END AS readable FROM gonof_forum_forum f WHERE f.fid=? {mode}",forum_readable_sql("f",Some(1))))
        .bind(fid).fetch_one(db.observe(&mut **tx)).await?;
    if row.try_get::<Option<i64>, _>("parent_fid")? != parent {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().forum_error_hierarchy_changed(),
        ));
    }
    Ok(row)
}

pub async fn lock_forum_paths(db: &Database, tx: &mut Tx<'_>, fids: &[i64]) -> Result<()> {
    let mut expected = std::collections::BTreeMap::new();
    let mut ids = BTreeSet::new();
    for &fid in fids {
        let parent: Option<i64> =
            db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
                .bind(fid)
                .fetch_optional(db.observe(&mut **tx))
                .await?
                .ok_or(ContentWriteError::NotFound)?;
        expected.insert(fid, parent);
        ids.insert(fid);
        ids.extend(parent);
    }
    for id in ids {
        db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
            .bind(id)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
    }
    for (fid, parent) in expected {
        let current: Option<i64> =
            db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
                .bind(fid)
                .fetch_one(db.observe(&mut **tx))
                .await?;
        if current != parent {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().forum_error_hierarchy_retry(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ForumRecord {
    pub fid: i64,
    pub parent_fid: Option<i64>,
    pub icon_aid: Option<i64>,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub guidance: String,
    pub displayorder: i64,
    pub status: i64,
    pub visibility: String,
    pub version: i64,
}
impl ForumRecord {
    pub fn parent_value(&self) -> i64 {
        self.parent_fid.unwrap_or(0)
    }
    pub fn icon_value(&self) -> i64 {
        self.icon_aid.unwrap_or(0)
    }
}
fn forum_row(r: DbRow) -> anyhow::Result<ForumRecord> {
    Ok(ForumRecord {
        fid: r.try_get("fid")?,
        parent_fid: r.try_get("parent_fid")?,
        icon_aid: r.try_get("icon_aid")?,
        name: r.try_get("name")?,
        slug: r.try_get("slug")?,
        description: r.try_get("description")?,
        guidance: r.try_get("guidance")?,
        displayorder: r.try_get("displayorder")?,
        status: r.try_get("status")?,
        visibility: r.try_get("visibility")?,
        version: r.try_get("version")?,
    })
}
/// Forums readable by this visitor, by the same rule as [`forum_readable_sql`] (this level and the parent both enabled,
/// visibility of both allowing). There are only a few dozen forums, so the whole table is read at once and the parent
/// is judged in code instead of with a nested subquery (docs/queries.md, "Process caches").
pub async fn forums(db: &Database, viewer_uid: Option<i64>) -> anyhow::Result<Vec<ForumRecord>> {
    let all = db::query("SELECT f.* FROM gonof_forum_forum f ORDER BY f.displayorder,f.fid")
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(forum_row)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let own = |forum: &ForumRecord| {
        forum.status == 1
            && (forum.visibility == "public"
                || (viewer_uid.is_some() && forum.visibility == "members"))
    };
    let readable: std::collections::HashSet<i64> = all
        .iter()
        .filter(|forum| own(forum))
        .map(|forum| forum.fid)
        .collect();
    Ok(all
        .into_iter()
        .filter(|forum| {
            readable.contains(&forum.fid)
                && forum
                    .parent_fid
                    .is_none_or(|parent| readable.contains(&parent))
        })
        .collect())
}
/// Loader for the process forum cache ([`crate::cache::GlobalCache::forums`]): one copy for anonymous visitors, one for
/// members. [`forum_readable_sql`] only looks at whether the visitor is signed in and does not bind a uid, so any uid
/// will do for the member copy.
pub async fn readable_forums(db: &Database, members: bool) -> anyhow::Result<Vec<ForumRecord>> {
    forums(db, members.then_some(0)).await
}
pub async fn managed_forums(db: &Database, actor: Actor) -> Result<Vec<ForumRecord>> {
    crate::manage::staff(db, actor)
        .await
        .map_err(identity_error)?;
    let rows = db::query(&format!(
        "SELECT f.* FROM gonof_forum_forum f JOIN gonof_common_member m ON m.uid=? WHERE m.adminid IN (1,2) OR {} ORDER BY f.displayorder,f.fid",
        moderates_sql("m.uid", "f")
    ))
    .bind(actor.uid)
    .fetch_all(db.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(forum_row)
        .collect::<anyhow::Result<_>>()?)
}
pub(crate) fn identity_error(e: identity::IdentityError) -> ContentWriteError {
    match e {
        identity::IdentityError::Unauthorized => ContentWriteError::Unauthorized,
        identity::IdentityError::Forbidden => {
            ContentWriteError::Forbidden(crate::i18n::current().forum_error_forbidden())
        }
        identity::IdentityError::NotFound => ContentWriteError::NotFound,
        identity::IdentityError::Validation(m) => ContentWriteError::Validation(m),
        identity::IdentityError::Conflict(m) => ContentWriteError::Conflict(m),
        e => ContentWriteError::Internal(e.into()),
    }
}
async fn role_locked(db: &Database, tx: &mut Tx<'_>, actor: Actor) -> Result<i64> {
    crate::interaction::lock_actor(db, tx, actor, false).await?;
    Ok(
        db::query_scalar("SELECT adminid FROM gonof_common_member WHERE uid=?")
            .bind(actor.uid)
            .fetch_one(db.observe(&mut **tx))
            .await?,
    )
}
/// `forum` is the forum row the caller read under the lock (the result of [`lock_forum_path`]); the parent forum ID
/// comes from it.
async fn require_scope(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    role: i64,
    forum: &DbRow,
) -> Result<()> {
    if matches!(role, 1 | 2) {
        return Ok(());
    }
    let fid: i64 = forum.try_get("fid")?;
    let parent: Option<i64> = forum.try_get("parent_fid")?;
    if role != 3 || !moderator_locked(db, tx, actor.uid, fid, parent).await? {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().forum_error_forum_permission(),
        ));
    }
    Ok(())
}
async fn audit(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    kind: &str,
    data: serde_json::Value,
) -> Result<()> {
    let [pid, fid, to_fid] = crate::manage::log_targets(kind, &data);
    db::query(
        "INSERT INTO gonof_common_log(operationuid,type,data,source,dateline,pid,fid,to_fid) VALUES(?,?,?,'web',?,?,?,?)",
    )
    .bind(actor.uid)
    .bind(kind)
    .bind(data.to_string())
    .bind(identity::now())
    .bind(pid)
    .bind(fid)
    .bind(to_fid)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}
fn reason(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 500 || value.contains('\0') {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_reason(),
        ));
    }
    Ok(value)
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct ForumInput {
    pub fid: i64,
    pub parent_fid: i64,
    pub icon_aid: i64,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub guidance: String,
    pub displayorder: i64,
    pub status: i64,
    pub visibility: String,
    pub version: i64,
}
/// Value rules for forum name, slug, visibility and enabled state (the rules that need only this row, guaranteed
/// here, not by the schema; hierarchy rules need other forum rows and are checked under the lock in `save_forum`).
/// Name 1–50 characters; slug 1–64 lowercase letters, digits or hyphens, not taking a system entry point and not ending
/// in "-digits"; visibility and enabled state take exact values. On success returns the tidied name and slug.
pub fn forum_fields(
    name: &str,
    slug: &str,
    visibility: &str,
    status: i64,
) -> Option<(String, String)> {
    let name = name.trim();
    let slug = slug.trim().to_lowercase();
    let valid = !name.is_empty()
        && name.chars().count() <= 50
        && !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !crate::urls::reserved_segment(&slug)
        && !slug
            .rsplit_once('-')
            .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && crate::content::copies::ForumVisibility::parse(visibility).is_some()
        && matches!(status, 0 | 1);
    valid.then(|| (name.to_owned(), slug))
}
pub async fn save_forum(db: &Database, actor: Actor, input: &ForumInput) -> Result<i64> {
    save_forum_with_orphan_seconds(
        db,
        actor,
        input,
        crate::media::MediaConfig::default().orphan_seconds,
    )
    .await
}
pub async fn save_forum_with_orphan_seconds(
    db: &Database,
    actor: Actor,
    input: &ForumInput,
    orphan_seconds: i64,
) -> Result<i64> {
    retry_transaction!(save_forum_with_orphan_seconds_inner(
        db,
        actor,
        input,
        orphan_seconds
    ))
}
async fn save_forum_with_orphan_seconds_inner(
    db: &Database,
    actor: Actor,
    input: &ForumInput,
    orphan_seconds: i64,
) -> Result<i64> {
    if orphan_seconds <= 0 {
        return Err(ContentWriteError::Validation(
            "attachment retention seconds must be positive",
        ));
    }
    let fields = forum_fields(&input.name, &input.slug, &input.visibility, input.status);
    let Some((name, slug)) = fields else {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_fields(),
        ));
    };
    let name = name.as_str();
    // Description and guidance are limited by character count, not bytes, the same rule as the form's `maxlength`.
    if input.fid < 0
        || input.version < 0
        || (input.fid > 0 && input.version == 0)
        || input.parent_fid < 0
        || input.icon_aid < 0
        || input.description.chars().count() > 4000
        || input.guidance.chars().count() > 8000
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_fields(),
        ));
    }
    let mut tx = db.begin_content_write().await?;
    if role_locked(db, &mut tx, actor).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().forum_error_admin_only(),
        ));
    }
    let paths: Vec<_> = [input.fid, input.parent_fid]
        .into_iter()
        .filter(|fid| *fid > 0)
        .collect();
    lock_forum_paths(db, &mut tx, &paths).await?;
    if input.parent_fid > 0 {
        let p = lock_forum_path(db, &mut tx, input.parent_fid).await?;
        if p.try_get::<Option<i64>, _>("parent_fid")?.is_some() || input.parent_fid == input.fid {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_levels(),
            ));
        }
        // The parent forum row is exclusively locked: another save adding a child forum to it must lock it first, so
        // the count cannot change under the lock.
        let siblings: i64 = db::query_scalar(
            "SELECT COUNT(*) FROM gonof_forum_forum WHERE parent_fid=? AND fid<>?",
        )
        .bind(input.parent_fid)
        .bind(input.fid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        if siblings >= MAX_CHILD_FORUMS {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_children(),
            ));
        }
    }
    let mut current_icon = None;
    let mut before = None;
    if input.fid > 0 {
        let old = lock_forum_path(db, &mut tx, input.fid).await?;
        current_icon = old.try_get::<Option<i64>, _>("icon_aid")?;
        before = Some(crate::content::copies::ForumFields {
            name: old.try_get("name")?,
            slug: old.try_get("slug")?,
            visibility: db::text(&old, "visibility")?,
            status: old.try_get("status")?,
            parent_fid: old.try_get("parent_fid")?,
        });
        if old.try_get::<i64, _>("version")? != input.version {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().forum_error_stale(),
            ));
        }
        if input.parent_fid > 0
            && db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_forum_forum WHERE parent_fid=?")
                .bind(input.fid)
                .fetch_one(db.observe(&mut *tx))
                .await?
                > 0
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_has_children(),
            ));
        }
    }
    let icon = (input.icon_aid > 0).then_some(input.icon_aid);
    for aid in current_icon
        .into_iter()
        .chain(icon)
        .collect::<BTreeSet<_>>()
    {
        db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(db.observe(&mut *tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
    }
    if input.icon_aid > 0 {
        let image=db::query("SELECT d.lifecycle,d.scan_status,d.mime_type,d.purge_after,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? AND (a.uid=? OR a.aid=?) FOR UPDATE")
            .bind(input.icon_aid).bind(actor.uid).bind(current_icon).fetch_optional(db.observe(&mut *tx)).await?.ok_or(ContentWriteError::NotFound)?;
        if db::text(&image, "lifecycle")? != "ready"
            || db::text(&image, "scan_status")? != "clean"
            || !db::text(&image, "mime_type")?.starts_with("image/")
            || (current_icon != icon
                && image
                    .try_get::<Option<i64>, _>("purge_after")?
                    .is_some_and(|expiry| expiry <= identity::now()))
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_icon(),
            ));
        }
        if image.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_icon_trashed(),
            ));
        }
    }
    let parent = (input.parent_fid > 0).then_some(input.parent_fid);
    // Multi-domain sites: when the same host has registered the root path, the forum slug must avoid the first segment
    // of its sub-paths.
    // This also takes a shared lock on the domain rows (lock order: forum row → domain row).
    if !crate::domains::write::check_forum_slug(db, &mut tx, &slug).await? {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_domain_path(),
        ));
    }
    let fid = if input.fid == 0 {
        db::query("INSERT INTO gonof_forum_forum(name,slug,description,guidance,displayorder,status,visibility,parent_fid,icon_aid) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(name).bind(&slug).bind(&input.description).bind(&input.guidance).bind(input.displayorder).bind(input.status).bind(&input.visibility).bind(parent).bind(icon).execute(db.observe(&mut *tx)).await.map_err(namespace_error)?.last_insert_id() as i64
    } else {
        db::query("UPDATE gonof_forum_forum SET name=?,slug=?,description=?,guidance=?,displayorder=?,status=?,visibility=?,parent_fid=?,icon_aid=?,version=version+1 WHERE fid=? AND version=?")
            .bind(name).bind(&slug).bind(&input.description).bind(&input.guidance).bind(input.displayorder).bind(input.status).bind(&input.visibility).bind(parent).bind(icon).bind(input.fid).bind(input.version).execute(db.observe(&mut *tx)).await.map_err(namespace_error)?;
        input.fid
    };
    // A newly created top-level forum is shown only on the main domain; one changed into a child forum is no longer
    // registered separately.
    crate::domains::write::forum_saved(db, &mut tx, fid, parent, input.fid == 0).await?;
    // The site-wide version is bumped only when the name, slug, visibility, enabled state or parent forum changed; when
    // the parent forum changed, the top-level forum ID of this forum's threads is also rewritten (the thread row holds
    // no other forum copies); a new forum has no threads yet.
    if let Some(before) = &before {
        let after = crate::content::copies::ForumFields {
            name: name.to_owned(),
            slug: slug.clone(),
            visibility: input.visibility.clone(),
            status: input.status,
            parent_fid: parent,
        };
        crate::content::copies::forum_saved(db, &mut tx, fid, before, &after).await?;
    }
    if let Some(aid) = icon {
        db::query("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=?")
            .bind(aid)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    if current_icon != icon
        && let Some(aid) = current_icon
    {
        release_icon(db, &mut tx, aid, orphan_seconds).await?;
    }
    audit(
        db,
        &mut tx,
        actor,
        "organization.forum.save",
        serde_json::json!({"fid":fid}),
    )
    .await?;
    tx.commit().await?;
    Ok(fid)
}

/// Arranged in parent-child groups (for the layered dropdowns and lists everywhere):
/// top-level forums keep their original order, each immediately followed by its child forums (also in order); child
/// forums whose parent is not in the list go last. Returns each item and "whether it is a child forum". Sorting is done
/// in code on the data already fetched, with no extra SQL.
pub fn grouped_by_parent<T>(
    items: &[T],
    key: impl Fn(&T) -> (i64, Option<i64>),
) -> Vec<(&T, bool)> {
    let mut rows = Vec::with_capacity(items.len());
    for parent in items.iter().filter(|item| key(item).1.is_none()) {
        rows.push((parent, false));
        let fid = key(parent).0;
        rows.extend(
            items
                .iter()
                .filter(|item| key(item).1 == Some(fid))
                .map(|child| (child, true)),
        );
    }
    rows.extend(
        items
            .iter()
            .filter(|item| {
                key(item)
                    .1
                    .is_some_and(|parent| !items.iter().any(|p| key(p).0 == parent))
            })
            .map(|child| (child, true)),
    );
    rows
}

/// Indent prefix before child forum names in layered dropdowns (parent forums can also take posts and be move targets,
/// so `<optgroup>` is not used).
pub const CHILD_PREFIX: &str = "└ ";

/// One row of the admin batch reordering: forum, new sort number, and the version carried in the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForumOrder {
    pub fid: i64,
    pub displayorder: i64,
    pub version: i64,
}

/// Changes the sort numbers of several forums at once from the admin list. Administrators only;
/// exclusively locks these forums' paths in ascending FID order (the same order as posting's "forum paths in ascending
/// FID, shared locks"), checks versions row by row, and writes only rows whose sort number really changed (version +1);
/// sort order is not part of the thread copies, so threads are not rewritten and the site-wide version is not bumped.
/// Records one `organization.forum.order`. Returns how many rows changed.
pub async fn reorder_forums(
    db: &Database,
    actor: Actor,
    orders: &[ForumOrder],
    why: &str,
) -> Result<usize> {
    retry_transaction!(reorder_forums_inner(db, actor, orders, why))
}
async fn reorder_forums_inner(
    db: &Database,
    actor: Actor,
    orders: &[ForumOrder],
    why: &str,
) -> Result<usize> {
    let why = reason(why)?;
    let mut fids: Vec<i64> = orders.iter().map(|order| order.fid).collect();
    fids.sort_unstable();
    fids.dedup();
    if orders.is_empty()
        || fids.len() != orders.len()
        || orders
            .iter()
            .any(|order| order.fid <= 0 || order.version <= 0)
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_order_form(),
        ));
    }
    let mut tx = db.begin_content_write().await?;
    if role_locked(db, &mut tx, actor).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().forum_error_admin_only(),
        ));
    }
    lock_forum_paths(db, &mut tx, &fids).await?;
    let mut changed = Vec::new();
    for order in orders {
        let row = db::query("SELECT displayorder,version FROM gonof_forum_forum WHERE fid=?")
            .bind(order.fid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
        if row.try_get::<i64, _>("version")? != order.version {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().forum_error_stale(),
            ));
        }
        if row.try_get::<i64, _>("displayorder")? != order.displayorder {
            db::query("UPDATE gonof_forum_forum SET displayorder=?,version=version+1 WHERE fid=? AND version=?")
                .bind(order.displayorder)
                .bind(order.fid)
                .bind(order.version)
                .execute(db.observe(&mut *tx))
                .await?;
            changed.push(order.fid);
        }
    }
    audit(
        db,
        &mut tx,
        actor,
        "organization.forum.order",
        serde_json::json!({"fids": changed, "reason": why}),
    )
    .await?;
    tx.commit().await?;
    Ok(changed.len())
}

/// For the edit page hint: the number of this forum's threads whose copies a save rewrites in the same transaction,
/// which only a new parent does (`content::copies::forum_saved`; a forum with child forums cannot be moved, so their
/// threads are never part of it). Counted via the `fid` foreign key index.
pub async fn rewrite_count(db: &Database, fid: i64) -> Result<i64> {
    Ok(
        db::query_scalar("SELECT COUNT(*) FROM gonof_forum_thread WHERE fid=?")
            .bind(fid)
            .fetch_one(db.executor())
            .await?,
    )
}

/// Icon attachment replaced, or deleted along with the forum: when nothing references it any more it enters the
/// retention period, and media cleanup reclaims it on expiry. The caller has already locked the attachment row.
async fn release_icon(db: &Database, tx: &mut Tx<'_>, aid: i64, orphan_seconds: i64) -> Result<()> {
    let mut references = 0_i64;
    for (table, column) in [
        ("gonof_forum_attachment_reference", "aid"),
        ("gonof_common_member", "avatar_aid"),
        ("gonof_common_site_asset", "aid"),
        ("gonof_forum_forum", "icon_aid"),
        ("gonof_common_site_domain", "logo_aid"),
        ("gonof_common_site_domain", "favicon_aid"),
    ] {
        references +=
            db::query_scalar::<i64>(&format!("SELECT COUNT(*) FROM {table} WHERE {column}=?"))
                .bind(aid)
                .fetch_one(db.observe(&mut **tx))
                .await?;
    }
    if references == 0 {
        let now = identity::now();
        db::query("UPDATE gonof_forum_attachment_0 SET purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle NOT IN ('purging','purged')").bind(now.saturating_add(orphan_seconds)).bind(now).bind(aid).execute(db.observe(&mut **tx)).await?;
    }
    Ok(())
}

// ---- Deleting and merging forums -----------------------------------------------------------------------------------

/// Event type of the forum merge job in the outbox; `aggregate_type` is `forum` and `aggregate_id` is the source forum.
pub const MERGE_EVENT: &str = "forum.merge";
/// Each merge batch moves at most this many threads (one transaction, one site-wide version).
pub const MERGE_BATCH: i64 = 100;
/// The merge job runs at most this many batches per round, then yields to the next round.
const MERGE_BATCHES_PER_ROUND: usize = 5;
/// Lease of the merge job: a batch moves 100 threads with the cascaded forum IDs of their posts, with ample margin.
const MERGE_LEASE_SECONDS: i64 = 300;

/// Parameters of the merge job, stored in the outbox `payload`.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct MergeTask {
    pub source: i64,
    pub target: i64,
    /// The administrator who started the merge.
    pub uid: i64,
    pub reason: String,
}

/// Merge jobs in progress (`pending`, `processing`). Forced onto `gonof_outbox_delivery`: takes a range by status,
/// reading only those in-progress rows without scanning events already sent within the retention period.
async fn active_merges(db: &Database, tx: &mut Tx<'_>) -> Result<Vec<(i64, MergeTask)>> {
    let rows = db::query("SELECT eventid,payload FROM gonof_common_outbox FORCE INDEX (gonof_outbox_delivery) WHERE status IN ('pending','processing') AND event_type=?")
        .bind(MERGE_EVENT)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    rows.iter()
        .map(|row| {
            let task: MergeTask = serde_json::from_str(&db::text(row, "payload")?)
                .map_err(|error| ContentWriteError::Internal(error.into()))?;
            Ok((row.try_get("eventid")?, task))
        })
        .collect()
}

/// For the admin forum page: the in-progress merge with this forum as source (target forum ID, job ID).
pub async fn pending_merge(db: &Database, fid: i64) -> Result<Option<(i64, i64)>> {
    let row = db::query("SELECT eventid,payload FROM gonof_common_outbox FORCE INDEX (gonof_outbox_delivery) WHERE status IN ('pending','processing') AND event_type=? AND aggregate_id=? LIMIT 1")
        .bind(MERGE_EVENT)
        .bind(fid)
        .fetch_optional(db.executor())
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let task: MergeTask = serde_json::from_str(&db::text(&row, "payload")?)
        .map_err(|error| ContentWriteError::Internal(error.into()))?;
    Ok(Some((task.target, row.try_get("eventid")?)))
}

/// Deletes an empty forum: no threads, no child forums, not the target of an in-progress merge job. The same
/// transaction deletes its moderator appointments, member follows and personal filters; the icon attachment enters the
/// retention period when nothing else references it; items in the navigation document referencing it are dropped
/// automatically at render time. Only administrators can delete.
///
/// Moderators' roles are left alone: a moderator who managed only this forum remains a "Forum moderator" after deletion
/// with no authorized forums, and the administrator can handle that separately on the appointment page. Authorization
/// is always queried live, so their sign-in does not need to be invalidated.
pub async fn delete_forum(
    db: &Database,
    actor: Actor,
    fid: i64,
    version: i64,
    why: &str,
    orphan_seconds: i64,
) -> Result<()> {
    retry_transaction!(delete_forum_inner(
        db,
        actor,
        fid,
        version,
        why,
        orphan_seconds
    ))
}
async fn delete_forum_inner(
    db: &Database,
    actor: Actor,
    fid: i64,
    version: i64,
    why: &str,
    orphan_seconds: i64,
) -> Result<()> {
    let why = reason(why)?;
    if fid <= 0 || version <= 0 || orphan_seconds <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_forum_id(),
        ));
    }
    let mut tx = db.begin_content_write().await?;
    if role_locked(db, &mut tx, actor).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().forum_error_delete_admin(),
        ));
    }
    let forum = lock_forum_path(db, &mut tx, fid).await?;
    if forum.try_get::<i64, _>("version")? != version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().forum_error_stale(),
        ));
    }
    let children: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_forum_forum WHERE parent_fid=?")
            .bind(fid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    if children > 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_delete_children(),
        ));
    }
    let thread: Option<i64> =
        db::query_scalar("SELECT tid FROM gonof_forum_thread WHERE fid=? LIMIT 1")
            .bind(fid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    if thread.is_some() {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_delete_threads(),
        ));
    }
    if active_merges(db, &mut tx)
        .await?
        .iter()
        .any(|(_, task)| task.target == fid)
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_delete_merge_target(),
        ));
    }
    let icon: Option<i64> = forum.try_get("icon_aid")?;
    if let Some(aid) = icon {
        db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    }
    // Association rows of multi-domain sites (the foreign key has no ON DELETE).
    crate::domains::write::forum_deleted(db, &mut tx, fid).await?;
    for sql in [
        "DELETE FROM gonof_forum_moderator WHERE fid=?",
        "DELETE FROM gonof_home_forum_follow WHERE fid=?",
        "DELETE FROM gonof_home_forum_filter WHERE fid=?",
        // Forum thread counts: an empty forum's count rows are all 0 and are deleted with the forum.
        "DELETE FROM gonof_forum_thread_count WHERE fid=?",
    ] {
        db::query(sql)
            .bind(fid)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    let deleted = db::query("DELETE FROM gonof_forum_forum WHERE fid=? AND version=?")
        .bind(fid)
        .bind(version)
        .execute(db.observe(&mut *tx))
        .await?
        .rows_affected();
    if deleted != 1 {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().forum_error_stale(),
        ));
    }
    if let Some(aid) = icon {
        release_icon(db, &mut tx, aid, orphan_seconds).await?;
    }
    audit(
        db,
        &mut tx,
        actor,
        "organization.forum.delete",
        serde_json::json!({
            "fid": fid,
            "name": db::text(&forum, "name")?,
            "slug": db::text(&forum, "slug")?,
            "reason": why,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Merges all threads of `source` into `target`: queued as a background job (outbox event [`MERGE_EVENT`]) that the
/// worker moves in batches, disabling the source forum once done ([`process_merges`]). Returns the job ID.
///
/// The source forum must not have child forums (move them elsewhere first), one source forum has only one merge job at
/// a time, and the target cannot be a forum that is being merged away. Thread IDs, post IDs and links are all kept;
/// authors do not receive individual move notifications, and the action log records one start and one completion.
pub async fn request_merge(
    db: &Database,
    actor: Actor,
    source: i64,
    target: i64,
    version: i64,
    why: &str,
) -> Result<i64> {
    retry_transaction!(request_merge_inner(db, actor, source, target, version, why))
}
async fn request_merge_inner(
    db: &Database,
    actor: Actor,
    source: i64,
    target: i64,
    version: i64,
    why: &str,
) -> Result<i64> {
    let why = reason(why)?;
    if source <= 0 || version <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_forum_id(),
        ));
    }
    if target <= 0 || target == source {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_merge_target(),
        ));
    }
    let mut tx = db.begin_content_write().await?;
    if role_locked(db, &mut tx, actor).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().forum_error_merge_admin(),
        ));
    }
    lock_forum_paths(db, &mut tx, &[source, target]).await?;
    let current: i64 = db::query_scalar("SELECT version FROM gonof_forum_forum WHERE fid=?")
        .bind(source)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    if current != version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().forum_error_stale(),
        ));
    }
    let children: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_forum_forum WHERE parent_fid=?")
            .bind(source)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    if children > 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_error_merge_children(),
        ));
    }
    for (_, task) in active_merges(db, &mut tx).await? {
        if task.source == source {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().forum_error_merge_running(),
            ));
        }
        if task.source == target {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().forum_error_merge_target_busy(),
            ));
        }
    }
    let task = MergeTask {
        source,
        target,
        uid: actor.uid,
        reason: why.to_owned(),
    };
    let now = identity::now();
    let eventid = db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'forum',?,?,?,?)")
        .bind(format!("{MERGE_EVENT}:{source}:{}", identity::random_token()))
        .bind(MERGE_EVENT)
        .bind(source)
        .bind(serde_json::to_string(&task).map_err(|error| ContentWriteError::Internal(error.into()))?)
        .bind(now)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?
        .last_insert_id() as i64;
    audit(
        db,
        &mut tx,
        actor,
        "organization.forum.merge",
        serde_json::json!({"fid": source, "to": target, "eventid": eventid, "reason": why}),
    )
    .await?;
    tx.commit().await?;
    Ok(eventid)
}

/// Result of one round of the merge worker.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Number of threads moved this round.
    pub moved: u64,
    /// Number of jobs completed this round (source forum disabled).
    pub finished: u64,
}

/// Background job: claims one merge job and runs at most [`MERGE_BATCHES_PER_ROUND`] batches, each one transaction of at
/// most [`MERGE_BATCH`] threads, moved to the target forum with the existing move logic (`copies::move_thread`; posts'
/// forum IDs cascade through the foreign key), bumping the site-wide version once per batch. When the source forum is
/// empty it is disabled (rewriting child forum copies, bumping the site-wide version) and marked done. If it fails
/// midway or the process exits, the next round continues with the remaining threads; those already moved are not
/// repeated.
///
/// After each batch commits, this process's pin index is cleared and broadcast (moved second-level pins changed
/// "home"); after the merge completes and the source forum is disabled, the forum cache is cleared too. A broadcast
/// failure is only logged; the data is committed, and other processes see it after at most one cache lifetime.
pub async fn process_merges(
    db: &Database,
    global: &crate::cache::GlobalCache,
    now: i64,
) -> anyhow::Result<MergeReport> {
    let mut report = MergeReport::default();
    let Some((eventid, lease, attempts, task)) = claim_merge(db, now).await? else {
        return Ok(report);
    };
    for _ in 0..MERGE_BATCHES_PER_ROUND {
        match merge_batch(db, eventid, &lease, &task, now).await {
            Ok(MergeStep::Moved(moved)) => {
                report.moved += moved;
                announce_merge(global, &[crate::cache::Block::Pins]).await;
            }
            Ok(MergeStep::Finished(moved)) => {
                report.moved += moved;
                report.finished += 1;
                announce_merge(
                    global,
                    &[crate::cache::Block::Pins, crate::cache::Block::Forums],
                )
                .await;
                return Ok(report);
            }
            Ok(MergeStep::Lost) => return Ok(report),
            Err(error) => {
                let description: String = format!("{error:#}").chars().take(1000).collect();
                tracing::warn!(eventid, attempts, error = %description, "forum merge failed, will retry");
                let (status, delay, message) = if attempts >= crate::outbox::MAX_ATTEMPTS {
                    (
                        "failed",
                        0,
                        crate::outbox::exhausted_error(attempts, &description),
                    )
                } else {
                    (
                        "pending",
                        5_i64.saturating_mul(1_i64 << attempts.min(9)).min(3600),
                        description,
                    )
                };
                let sql = if status == "failed" {
                    "UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?"
                } else {
                    "UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?"
                };
                let mut query = db::query(sql);
                if status == "pending" {
                    query = query.bind(now.saturating_add(delay));
                }
                query
                    .bind(message)
                    .bind(eventid)
                    .bind(&lease)
                    .execute(db.executor())
                    .await?;
                return Ok(report);
            }
        }
    }
    // Not finished moving: put it back in the queue to continue next round. Any progress resets the consecutive failure
    // count to zero.
    db::query("UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),attempts=0,lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?")
        .bind(now)
        .bind(eventid)
        .bind(&lease)
        .execute(db.executor())
        .await?;
    Ok(report)
}

async fn announce_merge(global: &crate::cache::GlobalCache, blocks: &[crate::cache::Block]) {
    if let Err(error) = global.changed(blocks).await {
        tracing::warn!(%error, "merge committed; process cache invalidation broadcast failed");
    }
}

async fn claim_merge(
    db: &Database,
    now: i64,
) -> anyhow::Result<Option<(i64, String, i64, MergeTask)>> {
    let mut tx = db.begin_write().await?;
    let row = db::query("SELECT eventid,payload,attempts FROM gonof_common_outbox FORCE INDEX (gonof_outbox_delivery) WHERE status IN ('pending','processing') AND event_type=? AND available_at<=? AND lease_until<=? ORDER BY eventid LIMIT 1 FOR UPDATE")
        .bind(MERGE_EVENT)
        .bind(now)
        .bind(now)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let eventid: i64 = row.try_get("eventid")?;
    let lease = identity::random_token();
    let attempts = row.try_get::<i64, _>("attempts")?.saturating_add(1);
    let task = serde_json::from_str::<MergeTask>(&db::text(&row, "payload")?);
    let Ok(task) = task else {
        db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=?")
            .bind("forum merge task payload is malformed")
            .bind(eventid)
            .execute(db.observe(&mut *tx))
            .await?;
        tx.commit().await?;
        return Ok(None);
    };
    db::query("UPDATE gonof_common_outbox SET status='processing',attempts=attempts+1,lease_token=?,lease_until=? WHERE eventid=?")
        .bind(&lease)
        .bind(now.saturating_add(MERGE_LEASE_SECONDS))
        .bind(eventid)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(Some((eventid, lease, attempts, task)))
}

enum MergeStep {
    /// Moved a full batch, and there are more left.
    Moved(u64),
    /// Finished moving (including a final partial batch); the source forum is disabled and the job marked done.
    Finished(u64),
    /// The lease no longer belongs to this round (timed out and claimed by another process); nothing was done.
    Lost,
}

async fn merge_batch(
    db: &Database,
    eventid: i64,
    lease: &str,
    task: &MergeTask,
    now: i64,
) -> anyhow::Result<MergeStep> {
    let mut tx = db.begin_content_write().await?;
    let current = db::query("SELECT lease_token FROM gonof_common_outbox WHERE eventid=? AND status='processing' FOR UPDATE")
        .bind(eventid)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
    if current
        .map(|row| row.try_get::<Option<String>, _>("lease_token"))
        .transpose()?
        .flatten()
        .as_deref()
        != Some(lease)
    {
        return Ok(MergeStep::Lost);
    }
    let exists = |fid: i64| {
        db::query_scalar::<i64>("SELECT fid FROM gonof_forum_forum WHERE fid=?").bind(fid)
    };
    let source_exists = exists(task.source)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .is_some();
    if !source_exists {
        // The source forum is already empty and has been deleted: nothing to move, counts as done.
        finish_merge(db, &mut tx, eventid, task, 0, false).await?;
        tx.commit().await?;
        return Ok(MergeStep::Finished(0));
    }
    if exists(task.target)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .is_none()
    {
        db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?")
            .bind("the target forum no longer exists; the merge task stopped")
            .bind(eventid)
            .bind(lease)
            .execute(db.observe(&mut *tx))
            .await?;
        tx.commit().await?;
        return Ok(MergeStep::Lost);
    }
    lock_forum_paths(db, &mut tx, &[task.source, task.target])
        .await
        .map_err(|error| anyhow::anyhow!("locking the forums failed: {error}"))?;
    let rows = db::query(&format!(
        "SELECT tid,version FROM gonof_forum_thread WHERE fid=? ORDER BY tid LIMIT {MERGE_BATCH} FOR UPDATE"
    ))
    .bind(task.source)
    .fetch_all(db.observe(&mut *tx))
    .await?;
    for row in &rows {
        crate::content::copies::move_thread(
            db,
            &mut tx,
            row.try_get("tid")?,
            row.try_get("version")?,
            task.target,
        )
        .await?;
    }
    let moved = rows.len() as u64;
    if moved == MERGE_BATCH as u64 {
        crate::content::copies::bump_site(db, &mut tx).await?;
        // Renew the lease: the next batch still runs in this round.
        db::query("UPDATE gonof_common_outbox SET lease_until=? WHERE eventid=? AND lease_token=?")
            .bind(now.saturating_add(MERGE_LEASE_SECONDS))
            .bind(eventid)
            .bind(lease)
            .execute(db.observe(&mut *tx))
            .await?;
        tx.commit().await?;
        return Ok(MergeStep::Moved(moved));
    }
    finish_merge(db, &mut tx, eventid, task, moved, true).await?;
    tx.commit().await?;
    Ok(MergeStep::Finished(moved))
}

/// The source forum has no threads left: disables it and marks the job done. The caller has already exclusively locked
/// the source and target forum paths.
async fn finish_merge(
    db: &Database,
    tx: &mut Tx<'_>,
    eventid: i64,
    task: &MergeTask,
    moved: u64,
    source_exists: bool,
) -> anyhow::Result<()> {
    if source_exists {
        let status: i64 = db::query_scalar("SELECT status FROM gonof_forum_forum WHERE fid=?")
            .bind(task.source)
            .fetch_one(db.observe(&mut **tx))
            .await?;
        if status != 0 {
            db::query("UPDATE gonof_forum_forum SET status=0,version=version+1 WHERE fid=?")
                .bind(task.source)
                .execute(db.observe(&mut **tx))
                .await?;
            // If someone added child forums to the source forum during the merge, disabling makes them unreadable too:
            // the read path judges by the process forum cache, so thread rows need not be rewritten;
            // bumping the site-wide version makes caches everywhere recompute.
            crate::content::copies::bump_site(db, tx).await?;
        } else if moved > 0 {
            crate::content::copies::bump_site(db, tx).await?;
        }
    }
    db::query("UPDATE gonof_common_outbox SET status='sent',lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing'")
        .bind(eventid)
        .execute(db.observe(&mut **tx))
        .await?;
    db::query(
        "INSERT INTO gonof_common_log(operationuid,type,data,source,dateline) VALUES(?,?,?,'web',?)",
    )
    .bind(task.uid)
    .bind("organization.forum.merge.done")
    .bind(
        serde_json::json!({"fid": task.source, "to": task.target, "eventid": eventid, "reason": task.reason})
            .to_string(),
    )
    .bind(identity::now())
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

pub async fn move_thread(
    db: &Database,
    actor: Actor,
    tid: i64,
    target_fid: i64,
    version: i64,
    key: &str,
    why: &str,
) -> Result<()> {
    retry_transaction!(move_thread_inner(
        db, actor, tid, target_fid, version, key, why
    ))
}
async fn move_thread_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    target_fid: i64,
    version: i64,
    key: &str,
    why: &str,
) -> Result<()> {
    let why = reason(why)?;
    if tid <= 0 || target_fid <= 0 || !identity::valid_request_key(key) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().forum_move_error_target(),
        ));
    }
    let hash=identity::token_hash(&serde_json::json!({"operation":"thread.move","tid":tid,"fid":target_fid,"version":version,"reason":why}).to_string());
    let owner: Option<i64> =
        db::query_scalar("SELECT authorid FROM gonof_forum_thread WHERE tid=?")
            .bind(tid)
            .fetch_optional(db.executor())
            .await?
            .ok_or(ContentWriteError::NotFound)?;
    let mut tx = db.begin_content_write().await?;
    let mut accounts = vec![actor.uid];
    accounts.extend(owner);
    crate::activities::lock_members(db, &mut tx, &accounts).await?;
    let role = role_locked(db, &mut tx, actor).await?;
    if let Some(previous) = db::query_scalar::<String>(
        "SELECT request_hash FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        if previous != hash {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().forum_error_request_key(),
            ));
        }
        tx.commit().await?;
        return Ok(());
    }
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    lock_forum_paths(db, &mut tx, &[fid, target_fid]).await?;
    for id in BTreeSet::from([fid, target_fid]) {
        let forum = lock_forum_path(db, &mut tx, id).await?;
        require_scope(db, &mut tx, actor, role, &forum).await?;
    }
    let row=db::query("SELECT t.authorid,t.fid,t.version,p.pid FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid WHERE t.tid=? FOR UPDATE").bind(tid).fetch_one(db.observe(&mut *tx)).await?;
    if row.try_get::<i64, _>("fid")? != fid
        || row.try_get::<i64, _>("version")? != version
        || row.try_get::<Option<i64>, _>("authorid")? != owner
    {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().forum_move_error_stale(),
        ));
    }
    let pid: i64 = row.try_get("pid")?;
    if fid != target_fid {
        // The same statement writes the target forum's copies (name, slug, inherited visibility and enabled state), and
        // the posts' fid is rewritten by the foreign key cascade.
        crate::content::copies::move_thread(db, &mut tx, tid, version, target_fid).await?;
        if let Some(uid) = owner.filter(|uid| *uid != actor.uid) {
            crate::moderation::notify(
                db,
                &mut tx,
                crate::moderation::Notice {
                    uid,
                    authorid: actor.uid,
                    kind: crate::notifications::record::Kind::Moderation,
                    target: crate::notifications::record::Target::Post(pid),
                    key: &format!("thread.move:{tid}:{key}"),
                    // Written in the site language (`/manage` is only served on the main domain); the notification
                    // list shows the stored sentence.
                    reason: &crate::i18n::current().forum_notice_thread_moved(why),
                    now: identity::now(),
                },
            )
            .await?;
        }
    }
    identity::rules::insert_post_request(db, &mut tx, actor.uid, key, &hash, pid, identity::now())
        .await?;
    audit(
        db,
        &mut tx,
        actor,
        "content.thread.move",
        serde_json::json!({"tid":tid,"fid":fid,"from":fid,"to":target_fid,"reason":why}),
    )
    .await?;
    if fid != target_fid {
        crate::content::copies::bump_site(db, &mut tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forum_fields_keep_the_value_rules_the_triggers_had() {
        assert_eq!(
            forum_fields(" News ", " News-Desk ", "members", 0),
            Some(("News".into(), "news-desk".into()))
        );
        // The name limit counts characters: "\u{540d}" (the Chinese for "name") takes three bytes in UTF-8.
        assert!(forum_fields(&"\u{540d}".repeat(50), "a", "private", 1).is_some());
        for (name, slug, visibility, status) in [
            ("", "a", "public", 1),
            (&*"\u{540d}".repeat(51), "a", "public", 1),
            ("Name", "", "public", 1),
            ("Name", &*"a".repeat(65), "public", 1),
            ("Name", "a b", "public", 1),
            ("Name", "a_b", "public", 1),
            ("Name", "news-2", "public", 1),
            ("Name", "a", "Public", 1),
            ("Name", "a", "public ", 1),
            ("Name", "a", "invalid", 1),
            ("Name", "a", "public", 2),
            ("Name", "a", "public", -1),
        ] {
            assert_eq!(
                forum_fields(name, slug, visibility, status),
                None,
                "{name:?} {slug:?} {visibility:?} {status}"
            );
        }
    }
}
