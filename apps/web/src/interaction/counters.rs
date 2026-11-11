//! Counter columns for the thread detail page (docs/queries.md, "Counter columns"): the post like count
//! `reactions` and downvote count `downvotes`, and the thread favorite count `favorites`, tip count `tips` and tip
//! total `tip_total`. The thread page reads these columns instead of running COUNT and SUM over the detail rows and
//! ledger.
//!
//! Only this module writes these columns (pinned by `db/source_guards.rs`), always in the same transaction that writes
//! the detail rows:
//! - After adding or removing a like, downvote or favorite, the count is redone with a current read of the detail rows
//!   (`LOCK IN SHARE MODE`) and written back, the same way growth points count likes; it does not rely on the
//!   affected-row count of `ON DUPLICATE KEY` (the driver has `FOUND_ROWS` on, so an insert and an existing row both
//!   return 1). The caller already holds an exclusive lock on the thread row, so writes to the same thread queue up
//!   and the recount reads the final detail rows before commit;
//! - Tips only go up (the ledger is append-only and cannot be withdrawn): once the payment and receipt ledger rows are
//!   written, the count and amount are increased once;
//! - Account deletion keeps the likes, downvotes and favorites the member gave, so these counts do not change.
//!
//! Every UPDATE of a thread row increments `cache_revision` (source scan rule), counter updates included.
use crate::db::Tx;
use anyhow::{Result, ensure};

use crate::db::Database;

/// Recounts the likes of one floor and writes them back to the post row.
pub(crate) async fn recount_reactions(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<()> {
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid=? LOCK IN SHARE MODE",
    )
    .bind(pid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    crate::db::query("UPDATE gonof_forum_post SET reactions=? WHERE pid=?")
        .bind(count)
        .bind(pid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Recounts the downvotes of the first post and writes them back to the post row.
pub(crate) async fn recount_downvotes(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<()> {
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_forum_post_downvote WHERE pid=? LOCK IN SHARE MODE",
    )
    .bind(pid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    crate::db::query("UPDATE gonof_forum_post SET downvotes=? WHERE pid=?")
        .bind(count)
        .bind(pid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Recounts the favorites of a thread and writes them back to the thread row.
pub(crate) async fn recount_favorites(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<()> {
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_favorite WHERE tid=? LOCK IN SHARE MODE",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    crate::db::query(
        "UPDATE gonof_forum_thread SET favorites=?,cache_revision=cache_revision+1 WHERE tid=?",
    )
    .bind(count)
    .bind(tid)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
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
