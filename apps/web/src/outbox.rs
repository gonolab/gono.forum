//! What the Outbox queues share: how a worker finds the next event to claim, and the retry limit of the queues for
//! mail, content notifications and media.
//!
//! After `MAX_ATTEMPTS` consecutive transient failures, automatic retries stop and the status becomes the existing
//! `failed`. `status` has only four values, pending, processing, sent and failed (`db::source_guards` pins the
//! literals written in `SET`), so "attempts exhausted" and "no longer eligible" share `failed` and are
//! told apart by a fixed prefix of `last_error`: the former can be scheduled for retry in the admin area, the latter
//! cannot. The admin task page only shows the redacted label derived from this, never the raw `last_error`.

use crate::db::{self, Database, DbRow, Query, Tx};

/// The index claims read: `(event_type, status, available_at, eventid)` (docs/queries.md, "Outbox claims").
const CLAIM_INDEX: &str = "gonof_outbox_claim";

/// The statuses a worker may claim: a pending event, and a processing one whose lease has run out (its worker stopped
/// or lost the database before it finished).
const CLAIMABLE: [&str; 2] = ["pending", "processing"];

/// How many candidates a claim tries before it gives up for this round: each miss means another process claimed the
/// candidate first, so the queue is being worked on anyway.
const CLAIM_TRIES: usize = 4;

/// The statement that finds the next event to claim among `types` kinds of event (docs/queries.md, "Outbox claims").
///
/// One branch for each type and claimable status reads its `gonof_outbox_claim` range in index order, from the event
/// due longest, up to the first one that is due and not leased (`LIMIT 1`); the statement keeps the branch row due
/// longest, by `available_at` and then `eventid`. A claim therefore reads a few index entries per type however long
/// the backlog of its own or other queues is and however many sent events are kept. Written as one condition with
/// `IN` lists instead, the ranges of several types or statuses would come out of the index unsorted, and both MySQL
/// and SQLite would read and sort all of them (filesort, temporary B-tree) to find the first.
///
/// The outbox is `o`. `join` adds tables after it, `filter` conditions (each starting with ` AND`), and `columns` is
/// the select list of the branches; the statement returns those columns by their names, `eventid` and `available_at`
/// among them. Parameters, branch by branch: the type, `now` for the due time, `now` for the lease, then those of
/// `filter`; [`bind_next_due`] binds them in this order.
pub(crate) fn next_due_sql(types: usize, columns: &str, join: &str, filter: &str) -> String {
    let branches: Vec<String> = (0..types)
        .flat_map(|_| CLAIMABLE)
        .enumerate()
        .map(|(n, status)| {
            format!(
                "SELECT * FROM (SELECT {columns} FROM gonof_common_outbox o FORCE INDEX ({CLAIM_INDEX}){join} WHERE o.event_type=? AND o.status='{status}' AND o.available_at<=? AND o.lease_until<=?{filter} ORDER BY o.available_at,o.eventid LIMIT 1) c{n}"
            )
        })
        .collect();
    format!(
        "{} ORDER BY available_at,eventid LIMIT 1",
        branches.join(" UNION ALL ")
    )
}

/// Binds the parameters of [`next_due_sql`]; `filter` binds those of the branch's own conditions.
pub(crate) fn bind_next_due<'q>(
    mut query: Query<'q>,
    types: &[&'q str],
    now: i64,
    filter: impl Fn(Query<'q>) -> Query<'q>,
) -> Query<'q> {
    for kind in types {
        for _ in CLAIMABLE {
            query = filter(query.bind(*kind).bind(now).bind(now));
        }
    }
    query
}

/// Finds and locks the next event to claim among `types` in `tx`, and reads `columns` of it.
///
/// The candidate comes from [`next_due_sql`], read without a lock (on MySQL write transactions are READ COMMITTED,
/// so each read sees what other processes have committed). Its row is then locked by primary key and checked again,
/// since another process may have claimed it in between; the claim then moves on to the next candidate, a few times
/// at most. The caller marks the row claimed in the same transaction.
pub(crate) async fn lock_next_due(
    db: &Database,
    tx: &mut Tx<'_>,
    types: &[&str],
    columns: &str,
    now: i64,
) -> anyhow::Result<Option<DbRow>> {
    let candidates = next_due_sql(types.len(), "o.eventid,o.available_at", "", "");
    let lock = format!(
        "SELECT {columns} FROM gonof_common_outbox WHERE eventid=? AND status IN ('pending','processing') AND available_at<=? AND lease_until<=? FOR UPDATE"
    );
    for _ in 0..CLAIM_TRIES {
        let candidate: Option<i64> = bind_next_due(db::query(&candidates), types, now, |q| q)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .map(|row| row.try_get("eventid"))
            .transpose()?;
        let Some(eventid) = candidate else {
            return Ok(None);
        };
        let row = db::query(&lock)
            .bind(eventid)
            .bind(now)
            .bind(now)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
        if row.is_some() {
            return Ok(row);
        }
    }
    Ok(None)
}

/// Retry limit for transient failures, counted by the Outbox's `attempts`.
pub const MAX_ATTEMPTS: i64 = 20;

const EXHAUSTED_PREFIX: &str = "gave up after ";
const EXHAUSTED_MARK: &str = " attempts";

/// The text written into `last_error` when attempts are exhausted, with the last failure reason, truncated to the
/// column width of 1000 characters.
pub fn exhausted_error(attempts: i64, last: &str) -> String {
    format!("{EXHAUSTED_PREFIX}{attempts}{EXHAUSTED_MARK}: {last}")
        .chars()
        .take(1000)
        .collect()
}

/// Whether this `failed` is due to exhausted retries.
pub fn is_exhausted(last_error: Option<&str>) -> bool {
    last_error.is_some_and(|error| {
        error
            .strip_prefix(EXHAUSTED_PREFIX)
            .and_then(|rest| rest.split_once(EXHAUSTED_MARK))
            .is_some_and(|(count, _)| {
                !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A claim takes the event due longest among its types, by `available_at` and then `eventid`, whatever its type
    /// or whether it is pending or a processing event whose lease ran out; it skips events that are not due yet, held
    /// under a live lease, sent, failed, or of other types.
    #[tokio::test]
    async fn claims_take_the_event_due_longest() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = 1_000;
        for (key, kind, status, available_at, lease_until) in [
            ("a-late", "test.a", "pending", 900, 0),
            ("b-early", "test.b", "pending", 500, 0),
            ("a-expired", "test.a", "processing", 700, 999),
            ("b-tie", "test.b", "pending", 900, 0),
            ("a-leased", "test.a", "processing", 100, 1_001),
            ("a-future", "test.a", "pending", 1_001, 0),
            ("b-sent", "test.b", "sent", 100, 0),
            ("b-failed", "test.b", "failed", 100, 0),
            ("other", "test.other", "pending", 100, 0),
        ] {
            db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status,lease_until) VALUES(?,?,'test',1,'{}',?,?,?,?)")
                .bind(key)
                .bind(kind)
                .bind(available_at)
                .bind(available_at)
                .bind(status)
                .bind(lease_until)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let mut order = Vec::new();
        loop {
            let mut tx = db.begin_write().await.unwrap();
            let row = lock_next_due(db, &mut tx, &["test.a", "test.b"], "eventid,event_key", now)
                .await
                .unwrap();
            let Some(row) = row else {
                break;
            };
            db::query(
                "UPDATE gonof_common_outbox SET status='processing',lease_until=? WHERE eventid=?",
            )
            .bind(now + 60)
            .bind(row.try_get::<i64, _>("eventid").unwrap())
            .execute(db.observe(&mut *tx))
            .await
            .unwrap();
            tx.commit().await.unwrap();
            order.push(db::text(&row, "event_key").unwrap());
        }
        assert_eq!(order, ["b-early", "a-expired", "a-late", "b-tie"]);
        services.cleanup().await.unwrap();
    }

    #[test]
    fn exhausted_failures_are_told_apart_by_their_fixed_prefix() {
        let error = exhausted_error(
            MAX_ATTEMPTS,
            "SMTP connection timed out; check the server and port",
        );
        assert_eq!(
            error,
            "gave up after 20 attempts: SMTP connection timed out; check the server and port"
        );
        assert!(is_exhausted(Some(&error)));
        for other in [
            None,
            Some(""),
            Some("the recipient turned off this kind of email notification"),
            Some("gave up after  attempts: no count"),
            Some("gave up after x attempts: not a number"),
            Some("content notification event is malformed"),
        ] {
            assert!(!is_exhausted(other), "{other:?}");
        }
        // Truncation counts characters: "\u{957f}" (the Chinese for "long") takes three bytes in UTF-8.
        assert_eq!(
            exhausted_error(20, &"\u{957f}".repeat(2000))
                .chars()
                .count(),
            1000
        );
    }
}
