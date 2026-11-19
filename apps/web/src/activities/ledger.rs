use super::*;
use crate::db::Tx;

#[derive(Clone, Debug, Serialize)]
pub struct Rule {
    pub key: String,
    pub enabled: bool,
    pub amount: i64,
    pub daily_limit: i64,
    pub version: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct LedgerItem {
    pub id: i64,
    pub tid: Option<i64>,
    pub activity_kind: Option<i64>,
    pub available_delta: i64,
    pub frozen_delta: i64,
    pub available_after: i64,
    pub frozen_after: i64,
    pub kind: String,
    pub reason: String,
    pub dateline: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct CreditPage {
    pub available: i64,
    pub frozen: i64,
    pub entries: Vec<LedgerItem>,
    pub next_before: i64,
    pub checked_in: bool,
}

pub(super) async fn accounts(db: &Database, tx: &mut Tx<'_>, uids: &[i64]) -> Result<()> {
    for uid in uids.iter().copied().collect::<BTreeSet<_>>() {
        crate::db::query(
            "INSERT INTO gonof_common_credit_account(uid) VALUES(?) ON DUPLICATE KEY UPDATE uid=uid",
        )
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
        crate::db::query("SELECT uid FROM gonof_common_credit_account WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    }
    Ok(())
}

/// The reason of a ledger row the system creates, in `lang` (docs/architecture.md, "Languages"): the points page shows
/// it in the viewer's language instead of the stored `reason`, and new rows store it in the language of the moment.
/// `None` for administrator adjustments, whose reason a person wrote, and for unknown kinds.
pub fn system_reason(kind: &str, lang: crate::i18n::Lang) -> Option<&'static str> {
    Some(match kind {
        "signin" => lang.activity_reason_signin(),
        "thread" => lang.activity_reason_thread(),
        "reply" => lang.activity_reason_reply(),
        "activity_reserve" => lang.activity_reason_reserve(),
        "activity_payout" => lang.activity_reason_payout(),
        "activity_reward" => lang.activity_reason_reward(),
        "activity_refund" => lang.activity_reason_refund(),
        "tip_out" => lang.activity_reason_tip_out(),
        "tip_in" => lang.activity_reason_tip_in(),
        "seed_grant" => lang.activity_reason_seed(),
        _ => return None,
    })
}

/// A failure whose message names the site's points: the name (in this request's language) is read from the settings
/// only on these rare failing paths. Inside a transaction use [`credit_failure_in`].
pub(super) async fn credit_failure(
    db: &Database,
    message: impl FnOnce(crate::i18n::Lang, &str) -> String,
) -> anyhow::Error {
    let lang = crate::i18n::current();
    match crate::runtime::read(db).await {
        Ok(settings) => problem(&message(lang, &settings.site.credit_word(lang))),
        Err(error) => anyhow::Error::new(error),
    }
}

/// [`credit_failure`] inside a transaction: the name is read through it, without a lock. Asking the pool for a second
/// connection would wait on the one this transaction holds when the pool has only one, or when every connection is
/// held by a transaction failing the same way.
pub(super) async fn credit_failure_in(
    db: &Database,
    tx: &mut Tx<'_>,
    message: impl FnOnce(crate::i18n::Lang, &str) -> String,
) -> anyhow::Error {
    let lang = crate::i18n::current();
    let name = crate::db::query_scalar::<String>(
        "SELECT svalue FROM gonof_common_settings WHERE skey='settings.site.credit_name'",
    )
    .fetch_optional(db.observe(&mut **tx))
    .await;
    match name {
        Ok(name) => problem(&message(
            lang,
            &crate::runtime::settings::credit_word(&name.unwrap_or_default(), lang),
        )),
        Err(error) => anyhow::Error::new(error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    key: &str,
    available: i64,
    frozen: i64,
    kind: &str,
    tid: Option<i64>,
    pid: Option<i64>,
    operator: Option<i64>,
    reason: &str,
    now: i64,
) -> Result<bool> {
    if let Some(row)=crate::db::query("SELECT uid,available_delta,frozen_delta,kind,reason FROM gonof_common_credit_ledger WHERE event_key=?").bind(key).fetch_optional(db.observe(&mut **tx)).await? {
        // System rows store their reason in the language of the moment, so a replay in another language compares the
        // kind only; a written reason (administrator adjustments) must match as before.
        valid(row.try_get::<i64,_>("uid")?==uid && row.try_get::<i64,_>("available_delta")?==available && row.try_get::<i64,_>("frozen_delta")?==frozen && crate::db::text(&row, "kind")?==kind && (system_reason(kind, crate::i18n::Lang::EN).is_some() || crate::db::text(&row, "reason")?==reason),crate::i18n::current().activity_error_ledger_reused())?;
        return Ok(false);
    }
    if !crate::privacy::active_member(db, tx, uid).await? {
        return Err(
            credit_failure_in(db, tx, |lang, credit| lang.activity_error_deleting(credit)).await,
        );
    }
    let row = crate::db::query(
        "SELECT available,frozen FROM gonof_common_credit_account WHERE uid=? FOR UPDATE",
    )
    .bind(uid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    let Some(available_after) = row.try_get::<i64, _>("available")?.checked_add(available) else {
        return Err(credit_failure_in(db, tx, |lang, credit| {
            lang.activity_error_balance_range(credit)
        })
        .await);
    };
    let Some(frozen_after) = row.try_get::<i64, _>("frozen")?.checked_add(frozen) else {
        return Err(credit_failure_in(db, tx, |lang, credit| {
            lang.activity_error_frozen_range(credit)
        })
        .await);
    };
    if available_after < 0 || frozen_after < 0 {
        return Err(credit_failure_in(db, tx, |lang, credit| {
            lang.activity_error_insufficient(credit)
        })
        .await);
    }
    crate::db::query(
        "UPDATE gonof_common_credit_account SET available=?,frozen=?,version=version+1 WHERE uid=?",
    )
    .bind(available_after)
    .bind(frozen_after)
    .bind(uid)
    .execute(db.observe(&mut **tx))
    .await?;
    crate::db::query("INSERT INTO gonof_common_credit_ledger(event_key,uid,available_delta,frozen_delta,available_after,frozen_after,kind,tid,pid,operator_uid,reason,dateline) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(key).bind(uid).bind(available).bind(frozen).bind(available_after).bind(frozen_after).bind(kind).bind(tid).bind(pid).bind(operator).bind(reason).bind(now).execute(db.observe(&mut **tx)).await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn transfer(
    db: &Database,
    tx: &mut Tx<'_>,
    owner: i64,
    recipient: i64,
    amount: i64,
    key: &str,
    tid: i64,
    pid: Option<i64>,
    now: i64,
) -> Result<()> {
    valid(
        amount > 0 && owner != recipient,
        crate::i18n::current().activity_error_transfer(),
    )?;
    funding(db, tx, recipient).await?;
    accounts(db, tx, &[owner, recipient]).await?;
    apply(
        db,
        tx,
        owner,
        &format!("{key}:out"),
        0,
        -amount,
        "activity_payout",
        Some(tid),
        pid,
        None,
        system_reason("activity_payout", crate::i18n::current()).unwrap_or_default(),
        now,
    )
    .await?;
    apply(
        db,
        tx,
        recipient,
        &format!("{key}:in"),
        amount,
        0,
        "activity_reward",
        Some(tid),
        pid,
        None,
        system_reason("activity_reward", crate::i18n::current()).unwrap_or_default(),
        now,
    )
    .await?;
    Ok(())
}
pub(super) async fn refund(
    db: &Database,
    tx: &mut Tx<'_>,
    owner: i64,
    tid: i64,
    amount: i64,
    now: i64,
) -> Result<()> {
    if amount > 0 {
        accounts(db, tx, &[owner]).await?;
        apply(
            db,
            tx,
            owner,
            &format!("activity:{tid}:refund"),
            amount,
            -amount,
            "activity_refund",
            Some(tid),
            None,
            None,
            system_reason("activity_refund", crate::i18n::current()).unwrap_or_default(),
            now,
        )
        .await?;
    }
    Ok(())
}

/// State the sidebar check-in button needs: whether the member has checked in today, and whether the check-in rule is
/// enabled and its reward amount.
///
/// The logic matches the credits page, but only the single `signin` rule is fetched by primary key instead of the
/// whole rule table. Both read at most one row, each once per member-card render; once per-member state caching lands
/// the cache takes this over, so the queries themselves are not changed here.
pub struct SigninState {
    pub checked_in: bool,
    pub enabled: bool,
    pub amount: i64,
}

/// Check-in state for the member card. The rule (switch and amount) comes from the caller: the page takes all rule rows
/// from the in-process global cache (`cache::GlobalCache::credit_rules`); this only checks whether the member checked
/// in today. "Today" follows the site time zone (`offset_minutes` comes from `settings.time.offset_minutes`).
pub async fn signin_state(
    db: &Database,
    uid: i64,
    now: i64,
    offset_minutes: i64,
    rules: &[Rule],
) -> Result<SigninState> {
    let checked_in = signed_in_on(db.executor(), uid, now, offset_minutes).await?;
    let (enabled, amount) = rules
        .iter()
        .find(|rule| rule.key == "signin")
        .map_or((false, 0), |rule| (rule.enabled, rule.amount));
    Ok(SigninState {
        checked_in,
        enabled,
        amount,
    })
}

pub async fn rules(db: &Database) -> Result<Vec<Rule>> {
    let rows=crate::db::query("SELECT rule_key,enabled,amount,daily_limit,version FROM gonof_common_credit_rule ORDER BY rule_key").fetch_all(db.executor()).await?;
    rows.into_iter()
        .map(|r| {
            Ok(Rule {
                key: crate::db::text(&r, "rule_key")?,
                enabled: r.try_get::<i64, _>("enabled")? == 1,
                amount: r.try_get("amount")?,
                daily_limit: r.try_get("daily_limit")?,
                version: r.try_get("version")?,
            })
        })
        .collect()
}
/// Saves one reward rule. After commit, clears this process's rule block and broadcasts (the member card and the rules
/// page read the process cache; the award transaction still does a locking read of the rule row from the database).
#[allow(clippy::too_many_arguments)]
pub async fn update_rule(
    db: &Database,
    global: &crate::cache::GlobalCache,
    actor: Actor,
    key: &str,
    enabled: bool,
    amount: i64,
    daily_limit: i64,
    expected_version: i64,
    now: i64,
) -> Result<()> {
    retry_transaction(|| {
        update_rule_inner(
            db,
            actor,
            key,
            enabled,
            amount,
            daily_limit,
            expected_version,
            now,
        )
    })
    .await?;
    global.announce(&[crate::cache::Block::CreditRules]).await
}
#[allow(clippy::too_many_arguments)]
async fn update_rule_inner(
    db: &Database,
    actor: Actor,
    key: &str,
    enabled: bool,
    amount: i64,
    daily_limit: i64,
    expected_version: i64,
    now: i64,
) -> Result<()> {
    if !(["signin", "thread", "reply"].contains(&key)
        && (1..=MAX_CREDIT).contains(&amount)
        && (1..=MAX_CREDIT).contains(&daily_limit))
    {
        return Err(credit_failure(db, |lang, credit| {
            lang.activity_manage_error_rule_amounts(credit)
        })
        .await);
    }
    // An award requires today's total plus one full amount to stay within the cap; with a cap below the per-award
    // amount the rule shows "on" yet can never award even once.
    if daily_limit < amount {
        return Err(credit_failure(db, |lang, credit| {
            lang.activity_manage_error_rule_limit(credit)
        })
        .await);
    }
    let mut tx = db.begin_write().await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, true).await?;
    let changed=crate::db::query("UPDATE gonof_common_credit_rule SET enabled=?,amount=?,daily_limit=?,version=version+1 WHERE rule_key=? AND version=?").bind(i64::from(enabled)).bind(amount).bind(daily_limit).bind(key).bind(expected_version).execute(db.observe(&mut *tx)).await?;
    valid(
        changed.rows_affected() == 1,
        crate::i18n::current().activity_manage_error_rule_stale(),
    )?;
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'credits.rule',?,'web',?)").bind(actor.uid).bind(actor.uid).bind(serde_json::json!({"rule":key,"enabled":enabled,"amount":amount,"daily_limit":daily_limit}).to_string()).bind(now).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn adjust(
    db: &Database,
    actor: Actor,
    uid: i64,
    amount: i64,
    key: &str,
    reason: &str,
    now: i64,
) -> Result<()> {
    retry_transaction(|| adjust_inner(db, actor, uid, amount, key, ("admin_adjust", reason), now))
        .await
}

/// The initial points an activity plan grants (`gono-forum-cli run-campaign-plan`): an administrator's adjustment
/// with the system kind `seed_grant`, whose reason is the system text in the language of the moment. A plan run
/// again after a failure finds its earlier grants by event key and compares the kind and amounts only, so changing
/// the site language in between does not make them look like a different operation. The event key is the
/// adjustment's, so a grant is never booked twice.
pub async fn seed_grant(
    db: &Database,
    actor: Actor,
    uid: i64,
    amount: i64,
    key: &str,
    now: i64,
) -> Result<()> {
    retry_transaction(|| {
        let reason = system_reason("seed_grant", crate::i18n::current()).unwrap_or_default();
        adjust_inner(db, actor, uid, amount, key, ("seed_grant", reason), now)
    })
    .await
}

async fn adjust_inner(
    db: &Database,
    actor: Actor,
    uid: i64,
    amount: i64,
    key: &str,
    (kind, reason): (&str, &str),
    now: i64,
) -> Result<()> {
    valid(
        uid > 0
            && amount != 0
            && (-MAX_CREDIT..=MAX_CREDIT).contains(&amount)
            && crate::identity::valid_request_key(key)
            && !reason.trim().is_empty()
            && reason.chars().count() <= 500,
        crate::i18n::current().activity_manage_error_adjust_input(),
    )?;
    let mut tx = db.begin_write().await?;
    lock_members(db, &mut tx, &[actor.uid, uid]).await?;
    actor_locked(db, &mut tx, actor, true).await?;
    if !crate::privacy::active_member(db, &mut tx, uid).await? {
        return Err(credit_failure_in(db, &mut tx, |lang, credit| {
            lang.activity_manage_error_adjust_deleting(credit)
        })
        .await);
    }
    accounts(db, &mut tx, &[uid]).await?;
    apply(
        db,
        &mut tx,
        uid,
        &format!("adjust:{}:{key}", actor.uid),
        amount,
        0,
        kind,
        None,
        None,
        Some(actor.uid),
        reason,
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The daily cap accumulates per calendar day in the site time zone; the caller takes `offset_minutes` from the
/// settings this transaction's write gate read.
#[allow(clippy::too_many_arguments)]
async fn rule_reward(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    rule: &str,
    key: &str,
    pid: Option<i64>,
    now: i64,
    offset_minutes: i64,
) -> Result<bool> {
    // Ordinary content remains writable during cooling without creating rewards.
    if !crate::privacy::funding_allowed(db, tx, uid).await? {
        return Ok(false);
    }
    if crate::db::query("SELECT id FROM gonof_common_credit_ledger WHERE event_key=?")
        .bind(key)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let row=crate::db::query("SELECT enabled,amount,daily_limit FROM gonof_common_credit_rule WHERE rule_key=? LOCK IN SHARE MODE").bind(rule).fetch_one(db.observe(&mut **tx)).await?;
    if row.try_get::<i64, _>("enabled")? != 1 {
        return Ok(false);
    }
    let day_start = crate::clock::day_start(now, offset_minutes);
    let awarded = crate::db::query_scalar::<Option<i64>>("SELECT CAST(SUM(available_delta) AS SIGNED) FROM gonof_common_credit_ledger WHERE uid=? AND kind=? AND dateline>=? AND dateline<?").bind(uid).bind(rule).bind(day_start).bind(day_start+86400).fetch_one(db.observe(&mut **tx)).await?.unwrap_or(0);
    let amount: i64 = row.try_get("amount")?;
    let daily_limit: i64 = row.try_get("daily_limit")?;
    if awarded
        .checked_add(amount)
        .is_none_or(|sum| sum > daily_limit)
    {
        return Ok(false);
    }
    accounts(db, tx, &[uid]).await?;
    apply(
        db,
        tx,
        uid,
        key,
        amount,
        0,
        rule,
        None,
        pid,
        None,
        system_reason(rule, crate::i18n::current()).unwrap_or_default(),
        now,
    )
    .await
}
/// Rewards for posting threads and replies. `offset_minutes` is the site time zone read by this transaction's write
/// gate; the daily cap counts calendar days in it.
pub async fn reward_content(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    pid: i64,
    first: bool,
    now: i64,
    offset_minutes: i64,
) -> Result<()> {
    let rule = if first { "thread" } else { "reply" };
    rule_reward(
        db,
        tx,
        uid,
        rule,
        &format!("reward:{rule}:{pid}"),
        Some(pid),
        now,
        offset_minutes,
    )
    .await?;
    Ok(())
}
/// Check-in is idempotent per member and calendar day in the site time zone: checking in again the same day returns
/// `false` without a second award. The day is decided by the times of the member's check-ins in the current site time
/// zone, as the calendar and the credits page show them ([`signed_in_on`]); the ledger `event_key` is
/// `signin:{uid}:{day}`, and after a time zone change a check-in can hold the key of a day it no longer falls on, so
/// that day's check-in takes `signin:{uid}:{day}:{offset minutes}` instead. It takes no request key and does not
/// replay an earlier result by request key.
///
/// The website allows it during read-only mode; through the member API (`actor.api_key` set) read-only mode refuses it
/// inside the transaction (docs/api.md, "Account and site rules").
pub async fn check_in(db: &Database, actor: Actor, now: i64) -> Result<bool> {
    check_in_inner(db, actor, Some(now)).await
}
pub async fn check_in_live(db: &Database, actor: Actor) -> Result<bool> {
    retry_transaction(|| check_in_inner(db, actor, None)).await
}
async fn check_in_inner(db: &Database, actor: Actor, timestamp: Option<i64>) -> Result<bool> {
    // Check-in only touches the member's own credits and works as usual on the website in read-only mode, so it
    // goes through the read gate; only the member API, which documents every write as refused in read-only mode,
    // looks at the read-only switch here. The gate is this transaction's first lock: the site time zone, the daily cap
    // and the switch come from this same read, and a check-in waits for an admin-area settings change to commit.
    let mut tx = db.begin_write().await?;
    let settings = crate::runtime::locked(db, &mut tx).await?;
    if actor.api_key.is_some() && settings.site.read_only {
        return Err(crate::identity::IdentityError::Suspended(
            settings.site.read_only_notice(crate::i18n::current()),
        )
        .into());
    }
    let offset_minutes = settings.time.offset_minutes;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    funding(db, &mut tx, actor.uid).await?;
    let enabled: i64 = crate::db::query_scalar(
        "SELECT enabled FROM gonof_common_credit_rule WHERE rule_key='signin' LOCK IN SHARE MODE",
    )
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        enabled == 1,
        crate::i18n::current().activity_error_signin_off(),
    )?;
    // The member row is locked, so check-ins of one member run one after another and this read sees the last one.
    if signed_in_on(db.observe(&mut *tx), actor.uid, now, offset_minutes).await? {
        tx.commit().await?;
        return Ok(false);
    }
    let mut key = signin_key(actor.uid, now, offset_minutes);
    if crate::db::query("SELECT id FROM gonof_common_credit_ledger WHERE event_key=?")
        .bind(&key)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .is_some()
    {
        // Taken by a check-in made under another time zone that now falls on another day.
        key = format!("{key}:{offset_minutes}");
    }
    let credited = rule_reward(
        db,
        &mut tx,
        actor.uid,
        "signin",
        &key,
        None,
        now,
        offset_minutes,
    )
    .await?;
    tx.commit().await?;
    Ok(credited)
}

/// Whether the member checked in on the site day containing `now` in the current site time zone, judged by the times
/// of the check-ins like the calendar ([`super::signin_stats`]) rather than by the day in their keys, which keeps the
/// time zone the site had at check-in. Reads `gonof_credit_rule_daily (uid, kind, dateline)`.
async fn signed_in_on<'c, E>(executor: E, uid: i64, now: i64, offset_minutes: i64) -> Result<bool>
where
    E: crate::db::Executor<'c>,
{
    let start = crate::clock::day_start(now, offset_minutes);
    Ok(crate::db::query(
        "SELECT id FROM gonof_common_credit_ledger WHERE uid=? AND kind='signin' AND dateline>=? AND dateline<? LIMIT 1",
    )
    .bind(uid)
    .bind(start)
    .bind(start + 86_400)
    .fetch_optional(executor)
    .await?
    .is_some())
}
/// Credits page. `offset_minutes` is the site time zone; "You have checked in today" is judged by its calendar day.
/// The balance, the history page and the check-in status are read in one read-only snapshot, so a check-in or another
/// entry committed meanwhile shows in all of them or in none.
pub async fn credit_page(
    db: &Database,
    uid: i64,
    before: i64,
    now: i64,
    offset_minutes: i64,
) -> Result<CreditPage> {
    let mut tx = db.begin_read_only().await?;
    let page = credit_page_in(db, &mut tx, uid, before, now, offset_minutes).await?;
    tx.rollback().await?;
    Ok(page)
}

async fn credit_page_in(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    before: i64,
    now: i64,
    offset_minutes: i64,
) -> Result<CreditPage> {
    let account =
        crate::db::query("SELECT available,frozen FROM gonof_common_credit_account WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    let (available, frozen) = match account {
        Some(r) => (r.try_get("available")?, r.try_get("frozen")?),
        None => (0, 0),
    };
    // Pages in descending (dateline, id) order, which is exactly the order of the index
    // `gonof_credit_member_history(uid, dateline, id)`, so each page reads only 51 rows. Ordering by id descending,
    // which the index cannot provide, would make every page read and sort all of this member's ledger entries.
    // The paging parameter is still the id of the last entry on the previous page; its time is looked
    // up by primary key first, and an entry that is not the member's own counts as no next page.
    const COLUMNS: &str = "SELECT l.id,l.tid,a.kind AS activity_kind,l.available_delta,l.frozen_delta,l.available_after,l.frozen_after,l.kind,l.reason,l.dateline FROM gonof_common_credit_ledger l LEFT JOIN gonof_forum_activity a ON a.tid=l.tid";
    let rows = if before > 0 {
        let cursor: Option<i64> = crate::db::query_scalar(
            "SELECT dateline FROM gonof_common_credit_ledger WHERE id=? AND uid=?",
        )
        .bind(before)
        .bind(uid)
        .fetch_optional(db.observe(&mut **tx))
        .await?;
        match cursor {
            Some(dateline) => crate::db::query(&format!("{COLUMNS} WHERE l.uid=? AND (l.dateline<? OR (l.dateline=? AND l.id<?)) ORDER BY l.dateline DESC,l.id DESC LIMIT 51"))
                .bind(uid)
                .bind(dateline)
                .bind(dateline)
                .bind(before)
                .fetch_all(db.observe(&mut **tx))
                .await?,
            None => Vec::new(),
        }
    } else {
        crate::db::query(&format!(
            "{COLUMNS} WHERE l.uid=? ORDER BY l.dateline DESC,l.id DESC LIMIT 51"
        ))
        .bind(uid)
        .fetch_all(db.observe(&mut **tx))
        .await?
    };
    let more = rows.len() > 50;
    let entries: Vec<LedgerItem> = rows
        .into_iter()
        .take(50)
        .map(|r| {
            Ok(LedgerItem {
                tid: r.try_get("tid")?,
                activity_kind: r.try_get("activity_kind")?,
                id: r.try_get("id")?,
                available_delta: r.try_get("available_delta")?,
                frozen_delta: r.try_get("frozen_delta")?,
                available_after: r.try_get("available_after")?,
                frozen_after: r.try_get("frozen_after")?,
                kind: crate::db::text(&r, "kind")?,
                reason: crate::db::text(&r, "reason")?,
                dateline: r.try_get("dateline")?,
            })
        })
        .collect::<Result<_>>()?;
    let checked_in = signed_in_on(db.observe(&mut **tx), uid, now, offset_minutes).await?;
    Ok(CreditPage {
        available,
        frozen,
        next_before: if more {
            entries.last().map(|x| x.id).unwrap_or(0)
        } else {
            0
        },
        entries,
        checked_in,
    })
}

pub async fn reward_content_live(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    pid: i64,
    first: bool,
    offset_minutes: i64,
) -> Result<()> {
    let rule = if first { "thread" } else { "reply" };
    crate::db::query(
        "SELECT rule_key FROM gonof_common_credit_rule WHERE rule_key=? LOCK IN SHARE MODE",
    )
    .bind(rule)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    let now = locked_now();
    reward_content(db, tx, uid, pid, first, now, offset_minutes).await
}

/// Credit reconciliation result (report only, changes nothing).
///
/// There are no triggers; two things keep ledger entries immutable and undeletable: the source only ever INSERTs into
/// the ledger (pinned by `db::source_guards`), and this
/// reconciliation: if anyone bypasses the code and changes the ledger or an account, the equations stop holding.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CreditReconciliation {
    /// Uids whose account available and frozen amounts differ from the sum of all their ledger deltas (no account row
    /// counts as 0/0).
    pub account_sum: Vec<i64>,
    /// Uids whose account available and frozen amounts differ from `*_after` of their last ledger entry (only members
    /// with ledger entries).
    pub account_last: Vec<i64>,
    /// Uids whose frozen amount differs from the sum of unpaid budgets of their unsettled activities (`open`,
    /// `drawing`).
    pub frozen_activity: Vec<i64>,
    /// Uids of accounts with a negative balance.
    pub negative_account: Vec<i64>,
    /// Ids of ledger entries with a negative balance.
    pub negative_ledger: Vec<i64>,
}

impl CreditReconciliation {
    pub fn is_consistent(&self) -> bool {
        *self == Self::default()
    }
}

/// Credit reconciliation (report only, no automatic fixes): account = sum of ledger deltas = balance of the last ledger
/// entry, frozen amount = sum of unpaid budgets of unsettled activities, and no balance is negative. Reads all accounts
/// and ledger entries; used by admin maintenance, pre-deployment checks and tests, never called on a request path.
pub async fn reconcile_credits(db: &Database) -> Result<CreditReconciliation> {
    let mut result = CreditReconciliation::default();
    // uid → (sum of available deltas, sum of frozen deltas, available balance of the last entry, frozen balance of the
    // last entry)
    let mut ledger: std::collections::BTreeMap<i64, (i64, i64, i64, i64)> = Default::default();
    for row in crate::db::query(
        "SELECT id,uid,available_delta,frozen_delta,available_after,frozen_after FROM gonof_common_credit_ledger ORDER BY id",
    )
    .fetch_all(db.executor())
    .await?
    {
        let available_after: i64 = row.try_get("available_after")?;
        let frozen_after: i64 = row.try_get("frozen_after")?;
        if available_after < 0 || frozen_after < 0 {
            result.negative_ledger.push(row.try_get("id")?);
        }
        let entry = ledger.entry(row.try_get("uid")?).or_default();
        entry.0 += row.try_get::<i64, _>("available_delta")?;
        entry.1 += row.try_get::<i64, _>("frozen_delta")?;
        entry.2 = available_after;
        entry.3 = frozen_after;
    }
    let mut reserved: std::collections::BTreeMap<i64, i64> = Default::default();
    for row in crate::db::query("SELECT owner_uid,remaining,state FROM gonof_forum_activity")
        .fetch_all(db.executor())
        .await?
    {
        let state = invariants::State::parse(&crate::db::text(&row, "state")?);
        if matches!(
            state,
            Some(invariants::State::Open | invariants::State::Drawing)
        ) {
            *reserved.entry(row.try_get("owner_uid")?).or_default() +=
                row.try_get::<i64, _>("remaining")?;
        }
    }
    let mut accounts: std::collections::BTreeMap<i64, (i64, i64)> = Default::default();
    for row in crate::db::query("SELECT uid,available,frozen FROM gonof_common_credit_account")
        .fetch_all(db.executor())
        .await?
    {
        accounts.insert(
            row.try_get("uid")?,
            (row.try_get("available")?, row.try_get("frozen")?),
        );
    }
    let uids: BTreeSet<i64> = ledger
        .keys()
        .chain(accounts.keys())
        .chain(reserved.keys())
        .copied()
        .collect();
    for uid in uids {
        let (available, frozen) = accounts.get(&uid).copied().unwrap_or_default();
        if available < 0 || frozen < 0 {
            result.negative_account.push(uid);
        }
        match ledger.get(&uid) {
            Some(&(sum_available, sum_frozen, last_available, last_frozen)) => {
                if (available, frozen) != (sum_available, sum_frozen) {
                    result.account_sum.push(uid);
                }
                if (available, frozen) != (last_available, last_frozen) {
                    result.account_last.push(uid);
                }
            }
            None if (available, frozen) != (0, 0) => result.account_sum.push(uid),
            None => {}
        }
        if frozen != reserved.get(&uid).copied().unwrap_or(0) {
            result.frozen_activity.push(uid);
        }
    }
    Ok(result)
}
