//! Per-member totals of the all-time threads, replies and likes boards (`gonof_rank_member`, migration 0009), so a
//! refresh reads their first places from the `(total, uid)` indexes instead of aggregating every post.
//!
//! A post counts towards its author the way the boards count it ([`COUNTED`]): it has an author, is not hidden,
//! withdrawn or erased and its floor is not hidden; its thread is listed and public; its board and the board's parent
//! are public and enabled (what anonymous visitors can read). A first post counts as a thread, any other post as a
//! reply, and the post's likes from members other than the author (`reactions` less the author's own like) as likes.
//!
//! Only this module writes the table (pinned by `db/source_guards.rs`), in the transaction that changes what counts:
//! - posting a thread or a reply: [`post_added`];
//! - a like given or taken back: [`liked`], from `interaction::counters::set_mark`;
//! - hiding, unhiding, withdrawing and restoring a post, erasing it, and changing a thread's visibility: a [`Tally`]
//!   of what the change can reach (the post; a top-level floor with its nested replies; the whole thread when the
//!   first post is hidden or unhidden, which unlists or relists it), read before and after the change, and the
//!   difference applied ([`Tally::apply_change`]);
//! - moving a thread between boards anonymous visitors can and cannot read: [`thread_moved`];
//! - saving a board whose readability for anonymous visitors flips (its visibility, enabled state or parent), which
//!   adds or takes away the content of the board or of its child boards: [`forum_saved`];
//! - imports and fixtures: [`rebuild`].
//!
//! The reads are current reads under the locks the writers already hold: the board path (shared for posting and
//! likes, exclusive for moderation, erasure, moves and board saves) and then the thread row (exclusive). The callers'
//! transactions are READ COMMITTED (`begin_content_write`), except likes, which read with `LOCK IN SHARE MODE` rows
//! the like has already locked. When several members' rows change, they are written in ascending uid order.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::content::copies::ForumFields;
use crate::db::{self, Database, Tx};

/// Joins a post `p` to its thread `t`, the thread's board `f` and the board's parent `pf`.
const JOINS: &str = "JOIN gonof_forum_thread t ON t.tid = p.tid JOIN gonof_forum_forum f ON f.fid = t.fid LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid";

/// The post and thread part of the boards' condition.
const CONTENT: &str = "p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0 AND p.erased_at = 0 AND p.root_invisible = 0 AND t.displayorder >= 0 AND t.visibility = 'public'";

/// The board part of the boards' condition: anonymous visitors can read the board (`organization::forums`).
const READABLE: &str = "f.status = 1 AND f.visibility = 'public' AND (pf.fid IS NULL OR (pf.status = 1 AND pf.visibility = 'public'))";

/// The whole condition on [`JOINS`]: what the threads, replies and likes boards count.
pub const COUNTED: &str = "p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0 AND p.erased_at = 0 AND p.root_invisible = 0 AND t.displayorder >= 0 AND t.visibility = 'public' AND f.status = 1 AND f.visibility = 'public' AND (pf.fid IS NULL OR (pf.status = 1 AND pf.visibility = 'public'))";

/// The columns of a tally, grouped by `p.authorid`; `r` is the author's own like on the post (LEFT JOIN).
const SUMS: &str = "p.authorid AS uid, CAST(SUM(p.first = 1) AS SIGNED) AS threads, CAST(SUM(p.first = 0) AS SIGNED) AS replies, CAST(SUM(p.reactions - (r.uid IS NOT NULL)) AS SIGNED) AS likes";

/// The author's own like on `p`.
const OWN_LIKE: &str =
    "LEFT JOIN gonof_forum_post_reaction r ON r.pid = p.pid AND r.uid = p.authorid";

/// What some posts add to one member's totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    threads: i64,
    replies: i64,
    likes: i64,
}

impl Counts {
    fn minus(self, other: Self) -> Self {
        Self {
            threads: self.threads - other.threads,
            replies: self.replies - other.replies,
            likes: self.likes - other.likes,
        }
    }
}

/// Some posts as the boards count them now, by author.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally(BTreeMap<i64, Counts>);

/// The posts a change can reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Post(i64),
    /// A top-level floor and its nested replies, whose `root_invisible` follows the floor's moderation hiding.
    Floor(i64),
    /// Every post of a thread.
    Thread(i64),
}

impl Tally {
    /// Reads the posts of `scope` as they count now.
    pub async fn read(db: &Database, tx: &mut Tx<'_>, scope: Scope) -> Result<Self> {
        match scope {
            Scope::Post(pid) => Self::sum(db, tx, "p.pid = ?", pid, COUNTED).await,
            Scope::Floor(pid) => {
                let mut tally = Self::sum(db, tx, "p.pid = ?", pid, COUNTED).await?;
                tally.add(Self::sum(db, tx, "p.root_pid = ?", pid, COUNTED).await?);
                Ok(tally)
            }
            Scope::Thread(tid) => Self::sum(db, tx, "p.tid = ?", tid, COUNTED).await,
        }
    }

    async fn sum(
        db: &Database,
        tx: &mut Tx<'_>,
        filter: &str,
        key: i64,
        condition: &str,
    ) -> Result<Self> {
        let sql = format!(
            "SELECT {SUMS} FROM gonof_forum_post p {JOINS} {OWN_LIKE} WHERE {filter} AND {condition} GROUP BY p.authorid"
        );
        let rows = db::query(&sql)
            .bind(key)
            .fetch_all(db.observe(&mut **tx))
            .await?;
        Self::of(&rows)
    }

    fn of(rows: &[db::DbRow]) -> Result<Self> {
        let mut tally = Self::default();
        for row in rows {
            tally.0.insert(
                row.try_get("uid")?,
                Counts {
                    threads: row.try_get("threads")?,
                    replies: row.try_get("replies")?,
                    likes: row.try_get("likes")?,
                },
            );
        }
        Ok(tally)
    }

    fn add(&mut self, other: Self) {
        for (uid, counts) in other.0 {
            let entry = self.0.entry(uid).or_default();
            entry.threads += counts.threads;
            entry.replies += counts.replies;
            entry.likes += counts.likes;
        }
    }

    /// Moves the totals by `after` less `before` (two tallies of the same scope around a change).
    pub async fn apply_change(
        db: &Database,
        tx: &mut Tx<'_>,
        before: &Self,
        after: &Self,
    ) -> Result<()> {
        let uids: BTreeSet<i64> = before.0.keys().chain(after.0.keys()).copied().collect();
        for uid in uids {
            let delta = after
                .0
                .get(&uid)
                .copied()
                .unwrap_or_default()
                .minus(before.0.get(&uid).copied().unwrap_or_default());
            if delta != Counts::default() {
                add(db, tx, uid, delta).await?;
            }
        }
        Ok(())
    }
}

/// Moves one member's totals. An UPDATE first: the row exists once the member has had anything counted (always
/// so when the change takes something away, or adds a like to a counted post), and an UPDATE of the totals leaves the
/// member row alone, while an INSERT checks the foreign key and so share-locks it. Writers lock member rows before
/// board paths, so taking that lock here, after the board path and thread row, could deadlock with them; only a
/// member's first counted content gets to the insert.
async fn add(db: &Database, tx: &mut Tx<'_>, uid: i64, delta: Counts) -> Result<()> {
    let updated = db::query(
        "UPDATE gonof_rank_member SET threads=threads+?,replies=replies+?,likes=likes+? WHERE uid=?",
    )
    .bind(delta.threads)
    .bind(delta.replies)
    .bind(delta.likes)
    .bind(uid)
    .execute(db.observe(&mut **tx))
    .await?;
    if updated.rows_affected() > 0 {
        return Ok(());
    }
    db::query(
        "INSERT INTO gonof_rank_member(uid,threads,replies,likes) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE threads=threads+?,replies=replies+?,likes=likes+?",
    )
    .bind(uid)
    .bind(delta.threads)
    .bind(delta.replies)
    .bind(delta.likes)
    .bind(delta.threads)
    .bind(delta.replies)
    .bind(delta.likes)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// A new thread or reply was written: it counts from now on if the boards count it.
pub async fn post_added(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<()> {
    let after = Tally::read(db, tx, Scope::Post(pid)).await?;
    Tally::apply_change(db, tx, &Tally::default(), &after).await
}

/// `uid` liked `pid` (`delta` 1) or took the like back (-1): the author's likes move with it when the post counts and
/// the like is not the author's own. The caller holds the post's board path and thread row (`interaction::lock_target`).
pub async fn liked(db: &Database, tx: &mut Tx<'_>, pid: i64, uid: i64, delta: i64) -> Result<()> {
    let author: Option<i64> = db::query_scalar(&format!(
        "SELECT p.authorid FROM gonof_forum_post p {JOINS} WHERE p.pid = ? AND {COUNTED} LOCK IN SHARE MODE"
    ))
    .bind(pid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .flatten();
    match author.filter(|author| *author != uid) {
        Some(author) => {
            add(
                db,
                tx,
                author,
                Counts {
                    likes: delta,
                    ..Counts::default()
                },
            )
            .await
        }
        None => Ok(()),
    }
}

/// Whether anonymous visitors can read board `fid` (and its parent), as the board rows stand in this transaction.
async fn readable(db: &Database, tx: &mut Tx<'_>, fid: i64) -> Result<bool> {
    let readable: Option<i64> = db::query_scalar(&format!(
        "SELECT CASE WHEN {READABLE} THEN 1 ELSE 0 END FROM gonof_forum_forum f LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid WHERE f.fid = ? LOCK IN SHARE MODE"
    ))
    .bind(fid)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    Ok(readable == Some(1))
}

/// A board's parent can be read by anonymous visitors (true for a top-level board).
async fn parent_readable(db: &Database, tx: &mut Tx<'_>, parent: Option<i64>) -> Result<bool> {
    match parent {
        None => Ok(true),
        Some(parent) => readable(db, tx, parent).await,
    }
}

/// The posts of threads in `fids` that would count if their board could be read (the board part of the condition
/// left out).
async fn content_of(db: &Database, tx: &mut Tx<'_>, fids: &[i64]) -> Result<Tally> {
    let mut tally = Tally::default();
    for fid in fids {
        tally.add(Tally::sum(db, tx, "t.fid = ?", *fid, CONTENT).await?);
    }
    Ok(tally)
}

/// Thread `tid` moved from board `from` to board `to` (the caller holds both board paths exclusively): when only one
/// of them can be read by anonymous visitors, the thread's content enters or leaves the totals.
pub async fn thread_moved(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    from: i64,
    to: i64,
) -> Result<()> {
    let (before, after) = (readable(db, tx, from).await?, readable(db, tx, to).await?);
    if before == after {
        return Ok(());
    }
    let content = Tally::sum(db, tx, "p.tid = ?", tid, CONTENT).await?;
    let empty = Tally::default();
    if after {
        Tally::apply_change(db, tx, &empty, &content).await
    } else {
        Tally::apply_change(db, tx, &content, &empty).await
    }
}

/// Board `fid` was saved from `before` to `after` (the caller holds its row exclusively, and the new parent's). When
/// that changed whether anonymous visitors can read the board, or its child boards (a top-level board's own state is
/// part of theirs), their content enters or leaves the totals.
pub async fn forum_saved(
    db: &Database,
    tx: &mut Tx<'_>,
    fid: i64,
    before: &ForumFields,
    after: &ForumFields,
) -> Result<()> {
    let own = |fields: &ForumFields| fields.visibility == "public" && fields.status == 1;
    let was = own(before) && parent_readable(db, tx, before.parent_fid).await?;
    let is = own(after) && parent_readable(db, tx, after.parent_fid).await?;
    let (mut gained, mut lost) = (Vec::new(), Vec::new());
    if was != is {
        (if is { &mut gained } else { &mut lost }).push(fid);
    }
    if own(before) != own(after) {
        let children = db::query(
            "SELECT fid,visibility,status FROM gonof_forum_forum WHERE parent_fid=? ORDER BY fid LOCK IN SHARE MODE",
        )
        .bind(fid)
        .fetch_all(db.observe(&mut **tx))
        .await?;
        for child in children {
            if db::text(&child, "visibility")? == "public"
                && child.try_get::<i64, _>("status")? == 1
            {
                (if own(after) { &mut gained } else { &mut lost }).push(child.try_get("fid")?);
            }
        }
    }
    let gained = content_of(db, tx, &gained).await?;
    let lost = content_of(db, tx, &lost).await?;
    Tally::apply_change(db, tx, &lost, &gained).await
}

/// Recomputes the whole table from the posts and likes, after threads and posts were written in bulk bypassing the
/// write paths (fixtures, imports, tests).
pub async fn rebuild(db: &Database, tx: &mut Tx<'_>) -> Result<()> {
    db::query("DELETE FROM gonof_rank_member")
        .execute(db.observe(&mut **tx))
        .await?;
    db::query(&format!(
        "INSERT INTO gonof_rank_member(uid,threads,replies,likes)
         SELECT p.authorid, SUM(p.first = 1), SUM(p.first = 0), COALESCE(SUM(l.likes), 0)
           FROM gonof_forum_post p {JOINS}
           LEFT JOIN (SELECT r.pid, COUNT(*) AS likes FROM gonof_forum_post_reaction r
                        JOIN gonof_forum_post rp ON rp.pid = r.pid WHERE r.uid <> rp.authorid GROUP BY r.pid) l
             ON l.pid = p.pid
          WHERE {COUNTED} GROUP BY p.authorid"
    ))
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_condition_is_its_two_parts() {
        assert_eq!(COUNTED, format!("{CONTENT} AND {READABLE}"));
    }
}
