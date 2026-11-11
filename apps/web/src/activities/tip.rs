//! Thread tips: limited tips from members to the thread author, bound to a thread.
//!
//! There is no tips table: payment and receipt each write one entry in the existing credits ledger (`tip_out` /
//! `tip_in`), both carrying the thread and first post. The event key `tip:{tid}:{payer}:out|in` is unique, so each
//! member can tip a thread only once, and neither concurrency nor a replay after the request row expires records an
//! extra entry. Tiers, the daily limit and the switch are the scalar settings `settings.tip.*`
//! (docs/settings-and-caching.md, "Write gate and locks"): the tip transaction uses the values the write gate read,
//! and saving goes through [`crate::runtime::update`], locking all scalar settings rows like the other settings pages.
use super::*;

pub use crate::runtime::{MAX_TIERS, TipSettings};

/// Parses the tiers from the admin form: comma-separated, whitespace allowed around commas; value rules are validated
/// uniformly with the settings at save time.
pub fn parse_amounts(text: &str) -> Option<Vec<i64>> {
    let amounts = text
        .split(',')
        .map(|part| part.trim().parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()?;
    crate::runtime::valid_tip_amounts(&amounts).then_some(amounts)
}

pub fn amounts_text(amounts: &[i64]) -> String {
    crate::runtime::list_text(amounts)
}

/// Saves the tip settings from the admin area. The daily limit cannot be less than the largest tier, or that tier could
/// never be used (same as the reward rules); this rule and the tier rules both live in the settings
/// validation (`runtime::settings`), shared by reads and writes.
pub async fn save_tip_settings(
    db: &Database,
    global: &crate::cache::GlobalCache,
    actor: Actor,
    enabled: bool,
    amounts: &str,
    daily_limit: i64,
) -> Result<()> {
    let amounts = parse_amounts(amounts)
        .ok_or_else(|| problem(crate::i18n::current().config_check_tip_amounts()))?;
    match crate::runtime::update(db, global, actor, "credits.tip", |settings| {
        settings.tip.enabled = enabled;
        settings.tip.amounts = amounts;
        settings.tip.daily_limit = daily_limit;
    })
    .await
    {
        Ok(_) => Ok(()),
        Err(crate::identity::IdentityError::Validation(message)) => Err(problem(message)),
        Err(crate::identity::IdentityError::Internal(error)) => Err(error),
        Err(other) => Err(anyhow!(other)),
    }
}

pub(crate) fn out_key(tid: i64, payer: i64) -> String {
    format!("tip:{tid}:{payer}:out")
}

/// Tips the thread author. `now` only lets tests fix the time; the web uses [`tip_live`], which takes the program time
/// after locking.
pub async fn tip(
    db: &Database,
    actor: Actor,
    tid: i64,
    amount: i64,
    key: &str,
    now: i64,
) -> Result<()> {
    retry_transaction(|| tip_inner(db, actor, tid, amount, key, Some(now))).await
}

pub async fn tip_live(db: &Database, actor: Actor, tid: i64, amount: i64, key: &str) -> Result<()> {
    retry_transaction(|| tip_inner(db, actor, tid, amount, key, None)).await
}

/// Lock order matches bounty acceptance: scalar settings (including tips, shared) → both members in uid order → forum
/// (shared) → thread and first post (exclusive) → both credits accounts in uid order. Discovery reads only decide what
/// to lock; everything is rechecked under the locks.
async fn tip_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    amount: i64,
    key: &str,
    timestamp: Option<i64>,
) -> Result<()> {
    valid(
        tid > 0 && crate::identity::valid_request_key(key),
        crate::i18n::current().activity_error_tip_request(),
    )?;
    let owner = discover_owner(db, tid)
        .await?
        .ok_or_else(|| problem(crate::i18n::current().topic_error_not_found()))?;
    let first_pid: i64 =
        crate::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE first_tid=?")
            .bind(tid)
            .fetch_optional(db.executor())
            .await?
            .ok_or_else(|| problem(crate::i18n::current().topic_error_not_found()))?;
    valid(
        owner != actor.uid,
        crate::i18n::current().activity_error_tip_own(),
    )?;
    let mut tx = db.begin_write().await?;
    let site = guard_write(db, &mut tx).await?;
    // The daily limit accumulates per calendar day in the site time zone, taken from the same gate snapshot as the tip
    // settings.
    let offset_minutes = site.time.offset_minutes;
    let settings = site.tip;
    lock_members(db, &mut tx, &[actor.uid, owner]).await?;
    reject_muted(actor_locked(db, &mut tx, actor, false).await?)?;
    let hash = crate::identity::token_hash(
        &serde_json::json!({"operation":"credit.tip","tid":tid,"amount":amount}).to_string(),
    );
    if let Some(row) = crate::db::query(
        "SELECT request_hash FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        valid(
            crate::db::text(&row, "request_hash")? == hash,
            crate::i18n::current().activity_error_request_reused(),
        )?;
        tx.commit().await?;
        return Ok(());
    }
    valid(
        settings.enabled,
        crate::i18n::current().activity_error_tip_off(),
    )?;
    valid(
        settings.amounts.contains(&amount),
        crate::i18n::current().activity_error_tip_amount(),
    )?;
    // A thread author who is frozen, being deleted or deleted cannot receive tips (same as bounty acceptance).
    let author =
        crate::db::query("SELECT `freeze`,account_state FROM gonof_common_member WHERE uid=?")
            .bind(owner)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    valid(
        author.try_get::<i64, _>("freeze")? == 0
            && crate::db::text(&author, "account_state")? == "active"
            && crate::privacy::funding_allowed(db, &mut tx, owner).await?,
        crate::i18n::current().activity_error_tip_recipient(),
    )?;
    funding(db, &mut tx, actor.uid).await?;
    let target = crate::interaction::social::lock_target_for(db, &mut tx, first_pid, actor.uid)
        .await
        .map_err(|error| match error {
            crate::content::writes::ContentWriteError::NotFound => {
                problem(crate::i18n::current().editor_error_thread())
            }
            other => anyhow!(other),
        })?;
    // `visible` already covers: readable by the member, not hidden by moderation, and the member has not blocked the
    // thread author.
    valid(
        target.first && target.tid == tid && target.authorid == Some(owner) && target.visible,
        crate::i18n::current().editor_error_thread(),
    )?;
    valid(
        !target.withdrawn,
        crate::i18n::current().activity_error_tip_withdrawn(),
    )?;
    valid(
        !crate::interaction::blocked_by(db, &mut tx, owner, actor.uid).await?,
        crate::i18n::current().activity_error_tip_blocked(),
    )?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    let out = out_key(tid, actor.uid);
    valid(
        crate::db::query("SELECT id FROM gonof_common_credit_ledger WHERE event_key=?")
            .bind(&out)
            .fetch_optional(db.observe(&mut *tx))
            .await?
            .is_none(),
        crate::i18n::current().activity_error_tipped(),
    )?;
    // Outgoing amounts in the ledger are negative; the member row lock already serializes tips by the same member.
    let spent = crate::db::query_scalar::<Option<i64>>("SELECT CAST(SUM(available_delta) AS SIGNED) FROM gonof_common_credit_ledger WHERE uid=? AND kind=? AND dateline>=? AND dateline<?")
        .bind(actor.uid)
        .bind("tip_out")
        .bind(crate::clock::day_start(now, offset_minutes))
        .bind(crate::clock::day_start(now, offset_minutes) + 86400)
        .fetch_one(db.observe(&mut *tx))
        .await?
        .unwrap_or(0);
    valid(
        spent
            .checked_neg()
            .and_then(|spent| spent.checked_add(amount))
            .is_some_and(|total| total <= settings.daily_limit),
        crate::i18n::current().activity_error_tip_daily(),
    )?;
    ledger::accounts(db, &mut tx, &[actor.uid, owner]).await?;
    ledger::apply(
        db,
        &mut tx,
        actor.uid,
        &out,
        -amount,
        0,
        "tip_out",
        Some(tid),
        Some(first_pid),
        None,
        ledger::system_reason("tip_out", crate::i18n::current()).unwrap_or_default(),
        now,
    )
    .await?;
    ledger::apply(
        db,
        &mut tx,
        owner,
        &format!("tip:{tid}:{}:in", actor.uid),
        amount,
        0,
        "tip_in",
        Some(tid),
        Some(first_pid),
        None,
        ledger::system_reason("tip_in", crate::i18n::current()).unwrap_or_default(),
        now,
    )
    .await?;
    // The thread's tip count and total (counter columns): the thread row is already exclusively locked; they are added
    // after both ledger entries are written.
    crate::interaction::counters::add_tip(db, &mut tx, tid, amount).await?;
    crate::identity::rules::insert_post_request(db, &mut tx, actor.uid, key, &hash, first_pid, now)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Tipper {
    pub uid: i64,
    pub username: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TipSummary {
    pub count: i64,
    pub total: i64,
    /// The 10 most recent tippers, newest first.
    pub recent: Vec<Tipper>,
}

pub const RECENT_TIPPERS: i64 = 10;

/// Tip summary for the thread page: count and total come from the counter columns on the thread row (supplied by the
/// caller from the thread details); only when there are tips are the recent tippers read via
/// `gonof_credit_thread_kind (tid, kind, id)`.
pub async fn tip_summary(
    db: &Database,
    tid: i64,
    (count, total): (i64, i64),
) -> Result<TipSummary> {
    if count == 0 {
        return Ok(TipSummary::default());
    }
    let recent = crate::db::query("SELECT l.uid,m.username,m.account_state FROM gonof_common_credit_ledger l JOIN gonof_common_member m ON m.uid=l.uid WHERE l.tid=? AND l.kind='tip_out' ORDER BY l.id DESC LIMIT ?")
        .bind(tid)
        .bind(RECENT_TIPPERS)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| {
            let deleted = crate::db::text(&row, "account_state")? == "deleted";
            Ok(Tipper {
                uid: row.try_get("uid")?,
                username: if deleted {
                    crate::i18n::current().common_deleted_user().into()
                } else {
                    crate::db::text(&row, "username")?
                },
                deleted,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TipSummary {
        count,
        total,
        recent,
    })
}

/// Whether the member has already tipped this thread: a point lookup on the ledger's unique key.
pub async fn tipped(db: &Database, tid: i64, uid: i64) -> Result<bool> {
    Ok(
        crate::db::query("SELECT id FROM gonof_common_credit_ledger WHERE event_key=?")
            .bind(out_key(tid, uid))
            .fetch_optional(db.executor())
            .await?
            .is_some(),
    )
}

/// Whether the thread author can receive tips now (used by the page to decide whether to show the button; no locks;
/// the transaction rechecks).
pub async fn can_receive(db: &Database, uid: i64) -> Result<bool> {
    let row =
        crate::db::query("SELECT `freeze`,account_state FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.executor())
            .await?;
    Ok(match row {
        Some(row) => {
            row.try_get::<i64, _>("freeze")? == 0
                && crate::db::text(&row, "account_state")? == "active"
        }
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_must_be_ascending_positive_and_bounded() {
        assert_eq!(parse_amounts("1,5,10,20"), Some(vec![1, 5, 10, 20]));
        assert_eq!(parse_amounts(" 2 , 3 "), Some(vec![2, 3]));
        for bad in [
            "",
            "0",
            "-1",
            "5,1",
            "1,1",
            "1,a",
            "1,,2",
            "1,2,3,4,5,6,7,8,9",
            "1000000001",
        ] {
            assert_eq!(parse_amounts(bad), None, "{bad:?} must be rejected");
        }
        assert_eq!(amounts_text(&[1, 5, 10]), "1,5,10");
    }
}
