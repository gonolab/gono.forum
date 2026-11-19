//! Counter columns for the thread detail page (docs/queries.md, "Counter columns"): the post like count
//! `reactions` and downvote count `downvotes`, and the thread favorite count `favorites`, tip count `tips` and tip
//! total `tip_total`. The thread page reads these columns instead of running COUNT and SUM over the detail rows and
//! ledger, and growth points read `reactions` instead of counting likes (`growth::sync_posts`).
//!
//! Only this module writes these columns (pinned by `db/source_guards.rs`), always in the same transaction that writes
//! the detail rows:
//! - Likes, downvotes and favorites go through [`set_mark`]: the member's own detail row is read by primary key with a
//!   current read, and only when the state really changes is the row inserted or deleted and the count moved by one.
//!   The caller already holds an exclusive lock on the thread row, so marks on one thread run one at a time and each
//!   sees the one before; the cost of a mark does not grow with the number of marks on the post. It does not rely on
//!   the affected-row count of `ON DUPLICATE KEY` (the driver has `FOUND_ROWS` on, so an insert and an existing row
//!   both return 1). On MySQL the read of a missing row takes a gap lock, which can deadlock two marks on adjacent
//!   posts of different threads; the callers retry such a transaction (`interaction::Interaction`);
//! - Tips only go up (the ledger is append-only and cannot be withdrawn): once the payment and receipt ledger rows are
//!   written, the count and amount are increased once;
//! - Account deletion keeps the likes, downvotes and favorites the member gave, so these counts do not change.
//!
//! Every UPDATE of a thread row increments `cache_revision` (source scan rule), counter updates included.
use crate::db::Tx;
use anyhow::{Result, ensure};

use crate::db::Database;

/// A member's like, downvote or favorite: the detail row and the counter column it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// A like on a post: `gonof_forum_post_reaction`, counted in `gonof_forum_post.reactions`.
    Reaction,
    /// A downvote on a first post: `gonof_forum_post_downvote`, counted in `gonof_forum_post.downvotes`.
    Downvote,
    /// A favorite on a thread: `gonof_home_favorite`, counted in `gonof_forum_thread.favorites`.
    Favorite,
}

/// The statements of one kind of mark; each binds the target (a pid, or a tid for favorites) first, then the member.
struct MarkSql {
    find: &'static str,
    insert: &'static str,
    delete: &'static str,
    /// Moves the counter by the bound delta.
    count: &'static str,
}

impl Mark {
    fn sql(self) -> MarkSql {
        match self {
            Self::Reaction => MarkSql {
                find: "SELECT uid FROM gonof_forum_post_reaction WHERE pid=? AND uid=? FOR UPDATE",
                insert: "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(?,?,?)",
                delete: "DELETE FROM gonof_forum_post_reaction WHERE pid=? AND uid=?",
                count: "UPDATE gonof_forum_post SET reactions=reactions+? WHERE pid=?",
            },
            Self::Downvote => MarkSql {
                find: "SELECT uid FROM gonof_forum_post_downvote WHERE pid=? AND uid=? FOR UPDATE",
                insert: "INSERT INTO gonof_forum_post_downvote(pid,uid,dateline) VALUES(?,?,?)",
                delete: "DELETE FROM gonof_forum_post_downvote WHERE pid=? AND uid=?",
                count: "UPDATE gonof_forum_post SET downvotes=downvotes+? WHERE pid=?",
            },
            Self::Favorite => MarkSql {
                find: "SELECT uid FROM gonof_home_favorite WHERE tid=? AND uid=? FOR UPDATE",
                insert: "INSERT INTO gonof_home_favorite(tid,uid,dateline) VALUES(?,?,?)",
                delete: "DELETE FROM gonof_home_favorite WHERE tid=? AND uid=?",
                count: "UPDATE gonof_forum_thread SET favorites=favorites+?,cache_revision=cache_revision+1 WHERE tid=?",
            },
        }
    }
}

/// Sets the member's mark on `target` (a pid, or a tid for favorites) to `desired` and returns whether it changed.
/// The caller holds the exclusive lock on the target's thread row.
pub(crate) async fn set_mark(
    db: &Database,
    tx: &mut Tx<'_>,
    mark: Mark,
    uid: i64,
    target: i64,
    desired: bool,
    now: i64,
) -> Result<bool> {
    let sql = mark.sql();
    let present = crate::db::query(sql.find)
        .bind(target)
        .bind(uid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .is_some();
    if present == desired {
        return Ok(false);
    }
    if desired {
        crate::db::query(sql.insert)
            .bind(target)
            .bind(uid)
            .bind(now)
            .execute(db.observe(&mut **tx))
            .await?;
    } else {
        crate::db::query(sql.delete)
            .bind(target)
            .bind(uid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    let updated = crate::db::query(sql.count)
        .bind(if desired { 1 } else { -1 })
        .bind(target)
        .execute(db.observe(&mut **tx))
        .await?;
    ensure!(
        updated.rows_affected() == 1,
        "the marked post or thread does not exist"
    );
    if mark == Mark::Reaction {
        crate::ranks::counts::liked(db, tx, target, uid, if desired { 1 } else { -1 }).await?;
    }
    Ok(true)
}

/// One tip: the payment and receipt ledger rows are already written (the event key is unique, so a repeated tip
/// fails at insert); the thread tip count goes up by one and the total by the amount received.
pub(crate) async fn add_tip(db: &Database, tx: &mut Tx<'_>, tid: i64, amount: i64) -> Result<()> {
    let updated = crate::db::query(
        "UPDATE gonof_forum_thread SET tips=tips+1,tip_total=tip_total+?,cache_revision=cache_revision+1 WHERE tid=?",
    )
    .bind(amount)
    .bind(tid)
    .execute(db.observe(&mut **tx))
    .await?;
    ensure!(
        updated.rows_affected() == 1,
        "the tipped thread does not exist"
    );
    Ok(())
}

/// Recomputes all counters from the detail rows and the tip ledger (used in tests after raw SQL changed the detail
/// rows, via `test_support::refresh_listing`).
pub async fn rebuild(db: &Database, tx: &mut Tx<'_>) -> Result<()> {
    // Zero first, then count the detail rows grouped and write them back in batches by value (previously a MySQL
    // multi-table UPDATE).
    crate::db::query(
        "UPDATE gonof_forum_post SET reactions=0,downvotes=0 WHERE reactions<>0 OR downvotes<>0",
    )
    .execute(db.observe(&mut **tx))
    .await?;
    crate::db::query("UPDATE gonof_forum_thread SET favorites=0,tips=0,tip_total=0,cache_revision=cache_revision+1 WHERE favorites<>0 OR tips<>0 OR tip_total<>0")
        .execute(db.observe(&mut **tx))
        .await?;
    for (counts, update) in [
        (
            "SELECT pid,COUNT(*) FROM gonof_forum_post_reaction GROUP BY pid",
            "UPDATE gonof_forum_post SET reactions=? WHERE pid IN (",
        ),
        (
            "SELECT pid,COUNT(*) FROM gonof_forum_post_downvote GROUP BY pid",
            "UPDATE gonof_forum_post SET downvotes=? WHERE pid IN (",
        ),
        (
            "SELECT tid,COUNT(*) FROM gonof_home_favorite GROUP BY tid",
            "UPDATE gonof_forum_thread SET favorites=?,cache_revision=cache_revision+1 WHERE tid IN (",
        ),
        (
            "SELECT tid,COUNT(*),CAST(SUM(available_delta) AS SIGNED) FROM gonof_common_credit_ledger WHERE kind='tip_in' AND tid IS NOT NULL GROUP BY tid",
            "UPDATE gonof_forum_thread SET tips=?,tip_total=?,cache_revision=cache_revision+1 WHERE tid IN (",
        ),
    ] {
        let rows = crate::db::query(counts)
            .fetch_all(db.observe(&mut **tx))
            .await?;
        let rows = rows
            .iter()
            .map(|row| {
                let values = (1..row.len())
                    .map(|index| row.try_get::<i64, _>(index))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((row.try_get(0)?, values))
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        crate::content::copies::update_by_value(db, tx, update, rows).await?;
    }
    Ok(())
}
