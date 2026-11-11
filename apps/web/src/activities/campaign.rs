use super::*;
use crate::db::Tx;
use rand::Rng;

/// `ring` encrypts the lottery's redemption codes; it may be `None` when there are no redemption-code prizes. `credit`
/// is the site's name for its points in this request's language (messages and the shorthand lottery prize).
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    tid: i64,
    spec: &ActivitySpec,
    ring: Option<&crate::identity::TwoFactorConfig>,
    credit: &str,
    now: i64,
) -> Result<()> {
    spec.validate(now, credit)?;
    if spec.kind == 0 {
        return Ok(());
    }
    let monetary = (1..=3).contains(&spec.kind);
    // A lottery only freezes credit prizes; when every prize is a redemption code or another item the budget is 0
    // and no freeze ledger entry is written.
    let budget = match spec.kind {
        3 => prize_totals(&spec.lottery_prizes(credit)).0,
        1 | 2 => spec.budget,
        _ => 0,
    };
    if monetary {
        funding(db, tx, uid).await?;
    }
    if budget > 0 {
        ledger::accounts(db, tx, &[uid]).await?;
        ledger::apply(
            db,
            tx,
            uid,
            &format!("activity:{tid}:reserve"),
            -budget,
            budget,
            "activity_reserve",
            Some(tid),
            None,
            None,
            ledger::system_reason("activity_reserve", crate::i18n::current()).unwrap_or_default(),
            now,
        )
        .await?;
    }
    // The caller has just inserted the thread row in this transaction; the activity's category and initiator must be
    // exactly the thread's category and author.
    let thread = crate::db::query("SELECT category,authorid FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or_else(|| anyhow!("the activity's thread does not exist"))?;
    invariants::check_identity(
        spec.kind,
        uid,
        thread.try_get("category")?,
        thread.try_get("authorid")?,
    )?;
    invariants::check_remaining(budget, budget)?;
    crate::db::query("INSERT INTO gonof_forum_activity(tid,owner_uid,kind,budget,remaining,deadline,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(tid).bind(uid).bind(spec.kind).bind(budget).bind(budget).bind(if spec.kind==5{0}else{spec.deadline}).bind(now).execute(db.observe(&mut **tx)).await?;
    match spec.kind {
        1 => {
            crate::db::query("INSERT INTO gonof_forum_reward(tid) VALUES(?)")
                .bind(tid)
                .execute(db.observe(&mut **tx))
                .await?;
        }
        2 => {
            let distribution = invariants::Distribution::parse(&spec.distribution)
                .ok_or_else(|| anyhow!("invalid red packet distribution"))?;
            invariants::check_packet_shares(spec.shares, spec.shares)?;
            crate::db::query("INSERT INTO gonof_forum_redpacket(tid,shares,remaining_shares,distribution) VALUES(?,?,?,?)").bind(tid).bind(spec.shares).bind(spec.shares).bind(distribution.value()).execute(db.observe(&mut **tx)).await?;
        }
        3 => lottery::publish(db, tx, tid, spec, ring, credit).await?,
        4 => {
            crate::db::query("INSERT INTO gonof_forum_poll(tid,max_choices) VALUES(?,?)")
                .bind(tid)
                .bind(spec.max_choices)
                .execute(db.observe(&mut **tx))
                .await?;
        }
        5 => {
            crate::db::query("INSERT INTO gonof_forum_news(tid,source_url,source_name,source_author) VALUES(?,?,?,?)").bind(tid).bind(&spec.source_url).bind(&spec.source_name).bind(&spec.source_author).execute(db.observe(&mut **tx)).await?;
        }
        _ => unreachable!(),
    }
    if spec.kind == 4 {
        for (index, label) in spec.options.iter().enumerate() {
            crate::db::query(
                "INSERT INTO gonof_forum_poll_option(tid,option_no,label) VALUES(?,?,?)",
            )
            .bind(tid)
            .bind(index as i64 + 1)
            .bind(label.trim())
            .execute(db.observe(&mut **tx))
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn eligible_reply(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    pid: i64,
) -> Result<Option<i64>> {
    let row=crate::db::query("SELECT p.authorid FROM gonof_forum_post p LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid WHERE p.tid=? AND p.pid=? AND p.first=0 AND p.invisible=0 AND p.deleted_at=0 AND p.erased_at=0 AND (p.root_pid IS NULL OR (parent.invisible=0 AND parent.deleted_at=0 AND parent.erased_at=0)) FOR UPDATE").bind(tid).bind(pid).fetch_optional(db.observe(&mut **tx)).await?;
    Ok(row
        .map(|r| r.try_get::<Option<i64>, _>("authorid"))
        .transpose()?
        .flatten())
}
pub async fn on_reply(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    tid: i64,
    pid: i64,
    now: i64,
) -> Result<()> {
    let Some(a)=crate::db::query("SELECT a.owner_uid,a.kind,a.state,a.deadline,a.budget,a.remaining,r.shares,r.remaining_shares,r.distribution,l.entry_mode FROM gonof_forum_activity a LEFT JOIN gonof_forum_redpacket r ON r.tid=a.tid LEFT JOIN gonof_forum_lottery l ON l.tid=a.tid WHERE a.tid=? FOR UPDATE").bind(tid).fetch_optional(db.observe(&mut **tx)).await? else{return Ok(())};
    let kind: i64 = a.try_get("kind")?;
    let owner: i64 = a.try_get("owner_uid")?;
    // A one-click-entry lottery does not take entries through replies.
    if kind == 3 && a.try_get::<Option<String>, _>("entry_mode")?.as_deref() == Some("click") {
        return Ok(());
    }
    if owner == uid
        || crate::db::text(&a, "state")? != "open"
        || now >= a.try_get::<i64, _>("deadline")?
    {
        return Ok(());
    }
    if (1..=3).contains(&kind) {
        funding(db, tx, uid).await?;
    }
    if ![2, 3].contains(&kind) {
        return Ok(());
    }
    if eligible_reply(db, tx, tid, pid).await? != Some(uid) {
        return Ok(());
    }
    let entry_table = if kind == 2 {
        "gonof_forum_redpacket_claim"
    } else {
        "gonof_forum_lottery_entry"
    };
    if crate::db::query(&format!(
        "SELECT uid FROM {entry_table} WHERE tid=? AND uid=?"
    ))
    .bind(tid)
    .bind(uid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .is_some()
    {
        return Ok(());
    }
    let amount = if kind == 2 {
        let remaining: i64 = a.try_get("remaining")?;
        let shares: i64 = a.try_get("remaining_shares")?;
        if remaining <= 0 || shares <= 0 {
            return Ok(());
        }
        let amount = match invariants::Distribution::parse(&crate::db::text(&a, "distribution")?) {
            Some(invariants::Distribution::Equal) => {
                a.try_get::<i64, _>("budget")? / a.try_get::<i64, _>("shares")?
            }
            Some(invariants::Distribution::Random) => {
                random_share(remaining, shares, &mut rand::thread_rng())
            }
            None => return Err(anyhow!("invalid red packet distribution")),
        };
        invariants::check_claim(amount)?;
        amount
    } else {
        0
    };
    crate::db::query(&format!(
        "INSERT INTO {entry_table}(tid,uid,source_pid,amount,dateline) VALUES(?,?,?,?,?)"
    ))
    .bind(tid)
    .bind(uid)
    .bind(pid)
    .bind(amount)
    .bind(now)
    .execute(db.observe(&mut **tx))
    .await?;
    if kind == 2 {
        ledger::transfer(
            db,
            tx,
            owner,
            uid,
            amount,
            &format!("activity:{tid}:packet:{uid}"),
            tid,
            Some(pid),
            now,
        )
        .await?;
        let remaining = a.try_get::<i64, _>("remaining")? - amount;
        let shares = a.try_get::<i64, _>("remaining_shares")? - 1;
        invariants::check_remaining(a.try_get("budget")?, remaining)?;
        invariants::check_packet_shares(a.try_get("shares")?, shares)?;
        let state = if shares == 0 {
            invariants::State::Settled
        } else {
            invariants::State::Open
        };
        crate::db::query(
            "UPDATE gonof_forum_activity SET remaining=?,state=?,settled_at=? WHERE tid=?",
        )
        .bind(remaining)
        .bind(state.value())
        .bind(if shares == 0 { now } else { 0 })
        .bind(tid)
        .execute(db.observe(&mut **tx))
        .await?;
        crate::db::query("UPDATE gonof_forum_redpacket SET remaining_shares=? WHERE tid=?")
            .bind(shares)
            .bind(tid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    Ok(())
}

pub(super) fn random_share(remaining: i64, shares: i64, rng: &mut impl Rng) -> i64 {
    if shares == 1 {
        return remaining;
    }
    let max = (2 * (remaining / shares))
        .max(1)
        .min(remaining - shares + 1);
    rng.gen_range(1..=max)
}

/// The caller locks members, forum, thread and first post in the regular order.
/// Only invoke for hiding/withdrawing the first post; restoring never calls this.
pub async fn terminate(db: &Database, tx: &mut Tx<'_>, tid: i64, now: i64) -> Result<()> {
    let Some(a) = crate::db::query(
        "SELECT owner_uid,kind,state,remaining FROM gonof_forum_activity WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    else {
        return Ok(());
    };
    let state = crate::db::text(&a, "state")?;
    if a.try_get::<i64, _>("kind")? == 5 || !["open", "drawing"].contains(&state.as_str()) {
        return Ok(());
    }
    // Once the winner list is fixed, payout still completes: a lottery whose draw record already holds a winner list
    // is neither refunded nor cancelled, and the background job pays out the rest; one whose step 0 has committed but
    // whose list is not fixed yet is still cancelled and refunded.
    if state == "drawing" && lottery_decided(db, tx, tid).await? {
        return Ok(());
    }
    ledger::refund(
        db,
        tx,
        a.try_get("owner_uid")?,
        tid,
        a.try_get("remaining")?,
        now,
    )
    .await?;
    crate::db::query(
        "UPDATE gonof_forum_activity SET state=?,remaining=0,settled_at=? WHERE tid=?",
    )
    .bind(invariants::State::Cancelled.value())
    .bind(now)
    .bind(tid)
    .execute(db.observe(&mut **tx))
    .await?;
    if a.try_get::<i64, _>("kind")? == 2 {
        crate::db::query("UPDATE gonof_forum_redpacket SET remaining_shares=0 WHERE tid=?")
            .bind(tid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    Ok(())
}

/// Whether the draw record already holds a fixed winner list (`winners` is a non-empty array).
async fn lottery_decided(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<bool> {
    let winners: Option<String> = crate::db::query_scalar(
        "SELECT winners FROM gonof_forum_activity_draw WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    Ok(match winners {
        Some(winners) => !serde_json::from_str::<Vec<i64>>(&winners)?.is_empty(),
        None => false,
    })
}

pub async fn accept(
    db: &Database,
    actor: Actor,
    tid: i64,
    pid: i64,
    key: &str,
    now: i64,
) -> Result<()> {
    accept_inner(db, actor, tid, pid, key, Some(now)).await
}
pub async fn accept_live(db: &Database, actor: Actor, tid: i64, pid: i64, key: &str) -> Result<()> {
    retry_transaction(|| accept_inner(db, actor, tid, pid, key, None)).await
}
async fn accept_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    pid: i64,
    key: &str,
    timestamp: Option<i64>,
) -> Result<()> {
    valid(
        tid > 0 && pid > 0 && crate::identity::valid_request_key(key),
        crate::i18n::current().activity_error_accept_request(),
    )?;
    let owner = discover_owner(db, tid)
        .await?
        .ok_or_else(|| problem(crate::i18n::current().topic_error_not_found()))?;
    let recipient: Option<i64> =
        crate::db::query_scalar("SELECT authorid FROM gonof_forum_post WHERE pid=? AND tid=?")
            .bind(pid)
            .bind(tid)
            .fetch_optional(db.executor())
            .await?
            .flatten();
    let recipient =
        recipient.ok_or_else(|| problem(crate::i18n::current().activity_error_reply()))?;
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid, owner, recipient]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    valid(
        owner == actor.uid && recipient != owner,
        crate::i18n::current().activity_error_accept_owner(),
    )?;
    let a=crate::db::query("SELECT a.owner_uid,a.kind,a.state,a.deadline,a.remaining,r.accepted_pid FROM gonof_forum_activity a JOIN gonof_forum_reward r ON r.tid=a.tid WHERE a.tid=? FOR UPDATE").bind(tid).fetch_optional(db.observe(&mut *tx)).await?.ok_or_else(||problem(crate::i18n::current().activity_error_bounty()))?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        a.try_get::<i64, _>("kind")? == 1 && a.try_get::<i64, _>("owner_uid")? == owner,
        crate::i18n::current().activity_error_not_your_bounty(),
    )?;
    let hash = crate::identity::token_hash(&format!("bounty:{tid}:{pid}"));
    if let Some(r) = crate::db::query(
        "SELECT request_hash FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        valid(
            crate::db::text(&r, "request_hash")? == hash,
            crate::i18n::current().activity_error_request_reused(),
        )?;
        tx.commit().await?;
        return Ok(());
    }
    valid(
        readable_thread(&thread)?,
        crate::i18n::current().activity_error_thread(),
    )?;
    valid(
        crate::db::text(&a, "state")? == "open" && now < a.try_get::<i64, _>("deadline")?,
        crate::i18n::current().activity_error_bounty_ended(),
    )?;
    valid(
        eligible_reply(db, &mut tx, tid, pid).await? == Some(recipient),
        crate::i18n::current().activity_error_accept_valid(),
    )?;
    funding(db, &mut tx, actor.uid).await?;
    funding(db, &mut tx, recipient).await?;
    let frozen: i64 = crate::db::query_scalar("SELECT freeze FROM gonof_common_member WHERE uid=?")
        .bind(recipient)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    valid(
        frozen == 0,
        crate::i18n::current().activity_error_recipient(),
    )?;
    ledger::transfer(
        db,
        &mut tx,
        owner,
        recipient,
        a.try_get("remaining")?,
        &format!("activity:{tid}:bounty"),
        tid,
        Some(pid),
        now,
    )
    .await?;
    crate::db::query("UPDATE gonof_forum_reward SET accepted_pid=? WHERE tid=?")
        .bind(pid)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
    crate::growth::sync_posts(db, &mut tx, &[pid], crate::growth::Cause::Accept, now).await?;
    crate::db::query(
        "UPDATE gonof_forum_activity SET remaining=0,state=?,settled_at=? WHERE tid=?",
    )
    .bind(invariants::State::Settled.value())
    .bind(now)
    .bind(tid)
    .execute(db.observe(&mut *tx))
    .await?;
    crate::identity::rules::insert_post_request(db, &mut tx, actor.uid, key, &hash, pid, now)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn vote(
    db: &Database,
    actor: Actor,
    tid: i64,
    choices: Vec<i64>,
    key: &str,
    now: i64,
) -> Result<()> {
    vote_inner(db, actor, tid, choices, key, Some(now)).await
}
pub async fn vote_live(
    db: &Database,
    actor: Actor,
    tid: i64,
    choices: Vec<i64>,
    key: &str,
) -> Result<()> {
    retry_transaction(|| vote_inner(db, actor, tid, choices.clone(), key, None)).await
}
async fn vote_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    mut choices: Vec<i64>,
    key: &str,
    timestamp: Option<i64>,
) -> Result<()> {
    valid(
        tid > 0 && crate::identity::valid_request_key(key) && !choices.is_empty(),
        crate::i18n::current().activity_error_vote_choice(),
    )?;
    choices.sort_unstable();
    choices.dedup();
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    reject_muted(actor_locked(db, &mut tx, actor, false).await?)?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    let a = crate::db::query(
        "SELECT a.kind,a.state,a.deadline,p.max_choices FROM gonof_forum_activity a JOIN gonof_forum_poll p ON p.tid=a.tid WHERE a.tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or_else(|| problem(crate::i18n::current().activity_error_poll()))?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        a.try_get::<i64, _>("kind")? == 4,
        crate::i18n::current().activity_error_not_poll(),
    )?;
    let serialized = serde_json::to_string(&choices)?;
    if let Some(row) = crate::db::query(
        "SELECT choices,request_key FROM gonof_forum_poll_vote WHERE tid=? AND uid=?",
    )
    .bind(tid)
    .bind(actor.uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        valid(
            crate::db::text(&row, "choices")? == serialized
                && crate::db::text(&row, "request_key")? == key,
            crate::i18n::current().activity_error_voted(),
        )?;
        tx.commit().await?;
        return Ok(());
    }
    // A key names one request (docs/api.md, "Idempotency keys"): the same key already counted in another poll is a
    // different request, not a retry. The member's votes are found through the `uid` index.
    let reused: Option<i64> = crate::db::query_scalar(
        "SELECT tid FROM gonof_forum_poll_vote WHERE uid=? AND request_key=? AND tid<>? LIMIT 1",
    )
    .bind(actor.uid)
    .bind(key)
    .bind(tid)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    valid(
        reused.is_none(),
        crate::i18n::current().activity_error_request_reused(),
    )?;
    valid(
        readable_thread(&thread)?
            && crate::db::text(&a, "state")? == "open"
            && now < a.try_get::<i64, _>("deadline")?,
        crate::i18n::current().activity_error_poll_closed(),
    )?;
    valid(
        choices.len() <= a.try_get::<i64, _>("max_choices")? as usize,
        crate::i18n::current().activity_error_vote_max(),
    )?;
    let available: Vec<i64> = crate::db::query_scalar(
        "SELECT option_no FROM gonof_forum_poll_option WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_all(db.observe(&mut *tx))
    .await?;
    valid(
        choices.iter().all(|x| available.contains(x)),
        crate::i18n::current().activity_error_vote_option(),
    )?;
    crate::db::query(
        "INSERT INTO gonof_forum_poll_vote(tid,uid,choices,request_key,dateline) VALUES(?,?,?,?,?)",
    )
    .bind(tid)
    .bind(actor.uid)
    .bind(serialized)
    .bind(key)
    .bind(now)
    .execute(db.observe(&mut *tx))
    .await?;
    for choice in choices {
        crate::db::query(
            "UPDATE gonof_forum_poll_option SET votes=votes+1 WHERE tid=? AND option_no=?",
        )
        .bind(tid)
        .bind(choice)
        .execute(db.observe(&mut *tx))
        .await?;
    }
    crate::db::query("UPDATE gonof_forum_poll SET voters=voters+1 WHERE tid=?")
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Root publication already owns its member/forum/new-thread locks.
pub async fn publish_live(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    tid: i64,
    spec: &ActivitySpec,
    ring: Option<&crate::identity::TwoFactorConfig>,
    credit: &str,
) -> Result<()> {
    let now = locked_now();
    publish(db, tx, uid, tid, spec, ring, credit, now).await
}
/// Timestamp is sampled only after an activity lock wait has completed.
pub async fn on_reply_live(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    tid: i64,
    pid: i64,
) -> Result<()> {
    if crate::db::query("SELECT tid FROM gonof_forum_activity WHERE tid=? FOR UPDATE")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .is_none()
    {
        return Ok(());
    }
    let now = locked_now();
    on_reply(db, tx, uid, tid, pid, now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};
    #[test]
    fn random_packets_conserve_budget_and_keep_all_shares_positive() {
        let mut rng = StdRng::seed_from_u64(177);
        for total in 1..300 {
            for count in 1..=total.min(30) {
                let mut remaining = total;
                for shares in (1..=count).rev() {
                    let amount = random_share(remaining, shares, &mut rng);
                    assert!(amount >= 1);
                    assert!(amount <= remaining - shares + 1);
                    remaining -= amount;
                }
                assert_eq!(remaining, 0);
            }
        }
    }
}
