//! Transactional topic activities and the credit ledger. Redis availability is
//! enforced by the HTTP/worker entry points; SQL remains the monetary authority.
mod campaign;
pub mod invariants;
mod ledger;
mod lottery;
mod signin;
mod tip;
mod views;
mod worker;

pub use campaign::{
    accept, accept_live, on_reply, on_reply_live, publish, publish_live, terminate, vote, vote_live,
};
pub use ledger::{
    CreditPage, CreditReconciliation, LedgerItem, Rule, SigninState, adjust, check_in,
    check_in_live, credit_page, reconcile_credits, reward_content, reward_content_live, rules,
    signin_state, system_reason, update_rule,
};
pub use lottery::{
    CODE_MAX_CHARS, CodeBoard, CodePrize, CodeRekeyReport, CodeSlot, ENTRY_PAGE,
    MAX_NON_CREDIT_WINNERS, MAX_PRIZES, MAX_REQUIREMENTS, PrizeItem, ReviewEntry, ReviewPage,
    code_board, correct_code, correct_code_live, draw_now, draw_now_live, enter, enter_live,
    exclude, exclude_live, my_prizes, parse_prize_lines, rekey_lottery_codes, replace_codes,
    replace_codes_live, restore, restore_live, review_page,
};
pub use signin::{CalendarDay, SigninStats, signin_stats, signin_stats_from_days};
pub(crate) use tip::out_key as tip_out_key;
pub use tip::{
    MAX_TIERS, RECENT_TIPPERS, TipSettings, TipSummary, Tipper, amounts_text, can_receive,
    parse_amounts, save_tip_settings, tip, tip_live, tip_summary, tipped,
};
pub use views::{ActivityCard, CardPrize, PollOption, Requirement, Winner, card, card_for_thread};
pub use worker::{DueBackoff, process_due, process_due_with};

use crate::db::Tx;
use crate::{db::Database, identity::Actor};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Every attempt owns its complete transaction. Only server-confirmed deadlocks
/// and lock timeouts may be replayed; uncertain connection/commit failures are
/// returned so the caller can later replay the original durable request key.
pub(crate) async fn retry_transaction<T, F, Fut>(mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    for attempt in 0..3 {
        match operation().await {
            Err(error) if attempt < 2 && retryable_lock_error(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(20 << attempt)).await;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt returns its result")
}

fn retryable_lock_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<sqlx::Error>()
            .is_some_and(crate::db::is_lock_conflict)
    })
}

/// The six thread types in category order, in this request's language (the words of
/// [`crate::content::category_label`]).
pub fn labels() -> [&'static str; 6] {
    std::array::from_fn(|category| crate::content::category_label(category as i64))
}
pub const MAX_CREDIT: i64 = 1_000_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActivitySpec {
    pub kind: i64,
    pub budget: i64,
    pub shares: i64,
    pub distribution: String,
    pub deadline: i64,
    pub max_choices: i64,
    pub options: Vec<String>,
    pub source_url: String,
    pub source_name: String,
    pub source_author: String,
    /// How members enter the lottery: `reply` enters automatically on a valid reply, `click` enters with one click.
    pub entry_mode: String,
    /// Entry requirements for one-click lotteries; each item must be confirmed when entering.
    pub requirements: Vec<String>,
    /// The author may remove entrants before the draw.
    pub review: bool,
    /// The author may draw early.
    pub early_draw: bool,
    /// Lottery prizes, allocated in numbering order. When empty, `budget` and `shares` describe a single credit prize.
    pub prizes: Vec<PrizeSpec>,
}
impl Default for ActivitySpec {
    fn default() -> Self {
        Self {
            kind: 0,
            budget: 0,
            shares: 0,
            distribution: "equal".into(),
            deadline: 0,
            max_choices: 1,
            options: vec![],
            source_url: String::new(),
            source_name: String::new(),
            source_author: String::new(),
            entry_mode: "reply".into(),
            requirements: vec![],
            review: false,
            early_draw: false,
            prizes: vec![],
        }
    }
}

/// One lottery prize. `kind`: `credit` credits (`amount` per winner), `code` redemption codes (`codes` has exactly
/// `winners` entries), `item` other prizes (not handed out by the platform; `note` describes how to claim it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrizeSpec {
    pub name: String,
    pub winners: i64,
    pub kind: String,
    pub amount: i64,
    pub note: String,
    pub codes: Vec<String>,
}

impl ActivitySpec {
    /// Normalizes by kind: keeps only the fields the kind really uses and sets the rest to defaults.
    /// The idempotency digest and the stored row both use the normalized values, so extra or missing unrelated fields
    /// from the client never affect the retry check.
    pub fn normalized(&self) -> Self {
        let mut spec = Self {
            kind: self.kind,
            ..Self::default()
        };
        if (1..=4).contains(&self.kind) {
            spec.deadline = self.deadline;
        }
        if (1..=3).contains(&self.kind) {
            spec.budget = self.budget;
        }
        if [2, 3].contains(&self.kind) {
            spec.shares = self.shares;
        }
        // Lotteries do not store a distribution mode; keeping the default equal split passes validation and matches
        // the fallback used when reading it back.
        if self.kind == 2 {
            spec.distribution = self.distribution.clone();
        }
        if self.kind == 4 {
            spec.max_choices = self.max_choices;
            spec.options = self.options.clone();
        }
        if self.kind == 5 {
            spec.source_name = self.source_name.clone();
            spec.source_url = self.source_url.clone();
            spec.source_author = self.source_author.clone();
        }
        if self.kind == 3 {
            spec.entry_mode = self.entry_mode.clone();
            // Entry requirements belong to one-click entry only; extras sent with reply entry are not stored.
            if self.entry_mode == "click" {
                spec.requirements = self.requirements.clone();
            }
            spec.review = self.review;
            spec.early_draw = self.early_draw;
            spec.prizes = self
                .prizes
                .iter()
                .map(|prize| {
                    let mut prize = prize.clone();
                    if prize.kind != "credit" {
                        prize.amount = 0;
                    }
                    if prize.kind != "code" {
                        prize.codes.clear();
                    }
                    prize
                })
                .collect();
            // When prizes are given, the budget and share count are computed from them; the client's budget and shares
            // do not enter the idempotency digest.
            if !spec.prizes.is_empty() {
                let (budget, shares) = prize_totals(&spec.prizes);
                spec.budget = budget;
                spec.shares = shares;
            }
        }
        spec
    }

    /// Lottery prizes. Without prizes, `budget` and `shares` describe one credit prize named after the site's points
    /// (`credit`, API shorthand).
    pub fn lottery_prizes(&self, credit: &str) -> Vec<PrizeSpec> {
        if !self.prizes.is_empty() || self.shares <= 0 {
            return self.prizes.clone();
        }
        vec![PrizeSpec {
            name: credit.into(),
            winners: self.shares,
            kind: "credit".into(),
            amount: self.budget / self.shares,
            note: String::new(),
            codes: vec![],
        }]
    }

    /// `credit` is the site's name for its points in this request's language, for the messages that name it.
    pub fn validate(&self, now: i64, credit: &str) -> Result<()> {
        valid(
            (0..=5).contains(&self.kind),
            crate::i18n::current().activity_error_category(),
        )?;
        if (1..=4).contains(&self.kind) {
            valid(
                self.deadline > now,
                crate::i18n::current().activity_error_deadline(),
            )?;
        }
        if (1..=2).contains(&self.kind) || (self.kind == 3 && self.prizes.is_empty()) {
            valid(
                (1..=MAX_CREDIT).contains(&self.budget),
                &crate::i18n::current().activity_error_budget(credit),
            )?;
        }
        if self.kind == 2 || (self.kind == 3 && self.prizes.is_empty()) {
            valid(
                (1..=10_000).contains(&self.shares) && self.shares <= self.budget,
                crate::i18n::current().activity_error_shares(),
            )?;
            let distribution = invariants::Distribution::parse(&self.distribution);
            valid(
                distribution.is_some(),
                crate::i18n::current().activity_error_distribution(),
            )?;
            if self.kind == 3 || distribution == Some(invariants::Distribution::Equal) {
                valid(
                    self.budget % self.shares == 0,
                    crate::i18n::current().activity_error_divisible(),
                )?;
            }
        }
        if self.kind == 3 {
            self.validate_lottery(credit)?;
        }
        if self.kind == 4 {
            valid(
                (2..=20).contains(&self.options.len()),
                crate::i18n::current().activity_error_poll_options(),
            )?;
            valid(
                self.max_choices >= 1 && self.max_choices <= self.options.len() as i64,
                crate::i18n::current().activity_error_poll_max(),
            )?;
            let mut unique = BTreeSet::new();
            for option in &self.options {
                valid(
                    !option.trim().is_empty()
                        && option.chars().count() <= 255
                        && !option.contains('\0')
                        && unique.insert(option.trim()),
                    crate::i18n::current().activity_error_poll_option(),
                )?;
            }
        }
        if self.kind == 5 {
            let url = url::Url::parse(&self.source_url)
                .map_err(|_| problem(crate::i18n::current().activity_error_news_url()))?;
            valid(
                ["http", "https"].contains(&url.scheme())
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && self.source_url.len() <= 2048,
                crate::i18n::current().activity_error_news_link(),
            )?;
            valid(
                !self.source_name.trim().is_empty()
                    && self.source_name.chars().count() <= 255
                    && !self.source_name.contains('\0')
                    && !self.source_author.trim().is_empty()
                    && self.source_author.chars().count() <= 255
                    && !self.source_author.contains('\0'),
                crate::i18n::current().activity_error_news_fields(),
            )?;
        }
        Ok(())
    }

    /// Lottery entry mode, entry requirements and prizes.
    fn validate_lottery(&self, credit: &str) -> Result<()> {
        valid(
            ["reply", "click"].contains(&self.entry_mode.as_str()),
            crate::i18n::current().activity_error_entry_mode(),
        )?;
        valid(
            self.entry_mode == "click" || self.requirements.is_empty(),
            crate::i18n::current().activity_error_requirements_mode(),
        )?;
        valid(
            self.requirements.len() <= lottery::MAX_REQUIREMENTS,
            crate::i18n::current().activity_error_requirements_count(),
        )?;
        for item in &self.requirements {
            valid(
                text_fits(item, 200),
                crate::i18n::current().activity_error_requirement(),
            )?;
        }
        let prizes = self.lottery_prizes(credit);
        valid(
            (1..=lottery::MAX_PRIZES).contains(&prizes.len()),
            crate::i18n::current().activity_error_prizes_count(),
        )?;
        let mut codes = BTreeSet::new();
        let (mut total, mut non_credit, mut budget) = (0_i64, 0_i64, 0_i64);
        for prize in &prizes {
            valid(
                text_fits(&prize.name, 60),
                crate::i18n::current().activity_error_prize_name(),
            )?;
            valid(
                prize.note.chars().count() <= 200 && !prize.note.contains('\0'),
                crate::i18n::current().activity_error_prize_note(),
            )?;
            valid(
                (1..=10_000).contains(&prize.winners),
                crate::i18n::current().activity_error_prize_winners(),
            )?;
            total += prize.winners;
            match prize.kind.as_str() {
                "credit" => {
                    valid(
                        (1..=MAX_CREDIT).contains(&prize.amount) && prize.codes.is_empty(),
                        &crate::i18n::current().activity_error_prize_amount(credit),
                    )?;
                    budget = prize
                        .amount
                        .checked_mul(prize.winners)
                        .and_then(|cost| budget.checked_add(cost))
                        .filter(|budget| *budget <= MAX_CREDIT)
                        .ok_or_else(|| {
                            problem(&crate::i18n::current().activity_error_prize_total(credit))
                        })?;
                }
                "code" => {
                    non_credit += prize.winners;
                    valid(
                        prize.amount == 0 && prize.codes.len() as i64 == prize.winners,
                        crate::i18n::current().activity_error_prize_codes(),
                    )?;
                    for code in &prize.codes {
                        valid(
                            lottery::valid_code(code),
                            crate::i18n::current().activity_error_code_format(),
                        )?;
                        valid(
                            codes.insert(code.trim()),
                            crate::i18n::current().activity_error_code_duplicate(),
                        )?;
                    }
                }
                "item" => {
                    non_credit += prize.winners;
                    valid(
                        prize.amount == 0 && prize.codes.is_empty(),
                        &crate::i18n::current().activity_error_prize_item(credit),
                    )?;
                }
                _ => return Err(problem(crate::i18n::current().activity_error_prize_kind())),
            }
        }
        valid(
            total <= 10_000,
            crate::i18n::current().activity_error_winners_total(),
        )?;
        valid(
            non_credit <= lottery::MAX_NON_CREDIT_WINNERS,
            crate::i18n::current().activity_error_non_credit(),
        )?;
        Ok(())
    }
}

/// Budget of the lottery prizes (sum of credit prize amount × winners) and total winners; saturates on overflow and
/// leaves the rejection to validation.
pub(crate) fn prize_totals(prizes: &[PrizeSpec]) -> (i64, i64) {
    prizes
        .iter()
        .fold((0_i64, 0_i64), |(budget, shares), prize| {
            let cost = if prize.kind == "credit" {
                prize.amount.saturating_mul(prize.winners)
            } else {
                0
            };
            (
                budget.saturating_add(cost),
                shares.saturating_add(prize.winners),
            )
        })
}

/// After trimming leading and trailing whitespace, 1 to `max` characters, with no NUL.
fn text_fits(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.chars().count() <= max && !value.contains('\0')
}

#[derive(Debug)]
pub struct ActivityFailure(pub String);
impl std::fmt::Display for ActivityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ActivityFailure {}
fn problem(message: &str) -> anyhow::Error {
    anyhow!(ActivityFailure(message.into()))
}
pub(crate) fn valid(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(problem(message))
    }
}

/// Call before any member/content locks. All participating member rows are locked
/// in ascending UID order; this also serializes freeze and credential changes.
pub async fn lock_members(db: &Database, tx: &mut Tx<'_>, uids: &[i64]) -> Result<()> {
    for uid in uids.iter().copied().collect::<BTreeSet<_>>() {
        let found = crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
        valid(
            found.is_some(),
            crate::i18n::current().activity_error_member(),
        )?;
    }
    Ok(())
}
pub async fn discover_owner(db: &Database, tid: i64) -> Result<Option<i64>> {
    Ok(
        crate::db::query_scalar("SELECT authorid FROM gonof_forum_thread WHERE tid=?")
            .bind(tid)
            .fetch_optional(db.executor())
            .await?
            .flatten(),
    )
}
/// Locks and re-reads the actor's member row. Returns the mute expiry (0 for none): joining activities and tipping
/// are blocked by the caller with [`reject_muted`]; the starter's own actions (drawing, reviewing entries,
/// redemption codes, accepting an answer) and check-in proceed as usual.
pub(crate) async fn actor_locked(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    admin: bool,
) -> Result<i64> {
    let row=crate::db::query("SELECT credentials_version,`freeze`,mute_until,emailstatus,mobile,adminid,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE").bind(actor.uid).fetch_optional(db.observe(&mut **tx)).await?.ok_or_else(||problem(crate::i18n::current().activity_error_sign_in()))?;
    valid(
        row.try_get::<i64, _>("credentials_version")? == actor.credentials_version
            && row.try_get::<i64, _>("freeze")? == 0
            && crate::db::text(&row, "account_state")? == "active",
        crate::i18n::current().activity_error_session(),
    )?;
    valid(
        crate::identity::api_key_locked(db, tx, actor).await?,
        crate::i18n::current().activity_error_api_key(),
    )?;
    let role = row.try_get::<i64, _>("adminid")?;
    let participation = crate::login::Participation::current();
    if admin {
        if role != 1 {
            return Err(ledger::credit_failure_in(db, tx, |lang, credit| {
                lang.activity_manage_error_rules_admin(credit)
            })
            .await);
        }
    } else {
        valid(
            role == 1
                || crate::login::permits(
                    db.observe(&mut **tx),
                    actor.uid,
                    row.try_get::<i64, _>("emailstatus")? == 1,
                    row.try_get::<Option<String>, _>("mobile")?.is_some(),
                )
                .await?,
            participation.message(crate::i18n::current().common_need_email()),
        )?;
    }
    Ok(row.try_get("mute_until")?)
}

/// Muted members cannot tip, vote or enter lotteries (docs/roles.md, "Mutes").
pub(crate) fn reject_muted(mute_until: i64) -> Result<()> {
    valid(
        !crate::moderation::muted(mute_until, crate::identity::now()),
        &crate::moderation::muted_message(mute_until),
    )
}

/// Root user actions acquire this before account locks. Settlement workers and
/// management writes remain available while public posting is paused.
/// Write gate: the first lock of this transaction. Returns the whole range of scalar settings the gate read; tipping
/// uses its `tip` directly.
async fn guard_write(db: &Database, tx: &mut Tx<'_>) -> Result<crate::runtime::SiteSettings> {
    let settings = crate::runtime::locked(db, tx).await?;
    valid(
        !settings.site.read_only,
        &settings.site.read_only_notice(crate::i18n::current()),
    )?;
    Ok(settings)
}
async fn funding(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    valid(
        crate::privacy::funding_allowed(db, tx, uid).await?,
        crate::i18n::current().activity_error_funding(),
    )
}

/// Member locks precede this. The thread lock serializes activity state with
/// publication/governance. No pre-lock discovery read grants access.
async fn lock_thread(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<crate::db::DbRow> {
    let fid: i64 = crate::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or_else(|| problem(crate::i18n::current().topic_error_not_found()))?;
    // Takes exclusive locks on the forum and its parent forum in ascending FID order: the thread row carries no
    // inherited forum copies, so the re-check under the lock reads the forum table itself.
    let parent: Option<i64> =
        crate::db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
            .bind(fid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    for id in std::collections::BTreeSet::from_iter(std::iter::once(fid).chain(parent)) {
        crate::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
            .bind(id)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    }
    let row=crate::db::query(&format!("SELECT t.authorid,t.deleted_at,t.displayorder,t.visibility,t.publication_state,t.readperm,{},p.invisible AS first_invisible,p.deleted_at AS first_deleted FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid {} WHERE t.tid=? FOR UPDATE", crate::content::copies::FORUM_SCOPE_COLUMNS, crate::content::copies::FORUM_SCOPE_JOINS)).bind(tid).fetch_optional(db.observe(&mut **tx)).await?.ok_or_else(||problem(crate::i18n::current().topic_error_not_found()))?;
    Ok(row)
}
/// Whether a member can read this activity thread: public or members-only, not hidden, published. The forum status and
/// visibility are taken from the forum table as the inherited scope (the stricter of this level and the parent
/// level); the row must include `content::copies::FORUM_SCOPE_COLUMNS`. Withdrawal is checked by the caller.
pub(crate) fn member_visible(row: &crate::db::DbRow) -> Result<bool> {
    let scope = crate::content::copies::forum_scope(row)?;
    Ok(scope.enabled
        && scope.visibility <= crate::content::copies::ForumVisibility::Members
        && row.try_get::<i64, _>("displayorder")? >= 0
        && matches!(
            crate::db::text(row, "visibility")?.as_str(),
            "public" | "members"
        )
        && crate::db::text(row, "publication_state")? == "published"
        && row.try_get::<i64, _>("readperm")? == 0
        && row.try_get::<i64, _>("first_invisible")? == 0)
}
/// Participation and settlement condition shared by the five activity kinds: readable by the member and the thread
/// not withdrawn. Bounty acceptance, voting and drawing check it here; claiming red packets and entering lotteries
/// go through the reply transaction, where the reply itself is locked and re-checked under the same readability rule.
fn readable_thread(row: &crate::db::DbRow) -> Result<bool> {
    Ok(member_visible(row)?
        && row.try_get::<i64, _>("deleted_at")? == 0
        && row.try_get::<i64, _>("first_deleted")? == 0)
}
/// Check-in idempotency key: the member and the calendar day in the site time zone (`crate::clock::day_number`).
fn signin_key(uid: i64, now: i64, offset_minutes: i64) -> String {
    format!(
        "signin:{uid}:{}",
        crate::clock::day_number(now, offset_minutes)
    )
}

/// "Now" after locking: uses the program clock, not the database time, so the same SQL runs on both backends. Taken
/// after acquiring the activity row lock, so time spent waiting for the lock never makes the deadline check use a
/// stale moment.
fn locked_now() -> i64 {
    crate::identity::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lock conflict on SQLite is a real write-lock conflict: another process on the same machine (here another
    /// connection) holds the write lock for more than 5 seconds, so the first attempt waits out `busy_timeout` and
    /// reports BUSY; after it is released, the retry gets the lock and the write succeeds. Counterpart of the MySQL
    /// test that fakes deadlocks with SIGNAL,
    /// [`mysql_retries_only_confirmed_lock_errors_with_a_fixed_attempt_bound`].
    #[tokio::test]
    async fn sqlite_busy_write_locks_are_retried_until_another_process_lets_go() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "gonof_sqlite_retry_{}_{nanos}.db",
            std::process::id()
        ));
        let config = crate::config::DatabaseConfig {
            url: format!("sqlite://{}", path.display()),
            max_connections: 2,
            acquire_timeout_seconds: 30,
        };
        let db = Database::connect_creating(&config).await.unwrap();
        db.migrate().await.unwrap();
        let options = (*db.pool.as_sqlite().unwrap().connect_options()).clone();
        let mut other: sqlx::SqliteConnection =
            sqlx::Connection::connect_with(&options).await.unwrap();
        sqlx::Executor::execute(&mut other, "BEGIN IMMEDIATE")
            .await
            .unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5_500)).await;
            sqlx::Executor::execute(&mut other, "COMMIT").await.unwrap();
            sqlx::Connection::close(other).await.unwrap();
        });
        let mut attempts = 0;
        let started = std::time::Instant::now();
        retry_transaction(|| {
            attempts += 1;
            let db = db.clone();
            async move {
                let mut tx = db.begin_write().await?;
                crate::db::query(
                    "INSERT INTO gonof_common_settings(skey,svalue) VALUES('retry.busy','')",
                )
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(())
            }
        })
        .await
        .unwrap();
        release.await.unwrap();
        assert_eq!(
            attempts, 2,
            "the first attempt waits out 5 seconds and reports BUSY, the second writes"
        );
        assert!(started.elapsed() >= std::time::Duration::from_secs(5));
        db.pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
    /// A failure whose message names the points reads the name through its own transaction. With a pool of one
    /// connection, an adjustment past the balance reports the shortfall at once, instead of waiting for a second
    /// connection until the pool gives up.
    #[tokio::test]
    async fn credit_failures_need_no_second_connection() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let mut config = services.config.database.clone();
        config.max_connections = 1;
        config.acquire_timeout_seconds = 3;
        let db = Database::connect(&config).await.unwrap();
        for (uid, adminid) in [(9821, 1), (9822, 0)] {
            crate::db::query("INSERT INTO gonof_common_member(uid,username,password,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1)")
                .bind(uid)
                .bind(format!("ledger-{uid}"))
                .bind(adminid)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let admin = Actor {
            uid: 9821,
            credentials_version: crate::db::query_scalar(
                "SELECT credentials_version FROM gonof_common_member WHERE uid=9821",
            )
            .fetch_one(db.executor())
            .await
            .unwrap(),
            api_key: None,
        };
        let started = std::time::Instant::now();
        let error = adjust(
            &db,
            admin,
            9822,
            -5,
            &crate::identity::random_token(),
            "Correction",
            crate::identity::now(),
        )
        .await
        .unwrap_err();
        let credit = crate::runtime::read(&db)
            .await
            .unwrap()
            .site
            .credit_word(crate::i18n::Lang::EN);
        assert_eq!(
            error.to_string(),
            crate::i18n::Lang::EN.activity_error_insufficient(&credit)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        db.pool.close().await;
        services.cleanup().await.unwrap();
    }
    #[test]
    fn normalizing_keeps_only_the_fields_the_category_uses() {
        let every_field = ActivitySpec {
            kind: 4,
            budget: 50,
            shares: 5,
            distribution: "random".into(),
            deadline: 900,
            max_choices: 2,
            options: vec!["Red".into(), "Blue".into()],
            source_url: "https://example.test/a".into(),
            source_name: "Source".into(),
            source_author: "Author".into(),
            entry_mode: "click".into(),
            requirements: vec!["Follow the author".into()],
            review: true,
            early_draw: true,
            prizes: vec![PrizeSpec {
                name: "First prize".into(),
                winners: 1,
                kind: "code".into(),
                amount: 9,
                note: String::new(),
                codes: vec!["CODE-1".into()],
            }],
        };
        let poll = every_field.normalized();
        assert_eq!(poll.entry_mode, "reply");
        assert!(poll.prizes.is_empty() && poll.requirements.is_empty() && !poll.review);
        assert_eq!(
            (poll.deadline, poll.max_choices, poll.options.len()),
            (900, 2, 2)
        );
        assert_eq!((poll.budget, poll.shares), (0, 0));
        assert_eq!(poll.distribution, "equal");
        assert_eq!(poll.source_name, "");
        // Omitted unrelated fields must normalize to the same value, so the retry digest does not depend on whether the
        // client disables fields.
        let lean = ActivitySpec {
            kind: 4,
            deadline: 900,
            max_choices: 2,
            options: every_field.options.clone(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&poll).unwrap(),
            serde_json::to_string(&lean.normalized()).unwrap()
        );
        // Lotteries do not store a distribution mode; after normalizing back to equal split they must still validate.
        let lottery = ActivitySpec {
            kind: 3,
            prizes: vec![],
            ..every_field.clone()
        }
        .normalized();
        assert_eq!(lottery.distribution, "equal");
        assert_eq!((lottery.budget, lottery.shares), (50, 5));
        assert!(lottery.validate(100, "Coins").is_ok());
        // Prizes given: the budget and share count come from the prizes, not from the client; code prizes have their
        // amount reset to zero.
        let prized = ActivitySpec {
            kind: 3,
            ..every_field.clone()
        }
        .normalized();
        assert_eq!((prized.budget, prized.shares), (0, 1));
        assert_eq!(prized.prizes[0].amount, 0);
        assert_eq!(prized.requirements, vec!["Follow the author".to_owned()]);
        assert!(prized.validate(100, "Coins").is_ok());
        // Reply entry carries no entry requirements.
        let replied = ActivitySpec {
            kind: 3,
            entry_mode: "reply".into(),
            ..every_field.clone()
        }
        .normalized();
        assert!(replied.requirements.is_empty());
        // A normal thread carries no activity fields.
        let plain = ActivitySpec {
            kind: 0,
            ..every_field
        }
        .normalized();
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            serde_json::to_string(&ActivitySpec::default()).unwrap()
        );
    }
    #[test]
    fn deadlines_and_budget_validation() {
        let mut s = ActivitySpec {
            kind: 2,
            budget: 10,
            shares: 3,
            deadline: 101,
            ..Default::default()
        };
        assert!(s.validate(100, "Coins").is_err());
        s.distribution = "random".into();
        assert!(s.validate(100, "Coins").is_ok());
        assert!(s.validate(101, "Coins").is_err());
        s.kind = 3;
        assert!(s.validate(100, "Coins").is_err());
        s.budget = 12;
        assert!(s.validate(100, "Coins").is_ok());
    }
    #[test]
    fn votes_validate_once_created_options() {
        let mut s = ActivitySpec {
            kind: 4,
            deadline: 101,
            options: vec!["A".into(), "A".into()],
            ..Default::default()
        };
        assert!(s.validate(100, "Coins").is_err());
        s.options[1] = "B".into();
        assert!(s.validate(100, "Coins").is_ok());
        s.max_choices = 3;
        assert!(s.validate(100, "Coins").is_err());
    }
    fn prize(name: &str, winners: i64, kind: &str, amount: i64, codes: &[&str]) -> PrizeSpec {
        PrizeSpec {
            name: name.into(),
            winners,
            kind: kind.into(),
            amount,
            note: String::new(),
            codes: codes.iter().map(|c| (*c).to_owned()).collect(),
        }
    }
    #[test]
    fn lottery_prizes_validate_counts_codes_and_budget() {
        let base = ActivitySpec {
            kind: 3,
            deadline: 101,
            prizes: vec![
                prize("First prize", 1, "code", 0, &["A-1"]),
                prize("Second prize", 2, "credit", 30, &[]),
                prize("Souvenir", 1, "item", 0, &[]),
            ],
            ..Default::default()
        };
        assert!(base.validate(100, "Coins").is_ok());
        assert_eq!(prize_totals(&base.prizes), (60, 4));
        let check = |prizes: Vec<PrizeSpec>| {
            ActivitySpec {
                prizes,
                ..base.clone()
            }
            .validate(100, "Coins")
            .unwrap_err()
            .to_string()
        };
        assert!(
            check(vec![prize("First prize", 2, "code", 0, &["A"])])
                .contains("A code prize needs exactly one code per winner")
        );
        assert!(
            check(vec![prize("First prize", 2, "code", 0, &["A", "A"])])
                .contains("Codes cannot repeat within one lottery")
        );
        assert!(
            check(vec![
                prize("First prize", 1, "code", 0, &["A"]),
                prize("Second prize", 1, "code", 0, &[" A "])
            ])
            .contains("Codes cannot repeat within one lottery")
        );
        assert!(
            check(vec![prize("First prize", 1, "code", 0, &["quote\""])])
                .contains("A code must be 1 to 255 letters, digits or symbols")
        );
        assert!(
            check(vec![prize("", 1, "credit", 1, &[])])
                .contains("Prize names must be 1 to 60 characters")
        );
        assert!(
            check(vec![prize("Prize", 0, "credit", 1, &[])])
                .contains("Each prize needs 1 to 10,000 winners")
        );
        assert!(
            check(vec![prize("Prize", 1, "credit", 0, &[])])
                .contains("A Coins prize must give 1 to 1,000,000,000 Coins per winner")
        );
        assert!(check(vec![prize("Prize", 1, "gold", 1, &[])]).contains("Invalid prize type"));
        assert!(
            check(vec![prize("Prize", 201, "item", 0, &[])])
                .contains("cannot have more than 200 winners in total")
        );
        assert!(
            check(vec![
                prize("Prize", 10_000, "credit", 1, &[]),
                prize("Prize 2", 1, "item", 0, &[])
            ])
            .contains("There cannot be more than 10,000 winners in total")
        );
        assert!(
            check(vec![prize("Prize", 2, "credit", MAX_CREDIT, &[])])
                .contains("Coins prizes cannot add up to more than 1,000,000,000 Coins")
        );
        assert!(
            check(vec![prize("Prize", 1, "credit", 1, &[]); 11])
                .contains("A lottery needs 1 to 10 prizes")
        );
        // Entry requirements belong to one-click entry only, at most 10 items.
        let mut spec = ActivitySpec {
            requirements: vec!["Follow".into()],
            ..base.clone()
        };
        assert!(
            spec.validate(100, "Coins")
                .unwrap_err()
                .to_string()
                .contains("Only one-click lotteries can have entry requirements")
        );
        spec.entry_mode = "click".into();
        assert!(spec.validate(100, "Coins").is_ok());
        spec.requirements = vec!["Requirement".into(); 11];
        assert!(spec.validate(100, "Coins").is_err());
        spec.requirements = vec!["  ".into()];
        assert!(spec.validate(100, "Coins").is_err());
        spec.requirements.clear();
        spec.entry_mode = "auto".into();
        assert!(spec.validate(100, "Coins").is_err());
        // A lottery made only of redemption codes has a budget of 0, which is also valid.
        let codes_only = ActivitySpec {
            prizes: vec![prize("Code", 1, "code", 0, &["X"])],
            ..base.clone()
        };
        assert!(codes_only.validate(100, "Coins").is_ok());
        assert_eq!(prize_totals(&codes_only.prizes).0, 0);
    }
    #[test]
    fn checkin_key_follows_the_site_day() {
        // 1970-01-02 00:00 in UTC+8 is 1970-01-01 16:00 UTC (57600).
        assert_eq!(signin_key(7, 57_599, 480), "signin:7:0");
        assert_eq!(signin_key(7, 57_600, 480), "signin:7:1");
        // A UTC site only crosses the day at 86400.
        assert_eq!(signin_key(7, 57_600, 0), "signin:7:0");
        assert_eq!(signin_key(7, 86_400, 0), "signin:7:1");
    }
    #[test]
    fn news_rejects_active_and_credential_urls() {
        let mut s = ActivitySpec {
            kind: 5,
            source_url: "javascript:alert(1)".into(),
            source_name: "Source".into(),
            source_author: "Author".into(),
            ..Default::default()
        };
        assert!(s.validate(0, "Coins").is_err());
        s.source_url = "https://example.com/story".into();
        assert!(s.validate(0, "Coins").is_ok());
    }

    #[tokio::test]
    async fn mysql_retries_only_confirmed_lock_errors_with_a_fixed_attempt_bound() {
        // Only checks the MySQL deadlock and lock-wait-timeout error numbers faked with SIGNAL.
        crate::mysql_only!();
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = crate::test_support::TestFixture::new().await.unwrap();
        let db = &fixture.db;
        for code in [1213, 1205] {
            let attempts = AtomicUsize::new(0);
            let result = retry_transaction(|| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        crate::db::raw_sql(&format!("SIGNAL SQLSTATE '40001' SET MYSQL_ERRNO={code}, MESSAGE_TEXT='retry contract'"))
                            .execute(&db.pool).await.map_err(|error| anyhow::Error::new(error).context("wrapped transaction failure"))?;
                    }
                    Ok(7)
                }
            }).await.unwrap();
            assert_eq!(result, 7);
            assert_eq!(attempts.load(Ordering::SeqCst), 3);
        }
        let attempts = AtomicUsize::new(0);
        let failed: Result<()> = retry_transaction(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                crate::db::raw_sql("SIGNAL SQLSTATE '40001' SET MYSQL_ERRNO=1213, MESSAGE_TEXT='persistent deadlock'")
                    .execute(&db.pool).await?;
                Ok(())
            }
        }).await;
        assert!(failed.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let attempts = AtomicUsize::new(0);
        let failed: Result<()> = retry_transaction(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                crate::db::query(
                    "SIGNAL SQLSTATE '45000' SET MYSQL_ERRNO=1644, MESSAGE_TEXT='not retryable'",
                )
                .execute(&db.pool)
                .await?;
                Ok(())
            }
        })
        .await;
        assert!(failed.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        fixture.cleanup().await.unwrap();
    }
}
