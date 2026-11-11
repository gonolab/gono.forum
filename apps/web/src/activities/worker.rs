use super::*;
use crate::db::Tx;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha256};

/// Activities whose settlement failed back off in-process and stay out of the due list meanwhile.
///
/// Durable backoff would need a new column on the activity table, so it is kept in-process only: after a restart
/// everything is retried from scratch, and each process keeps its own record.
/// Only activities that have failed are recorded; on success or a state change (no longer due) the entry is removed.
#[derive(Default)]
pub struct DueBackoff(std::sync::Mutex<std::collections::BTreeMap<i64, (u32, i64)>>);

impl DueBackoff {
    /// Activities in backoff; at most 100 are excluded so the query stays bounded.
    fn waiting(&self, now: i64) -> Vec<i64> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, (_, retry_at)| *retry_at > now.saturating_sub(86400));
        entries
            .iter()
            .filter(|(_, (_, retry_at))| *retry_at > now)
            .map(|(tid, _)| *tid)
            .take(100)
            .collect()
    }

    /// Records one failure and returns the backoff in seconds: 10, 20, 40, ... up to 600.
    fn fail(&self, tid: i64, now: i64) -> i64 {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(tid).or_insert((0, now));
        entry.0 = entry.0.saturating_add(1);
        let delay = 5_i64.saturating_mul(1 << entry.0.min(7)).min(600);
        entry.1 = now.saturating_add(delay);
        delay
    }

    fn clear(&self, tid: i64) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&tid);
    }
}

/// A connection-level failure: when the database is unavailable every item fails, so the whole batch stops and is
/// left to the next round instead of recording a backoff per item.
fn connection_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed)
        )
    })
}

/// Caller must pass the mandatory Redis health gate before every batch. Monetary
/// idempotency and resumability are SQL-backed even if Redis fails after commit.
pub async fn process_due(db: &Database, now: i64, limit: i64) -> Result<usize> {
    process_due_with(db, now, limit, &DueBackoff::default()).await
}

/// Same as `process_due`, with failure backoff kept across batches in `backoff`.
///
/// A failing item is only logged and backed off, and the loop moves on: the list is sorted by deadline, and with
/// `?` an item that kept failing would always come first and block every due settlement.
pub async fn process_due_with(
    db: &Database,
    now: i64,
    limit: i64,
    backoff: &DueBackoff,
) -> Result<usize> {
    let waiting = backoff.waiting(now);
    let mut sql = String::from(
        "SELECT tid FROM gonof_forum_activity WHERE ((state='open' AND deadline>0 AND deadline<=?) OR state='drawing')",
    );
    if !waiting.is_empty() {
        sql.push_str(" AND tid NOT IN (");
        sql.push_str(&vec!["?"; waiting.len()].join(","));
        sql.push(')');
    }
    sql.push_str(" ORDER BY deadline,tid LIMIT ?");
    let mut query = crate::db::query_scalar(&sql).bind(now);
    for tid in &waiting {
        query = query.bind(*tid);
    }
    let tids: Vec<i64> = query
        .bind(limit.clamp(1, 100))
        .fetch_all(db.executor())
        .await?;
    let mut completed = 0;
    if tids.is_empty() {
        return Ok(completed);
    }
    // Settlement is background work: the ledger reasons it writes are in the site language
    // (docs/architecture.md, "Languages").
    let site = crate::site::Site {
        language: crate::runtime::read(db)
            .await
            .map_err(anyhow::Error::new)?
            .site
            .lang(),
        ..crate::site::Site::default()
    };
    for tid in tids {
        match crate::site::scope(site.clone(), retry_transaction(|| settle_one(db, tid, now))).await
        {
            Ok(settled) => {
                backoff.clear(tid);
                completed += usize::from(settled);
            }
            Err(error) if connection_failure(&error) => return Err(error),
            Err(error) => {
                let delay = backoff.fail(tid, now);
                tracing::warn!(
                    tid,
                    retry_in = delay,
                    error = %format!("{error:#}"),
                    "settling a due activity failed; skipping it and continuing with the next one"
                );
            }
        }
    }
    Ok(completed)
}

async fn settle_one(db: &Database, tid: i64, now: i64) -> Result<bool> {
    // Close lottery enrollment before discovering the full set of account locks.
    // No balance changes occur in this phase. A crash leaves a resumable drawing.
    let mut phase = db.begin_write().await?;
    let _ = lock_thread(db, &mut phase, tid).await?;
    let a = crate::db::query(
        "SELECT kind,state,deadline FROM gonof_forum_activity WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut *phase))
    .await?;
    let kind: i64 = a.try_get("kind")?;
    if kind == 3
        && crate::db::text(&a, "state")? == "open"
        && a.try_get::<i64, _>("deadline")? <= now
    {
        crate::db::query("UPDATE gonof_forum_activity SET state=? WHERE tid=?")
            .bind(super::invariants::State::Drawing.value())
            .bind(tid)
            .execute(db.observe(&mut *phase))
            .await?;
        crate::db::query("INSERT INTO gonof_forum_activity_draw(tid,seed,eligible,winners,dateline) VALUES(?,?,'[]','[]',?) ON DUPLICATE KEY UPDATE tid=tid").bind(tid).bind(crate::identity::random_token()).bind(now).execute(db.observe(&mut *phase)).await?;
    }
    phase.commit().await?;
    let owner = discover_owner(db, tid)
        .await?
        .ok_or_else(|| problem("the activity's author does not exist"))?;
    if kind == 3 {
        return settle_lottery(db, tid, owner, now).await;
    }
    let mut tx = db.begin_content_write().await?;
    lock_members(db, &mut tx, &[owner]).await?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    let a = crate::db::query(
        "SELECT owner_uid,state,deadline,remaining FROM gonof_forum_activity WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let state: String = crate::db::text(&a, "state")?;
    if !["open", "drawing"].contains(&state.as_str()) || a.try_get::<i64, _>("deadline")? > now {
        tx.commit().await?;
        return Ok(false);
    }
    valid(
        a.try_get::<i64, _>("owner_uid")? == owner,
        "the activity's author changed",
    )?;
    close_unpaid(
        db,
        &mut tx,
        tid,
        owner,
        a.try_get("remaining")?,
        readable_thread(&thread)?,
        now,
    )
    .await?;
    if kind == 2 {
        crate::db::query("UPDATE gonof_forum_redpacket SET remaining_shares=0 WHERE tid=?")
            .bind(tid)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Refunds all unpaid budget to the author; the activity is recorded as expired or cancelled depending on whether
/// the thread is still readable.
async fn close_unpaid(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    owner: i64,
    remaining: i64,
    readable: bool,
    now: i64,
) -> Result<()> {
    ledger::refund(db, tx, owner, tid, remaining, now).await?;
    crate::db::query(
        "UPDATE gonof_forum_activity SET state=?,remaining=0,settled_at=? WHERE tid=?",
    )
    .bind(
        if readable {
            super::invariants::State::Expired
        } else {
            super::invariants::State::Cancelled
        }
        .value(),
    )
    .bind(now)
    .bind(tid)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// At most this many winners are paid per payout transaction.
pub(super) const DRAW_BATCH: usize = 100;

/// One prize: number, type, slots and credits per person (> 0 only for credit prizes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Prize {
    pub number: i64,
    pub kind: String,
    pub winners: i64,
    pub amount: i64,
}

/// The prize and slot a person on the winner list gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Allocation {
    pub uid: i64,
    pub prize: i64,
    pub slot: i64,
    pub kind: String,
    pub amount: i64,
}

/// Splits the winner list by prize number order: the first winners₁ people get prize 1, the
/// next winners₂ people get prize 2, and so on; within one prize the j-th person gets the j-th slot. When the list
/// is too short, the later prizes stay unfilled.
pub(super) fn allocate(winners: &[i64], prizes: &[Prize]) -> Vec<Allocation> {
    let mut list = winners.iter();
    let mut result = Vec::with_capacity(winners.len());
    for prize in prizes {
        for slot in 1..=prize.winners {
            let Some(uid) = list.next() else {
                return result;
            };
            result.push(Allocation {
                uid: *uid,
                prize: prize.number,
                slot,
                kind: prize.kind.clone(),
                amount: prize.amount,
            });
        }
    }
    result
}

/// Payees of credit prizes, in winning order: (uid, credits).
fn payouts(allocations: &[Allocation]) -> Vec<(i64, i64)> {
    allocations
        .iter()
        .filter(|a| a.kind == "credit")
        .map(|a| (a.uid, a.amount))
        .collect()
}

pub(super) async fn prizes_locked(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<Vec<Prize>> {
    crate::db::query(
        "SELECT prize_no,kind,winners,amount FROM gonof_forum_lottery_prize WHERE tid=? ORDER BY prize_no",
    )
    .bind(tid)
    .fetch_all(db.observe(&mut **tx))
    .await?
    .iter()
    .map(|r| {
        Ok(Prize {
            number: r.try_get("prize_no")?,
            kind: crate::db::text(r, "kind")?,
            winners: r.try_get("winners")?,
            amount: r.try_get("amount")?,
        })
    })
    .collect()
}

/// Result of a draw: already finished, or the winner list is fixed and credits are waiting to be paid in batches.
enum Draw {
    Finished(bool),
    Decided {
        payouts: Vec<(i64, i64)>,
        table: &'static str,
    },
}

/// A lottery settles in two steps.
///
/// One transaction that locked every entrant (with no limit on their number) and paid them one by one would hold the
/// forum row and thread row the whole time, so settling is split:
///
/// 1. Draw: lock only the author, forum, thread and activity rows, use a consistent read to find the eligible
///    entrants and fix the winner list, write it into the draw record, and in the same transaction assign the
///    redemption codes and other prize slots to the winners. Balances are not touched. Entry was already closed in
///    step 0; a freeze or account deletion of an entrant running concurrently counts as happening after the draw,
///    with the same result as a freeze after the draw had committed.
/// 2. Payout: credit winners, in list order, at most [`DRAW_BATCH`] people per batch, one transaction per batch:
///    lock the author and this batch of winners, then the content and activity rows, transfer one by one and reduce
///    the activity's `remaining` alongside. After any batch commits, the author's frozen credits equal the sum of
///    `remaining` over their activities, so the ledger matches the budget. Progress is recorded in `remaining`
///    itself: people paid = the number of credit winners whose amount prefix sum equals budget - remaining (every
///    amount is ≥ 1, so prefix sums are strictly increasing).
///    A failure midway rolls back only the current batch and the next round resumes from that point; the transfer
///    event key `activity:{tid}:draw:{uid}` stays the same, so a replay never pays twice. The last batch also refunds
///    the unpaid budget and marks the activity as settled.
///
/// Once written, the list never changes: if the thread is later hidden or withdrawn, the list is still paid out in
/// full (`terminate` does not cancel it).
async fn settle_lottery(db: &Database, tid: i64, owner: i64, now: i64) -> Result<bool> {
    let (payouts, table) = match draw_lottery(db, tid, owner, now).await? {
        Draw::Finished(settled) => return Ok(settled),
        Draw::Decided { payouts, table } => (payouts, table),
    };
    loop {
        if let Some(settled) = pay_winners(db, tid, owner, &payouts, table, now).await? {
            return Ok(settled);
        }
    }
}

/// Reply entry: at draw time the entrant must have a valid reply in this thread made before the deadline.
const REPLY_ELIGIBLE: &str = "SELECT e.uid FROM gonof_forum_lottery_entry e JOIN gonof_common_member m ON m.uid=e.uid WHERE e.tid=? AND e.uid<>? AND e.dateline<? AND m.`freeze`=0 AND m.account_state='active' AND NOT EXISTS(SELECT 1 FROM gonof_common_data_request d WHERE d.uid=e.uid AND d.kind IN ('close','erase') AND d.status IN ('cooling','pending','processing','failed')) AND NOT EXISTS(SELECT 1 FROM gonof_forum_lottery_exclusion x WHERE x.tid=e.tid AND x.uid=e.uid) AND EXISTS(SELECT 1 FROM gonof_forum_post p LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid WHERE p.tid=e.tid AND p.authorid=e.uid AND p.first=0 AND p.invisible=0 AND p.deleted_at=0 AND p.erased_at=0 AND p.dateline<? AND (p.root_pid IS NULL OR (parent.invisible=0 AND parent.deleted_at=0 AND parent.erased_at=0))) ORDER BY e.uid";
/// One-click entry: no reply required; otherwise the same conditions.
const CLICK_ELIGIBLE: &str = "SELECT e.uid FROM gonof_forum_lottery_signup e JOIN gonof_common_member m ON m.uid=e.uid WHERE e.tid=? AND e.uid<>? AND e.dateline<? AND m.`freeze`=0 AND m.account_state='active' AND NOT EXISTS(SELECT 1 FROM gonof_common_data_request d WHERE d.uid=e.uid AND d.kind IN ('close','erase') AND d.status IN ('cooling','pending','processing','failed')) AND NOT EXISTS(SELECT 1 FROM gonof_forum_lottery_exclusion x WHERE x.tid=e.tid AND x.uid=e.uid) ORDER BY e.uid";

async fn draw_lottery(db: &Database, tid: i64, owner: i64, now: i64) -> Result<Draw> {
    let mut tx = db.begin_content_write().await?;
    lock_members(db, &mut tx, &[owner]).await?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    let a=crate::db::query("SELECT a.owner_uid,a.state,a.deadline,a.budget,a.remaining,l.shares,l.entry_mode FROM gonof_forum_activity a LEFT JOIN gonof_forum_lottery l ON l.tid=a.tid WHERE a.tid=? FOR UPDATE").bind(tid).fetch_one(db.observe(&mut *tx)).await?;
    let state: String = crate::db::text(&a, "state")?;
    let deadline: i64 = a.try_get("deadline")?;
    if !["open", "drawing"].contains(&state.as_str()) || deadline > now {
        tx.commit().await?;
        return Ok(Draw::Finished(false));
    }
    valid(
        a.try_get::<i64, _>("owner_uid")? == owner,
        "the activity's author changed",
    )?;
    let remaining: i64 = a.try_get("remaining")?;
    let readable = readable_thread(&thread)?;
    let click = crate::db::text(&a, "entry_mode")? == "click";
    let table = super::lottery::entry_table(if click { "click" } else { "reply" });
    if state == "drawing" {
        let draw = crate::db::query(
            "SELECT seed,winners FROM gonof_forum_activity_draw WHERE tid=? FOR UPDATE",
        )
        .bind(tid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        let prizes = prizes_locked(db, &mut tx, tid).await?;
        let decided: Vec<i64> = serde_json::from_str(&crate::db::text(&draw, "winners")?)?;
        let fresh = decided.is_empty();
        if fresh && !readable {
            close_unpaid(db, &mut tx, tid, owner, remaining, readable, now).await?;
            tx.commit().await?;
            return Ok(Draw::Finished(true));
        }
        let winners = if fresh {
            // Consistent read: the entrants' member rows are not locked. This transaction is READ COMMITTED, so each
            // statement sees the latest commit: entering and removing both require the activity to still be open, so
            // they must have committed before step 0 and are always visible here; the thread row lock queues later
            // moderation writes behind this transaction.
            let eligible: Vec<i64> = if click {
                crate::db::query_scalar(CLICK_ELIGIBLE)
                    .bind(tid)
                    .bind(owner)
                    .bind(deadline)
                    .fetch_all(db.observe(&mut *tx))
                    .await?
            } else {
                crate::db::query_scalar(REPLY_ELIGIBLE)
                    .bind(tid)
                    .bind(owner)
                    .bind(deadline)
                    .bind(deadline)
                    .fetch_all(db.observe(&mut *tx))
                    .await?
            };
            let shares: i64 = a.try_get("shares")?;
            let seed = crate::db::text(&draw, "seed")?;
            let winners = select_winners(&eligible, shares as usize, &seed);
            crate::db::query(
                "UPDATE gonof_forum_activity_draw SET eligible=?,winners=?,dateline=? WHERE tid=?",
            )
            .bind(serde_json::to_string(&eligible)?)
            .bind(serde_json::to_string(&winners)?)
            .bind(now)
            .bind(tid)
            .execute(db.observe(&mut *tx))
            .await?;
            winners
        } else {
            decided
        };
        let allocations = allocate(&winners, &prizes);
        let payouts = payouts(&allocations);
        if fresh {
            // Credits for the newly fixed list must not exceed the frozen budget; on resume, `paid_count` checks
            // progress.
            let paid = payouts
                .iter()
                .try_fold(0_i64, |sum, (_, amount)| sum.checked_add(*amount))
                .ok_or_else(|| problem("activity budget out of range"))?;
            valid(paid <= remaining, "activity budget inconsistent")?;
            // Redemption codes and other prize slots are assigned in the same transaction that writes the list.
            for allocation in allocations.iter().filter(|a| a.kind != "credit") {
                let assigned = crate::db::query("UPDATE gonof_forum_lottery_award SET uid=?,assigned_at=? WHERE tid=? AND prize_no=? AND slot_no=? AND uid IS NULL")
                    .bind(allocation.uid)
                    .bind(now)
                    .bind(tid)
                    .bind(allocation.prize)
                    .bind(allocation.slot)
                    .execute(db.observe(&mut *tx))
                    .await?;
                valid(assigned.rows_affected() == 1, "prize slots inconsistent")?;
            }
        }
        if !payouts.is_empty() {
            tx.commit().await?;
            return Ok(Draw::Decided { payouts, table });
        }
        // No credits to pay (nobody eligible, or all winners got redemption codes or other prizes): refund the whole
        // budget and mark the activity as settled.
        ledger::refund(db, &mut tx, owner, tid, remaining, now).await?;
        crate::db::query(
            "UPDATE gonof_forum_activity SET state=?,remaining=0,settled_at=? WHERE tid=?",
        )
        .bind(super::invariants::State::Settled.value())
        .bind(now)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
        tx.commit().await?;
        return Ok(Draw::Finished(true));
    }
    close_unpaid(db, &mut tx, tid, owner, remaining, readable, now).await?;
    tx.commit().await?;
    Ok(Draw::Finished(true))
}

/// Pays one batch of credits. `Some(settled by this call)` means the activity is finished; `None` means another
/// batch follows.
async fn pay_winners(
    db: &Database,
    tid: i64,
    owner: i64,
    payouts: &[(i64, i64)],
    table: &str,
    now: i64,
) -> Result<Option<bool>> {
    // First read progress without locks to decide which members this batch locks; member rows must be locked
    // before the content locks.
    let progress =
        crate::db::query("SELECT state,budget,remaining FROM gonof_forum_activity WHERE tid=?")
            .bind(tid)
            .fetch_one(db.executor())
            .await?;
    if crate::db::text(&progress, "state")? != "drawing" {
        return Ok(Some(false));
    }
    let start = paid_count(
        progress.try_get("budget")?,
        progress.try_get("remaining")?,
        payouts,
    )?;
    let batch = &payouts[start..payouts.len().min(start + DRAW_BATCH)];
    let mut tx = db.begin_content_write().await?;
    let mut accounts = vec![owner];
    accounts.extend(batch.iter().map(|(uid, _)| *uid));
    lock_members(db, &mut tx, &accounts).await?;
    lock_thread(db, &mut tx, tid).await?;
    let a = crate::db::query(
        "SELECT owner_uid,state,budget,remaining FROM gonof_forum_activity WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if crate::db::text(&a, "state")? != "drawing" {
        tx.commit().await?;
        return Ok(Some(false));
    }
    valid(
        a.try_get::<i64, _>("owner_uid")? == owner,
        "the activity's author changed",
    )?;
    let remaining: i64 = a.try_get("remaining")?;
    let budget: i64 = a.try_get("budget")?;
    if paid_count(budget, remaining, payouts)? != start {
        // Another process paid this batch while we waited for the lock; start over with the new progress.
        tx.commit().await?;
        return Ok(None);
    }
    for (uid, amount) in batch {
        ledger::transfer(
            db,
            &mut tx,
            owner,
            *uid,
            *amount,
            &format!("activity:{tid}:draw:{uid}"),
            tid,
            None,
            now,
        )
        .await?;
    }
    // The entry-record and activity-row updates come last, merged into one statement to save round trips and
    // shorten lock time.
    let cases = vec!["WHEN ? THEN ?"; batch.len()].join(" ");
    let placeholders = vec!["?"; batch.len()].join(",");
    let sql = format!(
        "UPDATE {table} SET amount=CASE uid {cases} END WHERE tid=? AND amount=0 AND uid IN ({placeholders})"
    );
    let mut update = crate::db::query(&sql);
    for (uid, amount) in batch {
        update = update.bind(*uid).bind(*amount);
    }
    update = update.bind(tid);
    for (uid, _) in batch {
        update = update.bind(*uid);
    }
    let updated = update.execute(db.observe(&mut *tx)).await?;
    valid(
        updated.rows_affected() == batch.len() as u64,
        "lottery payout progress inconsistent",
    )?;
    let left = remaining - batch.iter().map(|(_, amount)| amount).sum::<i64>();
    if start + batch.len() == payouts.len() {
        ledger::refund(db, &mut tx, owner, tid, left, now).await?;
        crate::db::query(
            "UPDATE gonof_forum_activity SET state=?,remaining=0,settled_at=? WHERE tid=?",
        )
        .bind(super::invariants::State::Settled.value())
        .bind(now)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
        tx.commit().await?;
        return Ok(Some(true));
    }
    super::invariants::check_remaining(budget, left)?;
    crate::db::query("UPDATE gonof_forum_activity SET remaining=? WHERE tid=?")
        .bind(left)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(None)
}

/// Number of people already paid: after the draw only the payout reduces `remaining`, paying in list order with
/// every amount ≥ 1, so the number of people with "amount prefix sum = budget - remaining" is unique. A mismatch
/// means the ledger is inconsistent.
fn paid_count(budget: i64, remaining: i64, payouts: &[(i64, i64)]) -> Result<usize> {
    let spent = budget - remaining;
    valid(spent >= 0, "activity budget inconsistent")?;
    let mut sum = 0_i64;
    for (count, (_, amount)) in payouts.iter().enumerate() {
        if sum == spent {
            return Ok(count);
        }
        sum = sum
            .checked_add(*amount)
            .ok_or_else(|| problem("activity budget inconsistent"))?;
        if sum > spent {
            break;
        }
    }
    Err(problem("activity budget inconsistent"))
}

/// Draws the winner list with a fixed, reproducible algorithm.
///
/// The algorithm is hard-coded and does not depend on implementations such as `StdRng` or `SliceRandom` whose
/// output is not guaranteed to stay the same across versions:
///
/// 1. Random source: the SHA-256 of the draw seed (`gonof_forum_activity_draw.seed`) is the 32-byte ChaCha20
///    seed, `rand_chacha` is pinned at 0.3.1, and numbers are taken one by one with `next_u64`.
/// 2. List: eligible members in ascending uid order (the draw query `ORDER BY e.uid`; the `eligible` field of the
///    draw record stores exactly this list).
/// 3. Selection: run the first `shares` steps of Fisher–Yates on the list; step i picks a position in `[i, n)`
///    and swaps it with i. Uniform integers in a range use rejection sampling, discarding the tail that would
///    cause bias, see [`uniform`]. The winner list is in the order drawn.
///
/// Running the three steps above on the seed and `eligible` from the draw record yields the same `winners`.
pub(super) fn select_winners(eligible: &[i64], shares: usize, seed: &str) -> Vec<i64> {
    let mut rng = ChaCha20Rng::from_seed(Sha256::digest(seed.as_bytes()).into());
    let mut pool = eligible.to_vec();
    let take = shares.min(pool.len());
    for i in 0..take {
        let span = (pool.len() - i) as u64;
        let pick = i + uniform(&mut rng, span) as usize;
        pool.swap(i, pick);
    }
    pool.truncate(take);
    pool
}

/// A uniform integer on `[0, bound)`. Take a 64-bit number; if it falls in the final segment that does not make up
/// a full round of `bound`, draw again; otherwise reduce it modulo `bound`.
fn uniform(rng: &mut ChaCha20Rng, bound: u64) -> u64 {
    debug_assert!(bound > 0);
    // Remainder of 2^64 divided by bound; the largest `tail` values would cause bias.
    let tail = (u64::MAX % bound + 1) % bound;
    loop {
        let value = rng.next_u64();
        if value <= u64::MAX - tail {
            return value % bound;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draw_is_unique_bounded_and_replayable() {
        let eligible: Vec<i64> = (1..=100).collect();
        let a = select_winners(&eligible, 10, "saved-seed");
        assert_eq!(a, select_winners(&eligible, 10, "saved-seed"));
        assert_eq!(a.len(), 10);
        assert_eq!(a.iter().collect::<BTreeSet<_>>().len(), 10);
        assert_eq!(select_winners(&[1, 2], 10, "s").len(), 2);
        assert!(select_winners(&[], 5, "s").is_empty());
    }

    /// The selection result for a known seed and list is hard-coded here: if a dependency upgrade or algorithm
    /// change alters the result, this fails, because old draw records could no longer reproduce their original
    /// lists.
    #[test]
    fn draw_results_are_fixed_for_known_seeds() {
        let eligible: Vec<i64> = (1..=20).collect();
        assert_eq!(
            select_winners(&eligible, 5, "saved-seed"),
            EXPECTED_SAVED_SEED
        );
        let seed = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let members = [3, 8, 15, 16, 23, 42, 108, 4815, 162342];
        assert_eq!(select_winners(&members, 3, seed), EXPECTED_HEX_SEED);
        assert_eq!(select_winners(&members, 20, seed).len(), members.len());
    }

    // The expected values were computed independently in Python with the RFC 7539 ChaCha20 block function (first
    // checked against the known first block for an all-zero key) using the three steps above, and match this
    // implementation.
    const EXPECTED_SAVED_SEED: [i64; 5] = [14, 10, 9, 5, 1];
    const EXPECTED_HEX_SEED: [i64; 3] = [15, 42, 16];

    fn plan(list: &[(&str, i64, i64)]) -> Vec<Prize> {
        list.iter()
            .enumerate()
            .map(|(index, (kind, winners, amount))| Prize {
                number: index as i64 + 1,
                kind: (*kind).into(),
                winners: *winners,
                amount: *amount,
            })
            .collect()
    }

    /// Multi-prize assignment is hard-coded too: a known seed, list and prizes give a fixed "who gets which slot of
    /// which prize". Selection is still the same `select_winners`; assignment
    /// only slices it by prize order.
    #[test]
    fn prize_allocation_is_fixed_for_a_known_seed() {
        let eligible: Vec<i64> = (1..=20).collect();
        let prizes = plan(&[("code", 1, 0), ("credit", 2, 30), ("item", 2, 0)]);
        let winners = select_winners(&eligible, 5, "saved-seed");
        assert_eq!(winners, EXPECTED_SAVED_SEED);
        let got: Vec<(i64, i64, i64)> = allocate(&winners, &prizes)
            .iter()
            .map(|a| (a.uid, a.prize, a.slot))
            .collect();
        assert_eq!(
            got,
            [(14, 1, 1), (10, 2, 1), (9, 2, 2), (5, 3, 1), (1, 3, 2)]
        );
        assert_eq!(payouts(&allocate(&winners, &prizes)), [(10, 30), (9, 30)]);
    }

    #[test]
    fn short_lists_leave_later_prizes_empty() {
        let prizes = plan(&[("credit", 2, 5), ("code", 3, 0)]);
        let got = allocate(&[7, 8, 9], &prizes);
        assert_eq!(
            got.iter()
                .map(|a| (a.uid, a.prize, a.slot))
                .collect::<Vec<_>>(),
            [(7, 1, 1), (8, 1, 2), (9, 2, 1)]
        );
        assert!(allocate(&[], &prizes).is_empty());
    }

    #[test]
    fn payout_progress_follows_prefix_sums() {
        let payouts = [(1, 50), (2, 50), (3, 10), (4, 10)];
        assert_eq!(paid_count(120, 120, &payouts).unwrap(), 0);
        assert_eq!(paid_count(120, 70, &payouts).unwrap(), 1);
        assert_eq!(paid_count(120, 20, &payouts).unwrap(), 2);
        assert_eq!(paid_count(120, 10, &payouts).unwrap(), 3);
        // When everything is paid the activity is settled and progress should not be asked for again; any mismatched
        // balance means the ledger is inconsistent.
        assert!(paid_count(120, 0, &payouts).is_err());
        assert!(paid_count(120, 65, &payouts).is_err());
        assert!(paid_count(120, 130, &payouts).is_err());
        // When slots go unfilled the budget exceeds what is owed, and the rest is refunded in the last batch.
        assert!(paid_count(200, 180, &payouts).is_err());
        assert_eq!(paid_count(200, 200, &payouts).unwrap(), 0);
    }

    #[test]
    fn uniform_integers_stay_in_range() {
        let mut rng = ChaCha20Rng::from_seed([7; 32]);
        for bound in [1_u64, 2, 3, 7, 1000, u64::MAX] {
            for _ in 0..100 {
                assert!(uniform(&mut rng, bound) < bound);
            }
        }
    }
}
