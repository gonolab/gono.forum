//! Growth points: a reputation value separate from credits.
//!
//! Growth points come only from content contributions and cannot be spent, transferred or redeemed. Each post that earns
//! growth points has a row in `gonof_common_growth_post` recording the points fixed at posting time and how much is
//! currently credited; every operation that can change it (posting, liking and unliking, hiding and unhiding,
//! withdrawing and restoring, highlighting, accepting, erasing) calls [`sync_posts`] afterwards: it computes the points
//! due from the post's current state and writes nothing if that equals what is already credited; only when they differ
//! does it write an immutable ledger entry and update the account. Request replays and duplicate submissions naturally
//! never double-book, and the ledger's unique key is the final safeguard.
//!
//! Lock order: the business's own locks (member rows, forum, thread, posts) come first, then each post's growth row in
//! pid order, and finally the growth accounts in uid order. The member rows include the authors whose points change:
//! the foreign keys of their ledger entries and accounts share-lock their member rows, which must not be locked first
//! after the board path (a like or downvote share-locks the post's author with the member's own row,
//! `interaction::lock_actor_and_author`). A like transaction locks the post's growth row before writing the like
//! relation (see [`lock_post`]).
//!
//! The likes a post earns from are its like counter (`gonof_forum_post.reactions`, docs/queries.md, "Counter
//! columns"), which the like transaction moves together with the like rows, less the author's own like found by
//! primary key: the cost of a like, or of any other change that recomputes the points, does not grow with the post's
//! number of likes.
use crate::db::Tx;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use serde::Serialize;

use crate::db::Database;

/// Upper limit of a single change and of admin adjustments, far below BIGINT, so totals cannot overflow.
pub const MAX_POINTS: i64 = 1_000_000;

/// Growth point values and levels come from the settings `settings.growth.*` (the growth section of
/// "Global settings › Points" in the admin area). Points are written into the post's
/// growth row at posting time, so changing the settings affects only later posts; levels are computed on read, so after
/// a change everyone is shown against the new thresholds immediately. The value rules (first threshold 0, strictly
/// increasing, as many names as thresholds) are already validated when decoding in `runtime::settings`.
impl crate::runtime::GrowthSettings {
    /// Determines the level from growth points. Negative values (which only occur when points are taken back after an
    /// admin deduction) count as the first level.
    pub fn level(&self, growth: i64) -> Level {
        let index = self
            .levels
            .iter()
            .rposition(|floor| growth >= *floor)
            .unwrap_or(0);
        let floor = self.levels[index];
        let next = self.levels.get(index + 1).copied();
        Level {
            number: index + 1,
            name: self.level_name(index),
            growth,
            floor,
            next,
        }
    }

    /// The name of the level at `index` (0 is level 1): the configured name, or the language's default when the
    /// names are left empty (docs/architecture.md, "Languages"). `None` beyond the number of levels.
    pub fn name_of(&self, index: usize) -> Option<String> {
        if index >= self.levels.len() {
            return None;
        }
        Some(match self.level_names.get(index) {
            Some(name) => name.clone(),
            None => crate::i18n::current().brand_level_name(index + 1),
        })
    }

    fn level_name(&self, index: usize) -> String {
        self.name_of(index).unwrap_or_default()
    }

    /// How the level is phrased, e.g. "Lv3 (Level 3)"; beyond the number of levels only the level number is written
    /// (already blocked when saving settings; this only avoids going out of bounds).
    pub fn level_label(&self, number: i64) -> String {
        match usize::try_from(number - 1)
            .ok()
            .and_then(|index| self.name_of(index))
        {
            Some(name) => format!(
                "Lv{number}{}",
                crate::i18n::current().common_parenthesized(name)
            ),
            None => format!("Lv{number}"),
        }
    }

    fn points(&self, first: bool) -> Points {
        Points {
            base: if first { self.thread } else { self.reply },
            like: self.like,
            digest: if first { self.digest } else { 0 },
            accept: if first { 0 } else { self.accept },
        }
    }
}

/// The level a given growth value falls in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Level {
    /// Starts at 1.
    pub number: usize,
    pub name: String,
    pub growth: i64,
    /// This level's threshold.
    pub floor: i64,
    /// The next level's threshold; empty at the top level.
    pub next: Option<i64>,
}

impl Level {
    /// How much is still needed for the next level; 0 at the top level.
    pub fn remaining(&self) -> i64 {
        self.next.map_or(0, |next| (next - self.growth).max(0))
    }

    /// Progress within this level, 0–100.
    pub fn percent(&self) -> i64 {
        match self.next {
            Some(next) => ((self.growth - self.floor).max(0) * 100 / (next - self.floor)).min(100),
            None => 100,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Points {
    base: i64,
    like: i64,
    digest: i64,
    accept: i64,
}

/// The reason for a growth change, written into the ledger's `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Publish,
    Like,
    Unlike,
    Digest,
    Undigest,
    Accept,
    /// Taken back on hiding, withdrawal or erasure.
    Revoke,
    /// Given back on unhiding or restoring.
    Restore,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Like => "like",
            Self::Unlike => "unlike",
            Self::Digest => "digest",
            Self::Undigest => "undigest",
            Self::Accept => "accept",
            Self::Revoke => "revoke",
            Self::Restore => "restore",
        }
    }
}

/// Chinese label for a ledger `kind`; unknown values are shown as is.
pub fn kind_label(kind: &str) -> &str {
    match kind {
        "publish" => crate::i18n::current().activity_growth_publish(),
        "like" => crate::i18n::current().activity_growth_like(),
        "unlike" => crate::i18n::current().activity_growth_unlike(),
        "digest" => crate::i18n::current().activity_growth_digest(),
        "undigest" => crate::i18n::current().activity_growth_undigest(),
        "accept" => crate::i18n::current().activity_growth_accept(),
        "revoke" => crate::i18n::current().activity_growth_revoke(),
        "restore" => crate::i18n::current().activity_growth_restore(),
        "adjust" => crate::i18n::current().activity_ledger_adjust(),
        other => other,
    }
}

/// Called in the posting transaction: creates the new post's growth row and credits the base points.
///
/// Posting is only possible in member-readable forums (the forum was already locked under the member-readable condition
/// at posting time), so every new post earns points.
#[allow(clippy::too_many_arguments)]
pub async fn record_post(
    db: &Database,
    tx: &mut Tx<'_>,
    config: &crate::runtime::GrowthSettings,
    uid: i64,
    pid: i64,
    first: bool,
    now: i64,
) -> Result<()> {
    let points = config.points(first);
    crate::db::query(
        "INSERT INTO gonof_common_growth_post(pid,uid,points_base,points_like,points_digest,points_accept,dateline) VALUES(?,?,?,?,?,?,?)",
    )
    .bind(pid)
    .bind(uid)
    .bind(points.base)
    .bind(points.like)
    .bind(points.digest)
    .bind(points.accept)
    .bind(now)
    .execute(db.observe(&mut **tx))
    .await?;
    sync_posts(db, tx, &[pid], Cause::Publish, now).await
}

/// Called by the content import (`content::seed`) for each post it writes: a growth row without base points and with
/// nothing credited, so the import itself earns nothing, while likes, highlights and accepted answers the post gets
/// later count as usual ([`sync_posts`] skips posts without a row). A thread imported as featured leaves out the
/// highlight points too: the import's highlight is not one a moderator gave. No ledger entry or account is written.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_imported_post(
    db: &Database,
    tx: &mut Tx<'_>,
    config: &crate::runtime::GrowthSettings,
    uid: i64,
    pid: i64,
    first: bool,
    featured: bool,
    dateline: i64,
) -> Result<()> {
    let points = config.points(first);
    crate::db::query(
        "INSERT INTO gonof_common_growth_post(pid,uid,points_base,points_like,points_digest,points_accept,dateline) VALUES(?,?,?,?,?,?,?)",
    )
    .bind(pid)
    .bind(uid)
    .bind(0_i64)
    .bind(points.like)
    .bind(if featured { 0 } else { points.digest })
    .bind(points.accept)
    .bind(dateline)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// Locks this post's growth row and returns whether it exists. Liking and unliking must call it before writing the like
/// relation.
pub async fn lock_post(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<bool> {
    Ok(
        crate::db::query("SELECT pid FROM gonof_common_growth_post WHERE pid=? FOR UPDATE")
            .bind(pid)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .is_some(),
    )
}

struct Change {
    pid: i64,
    uid: i64,
    delta: i64,
    credited: i64,
    revision: i64,
}

/// Aligns these posts' growth points with their current state. Posts without a growth row (from before launch) are
/// skipped.
///
/// The caller has locked the post and thread rows; every read here is a current read, so a REPEATABLE READ like
/// transaction also sees the latest state.
pub async fn sync_posts(
    db: &Database,
    tx: &mut Tx<'_>,
    pids: &[i64],
    cause: Cause,
    now: i64,
) -> Result<()> {
    let mut changes = Vec::new();
    for pid in pids.iter().copied().collect::<BTreeSet<_>>() {
        let Some(row) = crate::db::query(
            "SELECT uid,points_base,points_like,points_digest,points_accept,credited,revision FROM gonof_common_growth_post WHERE pid=? FOR UPDATE",
        )
        .bind(pid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        else {
            continue;
        };
        let uid: i64 = row.try_get("uid")?;
        let credited: i64 = row.try_get("credited")?;
        let points = Points {
            base: row.try_get("points_base")?,
            like: row.try_get("points_like")?,
            digest: row.try_get("points_digest")?,
            accept: row.try_get("points_accept")?,
        };
        let target = target(db, tx, pid, uid, points).await?;
        if target != credited {
            changes.push(Change {
                pid,
                uid,
                delta: target - credited,
                credited: target,
                revision: row.try_get::<i64, _>("revision")? + 1,
            });
        }
    }
    if changes.is_empty() {
        return Ok(());
    }
    let uids: BTreeSet<i64> = changes.iter().map(|change| change.uid).collect();
    let mut balances = lock_accounts(db, tx, &uids).await?;
    for change in &changes {
        let balance = balances.get_mut(&change.uid).expect("account is locked");
        *balance = balance
            .checked_add(change.delta)
            .ok_or_else(|| anyhow::anyhow!("growth out of range"))?;
        insert_ledger(
            db,
            tx,
            &format!("post:{}:{}", change.pid, change.revision),
            change.uid,
            change.delta,
            *balance,
            cause.as_str(),
            Some(change.pid),
            None,
            "",
            now,
        )
        .await?;
        crate::db::query("UPDATE gonof_common_growth_post SET credited=?,revision=? WHERE pid=?")
            .bind(change.credited)
            .bind(change.revision)
            .bind(change.pid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    for uid in uids {
        let entries = changes.iter().filter(|change| change.uid == uid).count();
        crate::db::query(
            "UPDATE gonof_common_growth_account SET growth=?,version=version+? WHERE uid=?",
        )
        .bind(balances[&uid])
        .bind(i64::try_from(entries)?)
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    }
    Ok(())
}

/// The points this post should currently be credited: base points + likes × like points + highlight points + acceptance
/// points while it is valid, otherwise 0. Likes are the post's like counter less the author's own like, if any
/// (imported or from before liking one's own post was refused).
///
/// Only the post's own state matters: making the thread members-only, moving forums or hiding the parent
/// floor has no effect.
async fn target(
    db: &Database,
    tx: &mut Tx<'_>,
    pid: i64,
    author: i64,
    points: Points,
) -> Result<i64> {
    let post = crate::db::query(
        "SELECT p.tid,p.first,p.invisible,p.deleted_at,p.erased_at,p.reactions,t.digest FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=? LOCK IN SHARE MODE",
    )
    .bind(pid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    let active = post.try_get::<i64, _>("invisible")? == 0
        && post.try_get::<i64, _>("deleted_at")? == 0
        && post.try_get::<i64, _>("erased_at")? == 0;
    if !active {
        return Ok(0);
    }
    let first = post.try_get::<i64, _>("first")? == 1;
    let mut likes: i64 = post.try_get("reactions")?;
    if likes > 0
        && crate::db::query(
            "SELECT uid FROM gonof_forum_post_reaction WHERE pid=? AND uid=? LOCK IN SHARE MODE",
        )
        .bind(pid)
        .bind(author)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .is_some()
    {
        likes -= 1;
    }
    let mut total = points.base + likes * points.like;
    if first && post.try_get::<i64, _>("digest")? > 0 {
        total += points.digest;
    }
    if !first && points.accept > 0 {
        let accepted = crate::db::query(
            "SELECT tid FROM gonof_forum_reward WHERE tid=? AND accepted_pid=? LOCK IN SHARE MODE",
        )
        .bind(post.try_get::<i64, _>("tid")?)
        .bind(pid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .is_some();
        if accepted {
            total += points.accept;
        }
    }
    Ok(total)
}

/// Locks the growth accounts in uid order, creating any that are missing, and returns the current balances.
///
/// An existing account is locked with a locking read, which locks the account row alone. An INSERT, even one that
/// finds the account already there, checks the foreign key and so share-locks the member row, as the ledger entry
/// written next does ([`insert_ledger`]). Taken first here, after the board path and the thread row, that lock could
/// deadlock with writers that lock member rows before them; the callers hold it already (see "Lock order" above), and
/// the locking read does not depend on that. Only a member's first growth gets to the INSERT. Before it, the locking
/// read of the missing row takes a gap lock on MySQL, which can deadlock with another member's first growth in the
/// same gap: rare, and the callers retry deadlocks.
async fn lock_accounts(
    db: &Database,
    tx: &mut Tx<'_>,
    uids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, i64>> {
    const LOCK: &str = "SELECT growth FROM gonof_common_growth_account WHERE uid=? FOR UPDATE";
    let mut balances = BTreeMap::new();
    for &uid in uids {
        let existing: Option<i64> = crate::db::query_scalar(LOCK)
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
        let growth = match existing {
            Some(growth) => growth,
            None => {
                crate::db::query(
                    "INSERT INTO gonof_common_growth_account(uid) VALUES(?) ON DUPLICATE KEY UPDATE uid=uid",
                )
                .bind(uid)
                .execute(db.observe(&mut **tx))
                .await?;
                crate::db::query_scalar(LOCK)
                    .bind(uid)
                    .fetch_one(db.observe(&mut **tx))
                    .await?
            }
        };
        balances.insert(uid, growth);
    }
    Ok(balances)
}

#[allow(clippy::too_many_arguments)]
async fn insert_ledger(
    db: &Database,
    tx: &mut Tx<'_>,
    key: &str,
    uid: i64,
    delta: i64,
    growth_after: i64,
    kind: &str,
    pid: Option<i64>,
    operator: Option<i64>,
    reason: &str,
    now: i64,
) -> Result<()> {
    crate::db::query("INSERT INTO gonof_common_growth_ledger(event_key,uid,delta,growth_after,kind,pid,operator_uid,reason,dateline) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(key)
        .bind(uid)
        .bind(delta)
        .bind(growth_after)
        .bind(kind)
        .bind(pid)
        .bind(operator)
        .bind(reason)
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Admin adjustment of growth points. A reason is required; writes the ledger and the operation
/// log; cannot go below 0.
pub async fn adjust(
    db: &Database,
    actor: crate::identity::Actor,
    uid: i64,
    amount: i64,
    key: &str,
    reason: &str,
    now: i64,
) -> Result<()> {
    crate::activities::retry_transaction(|| adjust_inner(db, actor, uid, amount, key, reason, now))
        .await
}

async fn adjust_inner(
    db: &Database,
    actor: crate::identity::Actor,
    uid: i64,
    amount: i64,
    key: &str,
    reason: &str,
    now: i64,
) -> Result<()> {
    let reason = reason.trim();
    crate::activities::valid(
        uid > 0
            && amount != 0
            && (-MAX_POINTS..=MAX_POINTS).contains(&amount)
            && crate::identity::valid_request_key(key)
            && !reason.is_empty()
            && reason.chars().count() <= 200,
        crate::i18n::current().activity_manage_error_growth_input(),
    )?;
    let mut tx = db.begin_write().await?;
    crate::activities::lock_members(db, &mut tx, &[actor.uid, uid]).await?;
    crate::activities::actor_locked(db, &mut tx, actor, true).await?;
    crate::activities::valid(
        crate::privacy::active_member(db, &mut tx, uid).await?,
        crate::i18n::current().activity_manage_error_growth_account(),
    )?;
    let event = format!("adjust:{}:{key}", actor.uid);
    if let Some(row) = crate::db::query(
        "SELECT uid,delta,reason FROM gonof_common_growth_ledger WHERE event_key=?",
    )
    .bind(&event)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        crate::activities::valid(
            row.try_get::<i64, _>("uid")? == uid
                && row.try_get::<i64, _>("delta")? == amount
                && crate::db::text(&row, "reason")? == reason,
            crate::i18n::current().activity_manage_error_growth_reused(),
        )?;
        tx.commit().await?;
        return Ok(());
    }
    let balances = lock_accounts(db, &mut tx, &BTreeSet::from([uid])).await?;
    let after = balances[&uid]
        .checked_add(amount)
        .ok_or_else(|| anyhow::anyhow!("growth out of range"))?;
    crate::activities::valid(
        after >= 0,
        crate::i18n::current().activity_manage_error_growth_negative(),
    )?;
    insert_ledger(
        db,
        &mut tx,
        &event,
        uid,
        amount,
        after,
        "adjust",
        None,
        Some(actor.uid),
        reason,
        now,
    )
    .await?;
    crate::db::query(
        "UPDATE gonof_common_growth_account SET growth=?,version=version+1 WHERE uid=?",
    )
    .bind(after)
    .bind(uid)
    .execute(db.observe(&mut *tx))
    .await?;
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'growth.adjust',?,'web',?)")
        .bind(uid)
        .bind(actor.uid)
        .bind(serde_json::json!({"amount": amount, "growth_after": after, "reason": reason}).to_string())
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A member's current growth points; 0 without an account.
pub async fn growth(db: &Database, uid: i64) -> Result<i64> {
    Ok(
        crate::db::query_scalar("SELECT growth FROM gonof_common_growth_account WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.executor())
            .await?
            .unwrap_or(0),
    )
}

/// Reads a member's growth points inside a write transaction (0 without an account), for level thresholds. No lock: the
/// threshold is only a check, and [`sync_posts`] at the end of the posting transaction takes an exclusive lock on the
/// same account row, so taking a shared lock first and upgrading would invite deadlocks; the committed value is enough.
pub(crate) async fn growth_in(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
) -> std::result::Result<i64, sqlx::Error> {
    Ok(
        crate::db::query_scalar("SELECT growth FROM gonof_common_growth_account WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .unwrap_or(0),
    )
}

/// Level threshold (docs/roles.md, "Level gates"): a required level of 1 means no limit and sends no SQL;
/// otherwise the level is determined from growth points. `None` means allowed; `Some(phrase)` is the "Lv3 (Level 3)"
/// shown in the message when the level is not reached. Staff exemptions are decided by the caller first.
pub(crate) async fn level_gate(
    db: &Database,
    tx: &mut Tx<'_>,
    settings: &crate::runtime::GrowthSettings,
    uid: i64,
    required: i64,
) -> std::result::Result<Option<String>, sqlx::Error> {
    if required <= 1 {
        return Ok(None);
    }
    let growth = growth_in(db, tx, uid).await?;
    if (settings.level(growth).number as i64) < required {
        Ok(Some(settings.level_label(required)))
    } else {
        Ok(None)
    }
}

/// Current growth points of a group of members, read by primary key range; members without an account are absent from
/// the result (count as 0).
pub async fn growth_of(db: &Database, uids: &[i64]) -> Result<HashMap<i64, i64>> {
    let uids: BTreeSet<i64> = uids.iter().copied().filter(|uid| *uid > 0).collect();
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql =
        format!("SELECT uid,growth FROM gonof_common_growth_account WHERE uid IN ({placeholders})");
    let mut query = crate::db::query(&sql);
    for uid in &uids {
        query = query.bind(*uid);
    }
    query
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("uid")?, row.try_get("growth")?)))
        .collect()
}

/// Growth points and account state of the authors on one page of floors (docs/queries.md, "Thread page"):
/// joins growth accounts anchored on the member primary key and fetches freeze and account state with them.
/// Verification and honor group badges are read only for the authors in `active`, and neither query joins the member
/// table.
#[derive(Clone, Debug, Default)]
pub struct PageAuthors {
    /// Growth points of authors with a growth account (those without one are absent, as in [`growth_of`]).
    pub growth: HashMap<i64, i64>,
    /// Authors who are not frozen and whose accounts are normal.
    pub active: BTreeSet<i64>,
}

pub async fn page_authors(db: &Database, uids: &[i64]) -> Result<PageAuthors> {
    let uids: BTreeSet<i64> = uids.iter().copied().filter(|uid| *uid > 0).collect();
    let mut authors = PageAuthors::default();
    if uids.is_empty() {
        return Ok(authors);
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql = format!(
        "SELECT m.uid,m.freeze,m.account_state,g.growth FROM gonof_common_member m LEFT JOIN gonof_common_growth_account g ON g.uid=m.uid WHERE m.uid IN ({placeholders})"
    );
    let mut query = crate::db::query(&sql);
    for uid in &uids {
        query = query.bind(*uid);
    }
    for row in query.fetch_all(db.executor()).await? {
        let uid: i64 = row.try_get("uid")?;
        if let Some(growth) = row.try_get::<Option<i64>, _>("growth")? {
            authors.growth.insert(uid, growth);
        }
        if row.try_get::<i64, _>("freeze")? == 0
            && crate::db::text(&row, "account_state")? == "active"
        {
            authors.active.insert(uid);
        }
    }
    Ok(authors)
}

/// Today's total from one source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyPart {
    pub kind: String,
    pub total: i64,
}

/// The member's growth points today (the calendar day in the site time zone, the same day boundary as check-in and the
/// daily credit limit), totaled by source.
pub async fn today(
    db: &Database,
    uid: i64,
    now: i64,
    offset_minutes: i64,
) -> Result<Vec<DailyPart>> {
    let start = crate::clock::day_start(now, offset_minutes);
    let rows = crate::db::query(
        "SELECT kind,delta FROM gonof_common_growth_ledger WHERE uid=? AND dateline>=? AND dateline<?",
    )
    .bind(uid)
    .bind(start)
    .bind(start + 86_400)
    .fetch_all(db.executor())
    .await?;
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        *totals.entry(crate::db::text(&row, "kind")?).or_default() +=
            row.try_get::<i64, _>("delta")?;
    }
    Ok(order_parts(totals))
}

/// Credits the member earned today (increases of the available balance), totaled by source; shown next to growth points
/// under "Earned today". Decreases of the available balance from activity freezes, payouts and refunds do not count as
/// "earned".
pub async fn credits_today(
    db: &Database,
    uid: i64,
    now: i64,
    offset_minutes: i64,
) -> Result<Vec<DailyPart>> {
    let start = crate::clock::day_start(now, offset_minutes);
    let rows = crate::db::query(
        "SELECT kind,available_delta FROM gonof_common_credit_ledger WHERE uid=? AND dateline>=? AND dateline<?",
    )
    .bind(uid)
    .bind(start)
    .bind(start + 86_400)
    .fetch_all(db.executor())
    .await?;
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let delta: i64 = row.try_get("available_delta")?;
        if delta > 0 {
            *totals.entry(crate::db::text(&row, "kind")?).or_default() += delta;
        }
    }
    Ok(totals
        .into_iter()
        .map(|(kind, total)| DailyPart { kind, total })
        .collect())
}

/// Ordered in the fixed order posting, likes received, highlights, acceptance, take-backs…
fn order_parts(totals: BTreeMap<String, i64>) -> Vec<DailyPart> {
    const ORDER: [&str; 9] = [
        "publish", "like", "unlike", "digest", "undigest", "accept", "revoke", "restore", "adjust",
    ];
    let mut parts: Vec<DailyPart> = totals
        .into_iter()
        .map(|(kind, total)| DailyPart { kind, total })
        .collect();
    parts.sort_by_key(|part| {
        ORDER
            .iter()
            .position(|kind| *kind == part.kind)
            .unwrap_or(ORDER.len())
    });
    parts
}

/// One growth ledger entry of the member.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub delta: i64,
    pub growth_after: i64,
    pub kind: String,
    pub pid: Option<i64>,
    pub reason: String,
    pub dateline: i64,
}

/// One page of growth ledger entries, by (dateline, id) descending, using `gonof_growth_member_history`.
pub struct EntryPage {
    pub entries: Vec<Entry>,
    pub next_before: i64,
}

pub const PAGE_SIZE: usize = 50;

pub async fn entries(db: &Database, uid: i64, before: i64) -> Result<EntryPage> {
    const COLUMNS: &str =
        "SELECT id,delta,growth_after,kind,pid,reason,dateline FROM gonof_common_growth_ledger";
    let rows = if before > 0 {
        let cursor: Option<i64> = crate::db::query_scalar(
            "SELECT dateline FROM gonof_common_growth_ledger WHERE id=? AND uid=?",
        )
        .bind(before)
        .bind(uid)
        .fetch_optional(db.executor())
        .await?;
        match cursor {
            Some(dateline) => crate::db::query(&format!("{COLUMNS} WHERE uid=? AND (dateline<? OR (dateline=? AND id<?)) ORDER BY dateline DESC,id DESC LIMIT 51"))
                .bind(uid)
                .bind(dateline)
                .bind(dateline)
                .bind(before)
                .fetch_all(db.executor())
                .await?,
            None => Vec::new(),
        }
    } else {
        crate::db::query(&format!(
            "{COLUMNS} WHERE uid=? ORDER BY dateline DESC,id DESC LIMIT 51"
        ))
        .bind(uid)
        .fetch_all(db.executor())
        .await?
    };
    let more = rows.len() > PAGE_SIZE;
    let entries: Vec<Entry> = rows
        .into_iter()
        .take(PAGE_SIZE)
        .map(|row| {
            Ok(Entry {
                id: row.try_get("id")?,
                delta: row.try_get("delta")?,
                growth_after: row.try_get("growth_after")?,
                kind: crate::db::text(&row, "kind")?,
                pid: row.try_get("pid")?,
                reason: crate::db::text(&row, "reason")?,
                dateline: row.try_get("dateline")?,
            })
        })
        .collect::<Result<_>>()?;
    let next_before = if more {
        entries.last().map_or(0, |entry| entry.id)
    } else {
        0
    };
    Ok(EntryPage {
        entries,
        next_before,
    })
}

/// Reconciliation result: each item lists the inconsistent objects; empty means consistent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Reconciliation {
    /// pids whose `credited` differs from the sum of that post's ledger entries.
    pub post_ledger: Vec<i64>,
    /// uids whose account balance and version do not match the ledger sum, entry count and last entry's balance.
    pub account_ledger: Vec<i64>,
    /// pids whose `credited` differs from the points recomputed from the current state.
    pub post_state: Vec<i64>,
}

impl Reconciliation {
    pub fn is_consistent(&self) -> bool {
        self.post_ledger.is_empty() && self.account_ledger.is_empty() && self.post_state.is_empty()
    }
}

/// Reconciliation (report only, no automatic fixes) of three invariants: each post's `credited` equals the sum of its
/// ledger entries and the points recomputed from its current state, and each account matches its ledger (see
/// [`Reconciliation`]). Reads all growth rows and ledger entries; for admin maintenance and tests, never called on a
/// request path.
pub async fn reconcile(db: &Database) -> Result<Reconciliation> {
    let mut result = Reconciliation::default();
    let mut ledger_by_post: HashMap<i64, i64> = HashMap::new();
    let mut ledger_by_uid: HashMap<i64, (i64, i64, i64, i64)> = HashMap::new();
    for row in crate::db::query(
        "SELECT id,uid,pid,delta,growth_after FROM gonof_common_growth_ledger ORDER BY id",
    )
    .fetch_all(db.executor())
    .await?
    {
        let uid: i64 = row.try_get("uid")?;
        let delta: i64 = row.try_get("delta")?;
        if let Some(pid) = row.try_get::<Option<i64>, _>("pid")? {
            *ledger_by_post.entry(pid).or_default() += delta;
        }
        let entry = ledger_by_uid.entry(uid).or_default();
        entry.0 += delta;
        entry.1 += 1;
        entry.2 = row.try_get("growth_after")?;
        entry.3 = row.try_get("id")?;
    }
    // Read-only: roll back once read. Uses a read-only transaction and takes no write lock (on SQLite a write transaction
    // takes the site-wide write lock as soon as it opens).
    let mut tx = db.begin_read_only().await?;
    for row in crate::db::query(
        "SELECT pid,uid,points_base,points_like,points_digest,points_accept,credited FROM gonof_common_growth_post ORDER BY pid",
    )
    .fetch_all(db.observe(&mut *tx))
    .await?
    {
        let pid: i64 = row.try_get("pid")?;
        let credited: i64 = row.try_get("credited")?;
        if ledger_by_post.get(&pid).copied().unwrap_or(0) != credited {
            result.post_ledger.push(pid);
        }
        let points = Points {
            base: row.try_get("points_base")?,
            like: row.try_get("points_like")?,
            digest: row.try_get("points_digest")?,
            accept: row.try_get("points_accept")?,
        };
        if target(db, &mut tx, pid, row.try_get("uid")?, points).await? != credited {
            result.post_state.push(pid);
        }
    }
    tx.rollback().await?;
    let accounts = crate::db::query("SELECT uid,growth,version FROM gonof_common_growth_account")
        .fetch_all(db.executor())
        .await?;
    let mut seen = BTreeSet::new();
    for row in accounts {
        let uid: i64 = row.try_get("uid")?;
        seen.insert(uid);
        let (sum, count, last, _) = ledger_by_uid.get(&uid).copied().unwrap_or_default();
        let growth: i64 = row.try_get("growth")?;
        if growth != sum
            || row.try_get::<i64, _>("version")? != count
            || (count > 0 && last != growth)
        {
            result.account_ledger.push(uid);
        }
    }
    result
        .account_ledger
        .extend(ledger_by_uid.keys().filter(|uid| !seen.contains(uid)));
    result.account_ledger.sort_unstable();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed with eight custom level names (the real seed leaves them empty).
    fn seeded() -> crate::runtime::GrowthSettings {
        let mut growth = crate::runtime::settings::tests::seeded().growth;
        growth.level_names =
            "One-Star Member\nTwo-Star Member\nThree-Star Member\nFour-Star Member\nFive-Star Member\nSix-Star Member\nSeven-Star Member\nEight-Star Member"
                .split('\n')
                .map(str::to_owned)
                .collect();
        growth
    }

    /// Empty names (the seed) take the language's default name, one per threshold.
    #[test]
    fn empty_level_names_follow_the_language() {
        let growth = crate::runtime::settings::tests::seeded().growth;
        assert!(growth.level_names.is_empty());
        assert_eq!(growth.level(20).name, "Level 2");
        assert_eq!(growth.level_label(8), "Lv8 (Level 8)");
        assert_eq!(growth.name_of(8), None);
    }

    #[test]
    fn seeded_levels_follow_thresholds() {
        let config = seeded();
        assert_eq!(config.level(0).number, 1);
        assert_eq!(config.level(-5).number, 1);
        assert_eq!(config.level(19).number, 1);
        assert_eq!(config.level(20).number, 2);
        assert_eq!(config.level(20).name, "Two-Star Member");
        assert_eq!(config.level(199).number, 3);
        assert_eq!(config.level(200).number, 4);
        assert_eq!(config.level(7999).number, 7);
        let top = config.level(8000);
        assert_eq!((top.number, top.name.as_str()), (8, "Eight-Star Member"));
        assert_eq!((top.next, top.remaining(), top.percent()), (None, 0, 100));
        let middle = config.level(50);
        assert_eq!(
            (middle.floor, middle.next, middle.remaining()),
            (20, Some(80), 30)
        );
        assert_eq!(middle.percent(), 50);
    }

    #[test]
    fn points_follow_post_kind() {
        let config = seeded();
        let thread = config.points(true);
        assert_eq!((thread.base, thread.digest, thread.accept), (3, 10, 0));
        let reply = config.points(false);
        assert_eq!((reply.base, reply.digest, reply.accept), (1, 0, 5));
    }

    #[test]
    fn daily_parts_keep_a_stable_order() {
        let totals = BTreeMap::from([
            ("revoke".to_owned(), -3),
            ("like".to_owned(), 2),
            ("publish".to_owned(), 4),
        ]);
        let kinds: Vec<_> = order_parts(totals)
            .into_iter()
            .map(|part| part.kind)
            .collect();
        assert_eq!(kinds, ["publish", "like", "revoke"]);
    }
}
