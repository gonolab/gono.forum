//! Lottery entry methods, author review, early draw, multiple prizes and redemption codes.
//!
//! Lock order is the same as for other activities: settings rows → member rows (uid ascending) → forum → thread →
//! first post → activity row → `gonof_forum_lottery` row → this module's new tables. Entering, removing entrants and
//! replacing redemption codes all require the activity to still be `open`; once step 0 of the draw (at the deadline or
//! an early draw) changes it to `drawing`, all of them are refused, so the entry list the draw reads cannot change again.
use super::*;
use crate::db::Tx;
use crate::identity::TwoFactorConfig;

/// Maximum number of prizes per lottery.
pub const MAX_PRIZES: usize = 10;
/// Maximum number of participation requirements for one-click entry.
pub const MAX_REQUIREMENTS: usize = 10;
/// Upper limit on the combined slots of redemption-code and other prizes: a row is created for each of these slots at
/// publication.
pub const MAX_NON_CREDIT_WINNERS: i64 = 200;
/// Maximum length of a redemption code, in characters.
pub const CODE_MAX_CHARS: usize = 255;
/// Entrants per page on the review page.
pub const ENTRY_PAGE: i64 = 50;
/// Items per page in "My prizes".
const PRIZE_PAGE: i64 = 20;

/// Redemption codes accept only visible ASCII plus inner spaces, no quotes or backslashes: before encryption they are
/// encoded as JSON strings, where those two characters would need escaping; excluding them bounds the ciphertext length
/// (under the 512 characters of `gonof_forum_lottery_award.code_ciphertext`).
pub(crate) fn valid_code(code: &str) -> bool {
    let code = code.trim();
    !code.is_empty()
        && code.len() <= CODE_MAX_CHARS
        && code
            .bytes()
            .all(|b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\')
}

/// Prize syntax in the web form: one `name | slots | type | credits each | note` per line; the type is written as the
/// site's name for its points (`credit`), or this language's words for a code or another prize (`editor-prize-kind-*`),
/// or `credit`, `code`, `item`; "credits each" is left empty for non-credit prizes, and the note may be omitted. Redemption codes go in a separate field, one per line, in the order of the redemption-code prizes:
/// first all codes of the first redemption-code prize, then the next. Slots, counts and so on are validated by
/// `ActivitySpec::validate`.
pub fn parse_prize_lines(
    prizes: &str,
    codes: &str,
    credit: &str,
) -> std::result::Result<Vec<PrizeSpec>, String> {
    let lang = crate::i18n::current();
    let mut codes = codes
        .lines()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    let mut list = Vec::new();
    for (index, line) in prizes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
    {
        let row = index + 1;
        let fields: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        if fields.len() < 3 {
            return Err(lang.activity_error_prize_line(row, credit));
        }
        let winners: i64 = fields[1]
            .parse()
            .map_err(|_| lang.activity_error_prize_line_winners(row))?;
        let kind = match fields[2] {
            "credit" => "credit",
            "code" => "code",
            "item" => "item",
            word if word == credit || word == lang.brand_credit_name() => "credit",
            word if word == lang.editor_prize_kind_code() => "code",
            word if word == lang.editor_prize_kind_item() => "item",
            _ => {
                return Err(lang.activity_error_prize_line_kind(
                    row,
                    credit,
                    lang.editor_prize_kind_code(),
                    lang.editor_prize_kind_item(),
                ));
            }
        };
        let amount = match fields.get(3).copied().unwrap_or("") {
            "" => 0,
            value => value
                .parse()
                .map_err(|_| lang.activity_error_prize_line_amount(credit, row))?,
        };
        let taken = if kind == "code" {
            codes
                .by_ref()
                .take(winners.clamp(0, 10_000) as usize)
                .collect()
        } else {
            vec![]
        };
        list.push(PrizeSpec {
            name: fields[0].to_owned(),
            winners,
            kind: kind.into(),
            amount,
            note: fields.get(4).copied().unwrap_or("").to_owned(),
            codes: taken,
        });
    }
    if codes.next().is_some() {
        return Err(lang.activity_error_prize_line_codes().into());
    }
    Ok(list)
}

/// Fields read when the lottery row is locked together with the activity row.
struct Locked {
    owner: i64,
    kind: i64,
    state: String,
    deadline: i64,
    entry_mode: String,
    requirements: Vec<String>,
    review: bool,
    early_draw: bool,
}

/// The caller has already locked members and content in order. Locks the activity row and the lottery row and reads the
/// entry and draw settings.
async fn lottery_locked(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<Locked> {
    let row = crate::db::query("SELECT a.owner_uid,a.kind,a.state,a.deadline,l.entry_mode,l.requirements,l.review,l.early_draw FROM gonof_forum_activity a JOIN gonof_forum_lottery l ON l.tid=a.tid WHERE a.tid=? FOR UPDATE")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or_else(|| problem(crate::i18n::current().activity_error_not_lottery()))?;
    Ok(Locked {
        owner: row.try_get("owner_uid")?,
        kind: row.try_get("kind")?,
        state: crate::db::text(&row, "state")?,
        deadline: row.try_get("deadline")?,
        entry_mode: crate::db::text(&row, "entry_mode")?,
        requirements: requirements(row.try_get("requirements")?)?,
        review: row.try_get::<i64, _>("review")? == 1,
        early_draw: row.try_get::<i64, _>("early_draw")? == 1,
    })
}

/// The `requirements` column: NULL means no participation requirements, otherwise a JSON array of strings.
pub(super) fn requirements(raw: Option<String>) -> Result<Vec<String>> {
    match raw {
        None => Ok(vec![]),
        Some(raw) => Ok(serde_json::from_str(&raw)?),
    }
}

/// The entry table for one entry method.
pub(super) fn entry_table(entry_mode: &str) -> &'static str {
    if entry_mode == "click" {
        "gonof_forum_lottery_signup"
    } else {
        "gonof_forum_lottery_entry"
    }
}

/// Writes one operation log entry: `uid` is the member acted on (the author themself if none); only IDs and the reason
/// are recorded, never redemption codes.
async fn audit(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: i64,
    uid: i64,
    kind: &str,
    data: serde_json::Value,
    now: i64,
) -> Result<()> {
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
        .bind(uid)
        .bind(actor)
        .bind(kind)
        .bind(data.to_string())
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Writes the lottery row, the prizes and the slots of non-credit prizes in the publication transaction. The caller
/// writes the activity row and the budget freeze.
pub(super) async fn publish(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    spec: &ActivitySpec,
    ring: Option<&TwoFactorConfig>,
    credit: &str,
) -> Result<()> {
    let prizes = spec.lottery_prizes(credit);
    let (_, shares) = prize_totals(&prizes);
    let items: Vec<String> = spec
        .requirements
        .iter()
        .map(|item| item.trim().to_owned())
        .collect();
    let requirements = if items.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&items)?)
    };
    crate::db::query("INSERT INTO gonof_forum_lottery(tid,shares,entry_mode,requirements,review,early_draw) VALUES(?,?,?,?,?,?)")
        .bind(tid)
        .bind(shares)
        .bind(&spec.entry_mode)
        .bind(requirements)
        .bind(i64::from(spec.review))
        .bind(i64::from(spec.early_draw))
        .execute(db.observe(&mut **tx))
        .await?;
    let mut sql = String::from(
        "INSERT INTO gonof_forum_lottery_prize(tid,prize_no,name,winners,kind,amount,note) VALUES",
    );
    sql.push_str(&vec!["(?,?,?,?,?,?,?)"; prizes.len()].join(","));
    let mut insert = crate::db::query(&sql);
    for (index, prize) in prizes.iter().enumerate() {
        insert = insert
            .bind(tid)
            .bind(index as i64 + 1)
            .bind(prize.name.trim().to_owned())
            .bind(prize.winners)
            .bind(prize.kind.clone())
            .bind(prize.amount)
            .bind(prize.note.trim().to_owned());
    }
    insert.execute(db.observe(&mut **tx)).await?;
    let mut slots: Vec<(i64, i64, Option<String>)> = Vec::new();
    for (index, prize) in prizes.iter().enumerate() {
        let number = index as i64 + 1;
        match prize.kind.as_str() {
            "code" => {
                let ring = ring
                    .ok_or_else(|| problem(crate::i18n::current().activity_error_codes_save()))?;
                for (slot, code) in prize.codes.iter().enumerate() {
                    let slot = slot as i64 + 1;
                    let sealed = ring.seal_lottery_code(tid, number, slot, code.trim())?;
                    slots.push((number, slot, Some(sealed)));
                }
            }
            "item" => slots.extend((1..=prize.winners).map(|slot| (number, slot, None))),
            _ => {}
        }
    }
    for chunk in slots.chunks(100) {
        let mut sql = String::from(
            "INSERT INTO gonof_forum_lottery_award(tid,prize_no,slot_no,code_ciphertext) VALUES",
        );
        sql.push_str(&vec!["(?,?,?,?)"; chunk.len()].join(","));
        let mut insert = crate::db::query(&sql);
        for (prize, slot, code) in chunk {
            insert = insert.bind(tid).bind(*prize).bind(*slot).bind(code.clone());
        }
        insert.execute(db.observe(&mut **tx)).await?;
    }
    Ok(())
}

/// One-click entry. Returns `true` if this call newly entered, `false` if the member had already entered.
pub async fn enter(
    db: &Database,
    actor: Actor,
    tid: i64,
    confirmed: &[i64],
    now: i64,
) -> Result<bool> {
    enter_inner(db, actor, tid, confirmed, Some(now)).await
}
pub async fn enter_live(db: &Database, actor: Actor, tid: i64, confirmed: &[i64]) -> Result<bool> {
    retry_transaction(|| enter_inner(db, actor, tid, confirmed, None)).await
}
async fn enter_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    confirmed: &[i64],
    timestamp: Option<i64>,
) -> Result<bool> {
    valid(tid > 0, crate::i18n::current().activity_error_no_lottery())?;
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    reject_muted(actor_locked(db, &mut tx, actor, false).await?)?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    let lottery = lottery_locked(db, &mut tx, tid).await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        lottery.kind == 3 && lottery.entry_mode == "click",
        crate::i18n::current().activity_error_not_click(),
    )?;
    if crate::db::query("SELECT dateline FROM gonof_forum_lottery_signup WHERE tid=? AND uid=?")
        .bind(tid)
        .bind(actor.uid)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .is_some()
    {
        tx.commit().await?;
        return Ok(false);
    }
    valid(
        lottery.owner != actor.uid,
        crate::i18n::current().activity_error_own_lottery(),
    )?;
    valid(
        readable_thread(&thread)? && lottery.state == "open" && now < lottery.deadline,
        crate::i18n::current().activity_error_lottery_closed(),
    )?;
    let expected: BTreeSet<i64> = (1..=lottery.requirements.len() as i64).collect();
    let given: BTreeSet<i64> = confirmed.iter().copied().collect();
    valid(
        given == expected,
        crate::i18n::current().activity_error_confirm(),
    )?;
    funding(db, &mut tx, actor.uid).await?;
    crate::db::query("INSERT INTO gonof_forum_lottery_signup(tid,uid,dateline) VALUES(?,?,?)")
        .bind(tid)
        .bind(actor.uid)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// The author removes an entrant before the draw; a reason is required. Removing an already removed entrant again
/// returns unchanged and keeps the original reason.
pub async fn exclude(
    db: &Database,
    actor: Actor,
    tid: i64,
    uid: i64,
    reason: &str,
    now: i64,
) -> Result<()> {
    review_inner(db, actor, tid, uid, Some(reason), Some(now)).await
}
pub async fn exclude_live(
    db: &Database,
    actor: Actor,
    tid: i64,
    uid: i64,
    reason: &str,
) -> Result<()> {
    retry_transaction(|| review_inner(db, actor, tid, uid, Some(reason), None)).await
}
/// The author undoes a removal before the draw. An entrant who was never removed returns unchanged.
pub async fn restore(db: &Database, actor: Actor, tid: i64, uid: i64, now: i64) -> Result<()> {
    review_inner(db, actor, tid, uid, None, Some(now)).await
}
pub async fn restore_live(db: &Database, actor: Actor, tid: i64, uid: i64) -> Result<()> {
    retry_transaction(|| review_inner(db, actor, tid, uid, None, None)).await
}
async fn review_inner(
    db: &Database,
    actor: Actor,
    tid: i64,
    uid: i64,
    reason: Option<&str>,
    timestamp: Option<i64>,
) -> Result<()> {
    valid(
        tid > 0 && uid > 0,
        crate::i18n::current().activity_error_entrant(),
    )?;
    let reason = reason.map(str::trim);
    if let Some(reason) = reason {
        valid(
            !reason.is_empty() && reason.chars().count() <= 200 && !reason.contains('\0'),
            crate::i18n::current().activity_error_exclude_reason(),
        )?;
    }
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    lock_thread(db, &mut tx, tid).await?;
    let lottery = lottery_locked(db, &mut tx, tid).await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        lottery.kind == 3 && lottery.owner == actor.uid,
        crate::i18n::current().activity_error_review_owner(),
    )?;
    valid(
        lottery.review,
        crate::i18n::current().activity_error_no_review(),
    )?;
    valid(
        lottery.state == "open" && now < lottery.deadline,
        crate::i18n::current().activity_error_locked(),
    )?;
    let entered = crate::db::query(&format!(
        "SELECT uid FROM {} WHERE tid=? AND uid=?",
        entry_table(&lottery.entry_mode)
    ))
    .bind(tid)
    .bind(uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .is_some();
    valid(entered, crate::i18n::current().activity_error_not_entered())?;
    let current =
        crate::db::query("SELECT reason FROM gonof_forum_lottery_exclusion WHERE tid=? AND uid=?")
            .bind(tid)
            .bind(uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    match (reason, current) {
        (Some(_), Some(_)) | (None, None) => {
            tx.commit().await?;
            return Ok(());
        }
        (Some(reason), None) => {
            crate::db::query(
                "INSERT INTO gonof_forum_lottery_exclusion(tid,uid,reason,dateline) VALUES(?,?,?,?)",
            )
            .bind(tid)
            .bind(uid)
            .bind(reason)
            .bind(now)
            .execute(db.observe(&mut *tx))
            .await?;
            audit(
                db,
                &mut tx,
                actor.uid,
                uid,
                "activity.lottery.exclude",
                serde_json::json!({"tid": tid, "reason": reason}),
                now,
            )
            .await?;
        }
        (None, Some(_)) => {
            crate::db::query("DELETE FROM gonof_forum_lottery_exclusion WHERE tid=? AND uid=?")
                .bind(tid)
                .bind(uid)
                .execute(db.observe(&mut *tx))
                .await?;
            audit(
                db,
                &mut tx,
                actor.uid,
                uid,
                "activity.lottery.restore",
                serde_json::json!({"tid": tid}),
                now,
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Early draw by the author: step 0 of a deadline draw (lock the list, generate the seed, mark `drawing`) and step 1
/// (fix the eligible entrants and the winner list, assign codes and other prize slots) in one transaction; paying
/// credits is left to the background job (one round every 5 seconds), and a lottery with no credits to pay is settled
/// here. `deadline` becomes the draw time + 1, so entries and replies in the same second that commit before this
/// transaction still qualify, and since the list is fixed here, a reply posted after it in that second does not; the
/// original deadline is kept in `planned_deadline`. Returns `true` if this call started the draw, `false` if it was
/// already under way.
pub async fn draw_now(db: &Database, actor: Actor, tid: i64, now: i64) -> Result<bool> {
    draw_inner(db, actor, tid, Some(now)).await
}
pub async fn draw_now_live(db: &Database, actor: Actor, tid: i64) -> Result<bool> {
    retry_transaction(|| draw_inner(db, actor, tid, None)).await
}
async fn draw_inner(db: &Database, actor: Actor, tid: i64, timestamp: Option<i64>) -> Result<bool> {
    valid(tid > 0, crate::i18n::current().activity_error_no_lottery())?;
    // READ COMMITTED: the eligibility read below sees every entry and reply committed before the thread lock.
    let mut tx = db.begin_content_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    let thread = lock_thread(db, &mut tx, tid).await?;
    let lottery = lottery_locked(db, &mut tx, tid).await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        lottery.kind == 3 && lottery.owner == actor.uid,
        crate::i18n::current().activity_error_draw_owner(),
    )?;
    valid(
        lottery.early_draw,
        crate::i18n::current().activity_error_no_early_draw(),
    )?;
    if lottery.state == "drawing" {
        tx.commit().await?;
        return Ok(false);
    }
    valid(
        lottery.state == "open" && now < lottery.deadline,
        crate::i18n::current().activity_error_draw_closed(),
    )?;
    valid(
        readable_thread(&thread)?,
        crate::i18n::current().activity_error_thread(),
    )?;
    crate::db::query("UPDATE gonof_forum_lottery SET planned_deadline=? WHERE tid=?")
        .bind(lottery.deadline)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
    crate::db::query("UPDATE gonof_forum_activity SET state=?,deadline=? WHERE tid=?")
        .bind(super::invariants::State::Drawing.value())
        .bind(now + 1)
        .bind(tid)
        .execute(db.observe(&mut *tx))
        .await?;
    crate::db::query("INSERT INTO gonof_forum_activity_draw(tid,seed,eligible,winners,dateline) VALUES(?,?,'[]','[]',?) ON DUPLICATE KEY UPDATE tid=tid")
        .bind(tid)
        .bind(crate::identity::random_token())
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    let seed: String = crate::db::query_scalar(
        "SELECT seed FROM gonof_forum_activity_draw WHERE tid=? FOR UPDATE",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let row = crate::db::query(
        "SELECT a.remaining,l.shares FROM gonof_forum_activity a JOIN gonof_forum_lottery l ON l.tid=a.tid WHERE a.tid=?",
    )
    .bind(tid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let lot = super::worker::Lot {
        tid,
        owner: lottery.owner,
        deadline: now + 1,
        remaining: row.try_get("remaining")?,
        shares: row.try_get("shares")?,
        click: lottery.entry_mode == "click",
        seed,
    };
    super::worker::decide(db, &mut tx, &lot, now).await?;
    audit(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "activity.lottery.draw",
        serde_json::json!({"tid": tid, "planned_deadline": lottery.deadline}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// One row of the review page. Only reply entries have `source_pid`.
#[derive(Clone, Debug, Serialize)]
pub struct ReviewEntry {
    pub uid: i64,
    pub username: String,
    pub dateline: i64,
    pub source_pid: Option<i64>,
    pub excluded: Option<String>,
}
/// Review page: the author and administrators can view it; only the author can remove entrants, and only before the
/// draw.
#[derive(Clone, Debug, Serialize)]
pub struct ReviewPage {
    pub tid: i64,
    pub subject: String,
    pub entry_mode: String,
    pub review: bool,
    pub state: String,
    pub entries_total: i64,
    pub excluded_total: i64,
    pub entries: Vec<ReviewEntry>,
    pub next_after: Option<i64>,
    pub can_edit: bool,
}

/// Pages by uid ascending; `after` is the last uid of the previous page. Returns `None` for anyone who is neither the
/// author nor an administrator. `STRAIGHT_JOIN` pins reading from the entry table in primary-key order: when the member
/// table is smaller than the entry table (nearly every member entered) the optimizer switches to driving from the
/// member table and sorting, reading every entry for each page (measured on 5.7.44 and 8.4.11).
pub async fn review_page(
    db: &Database,
    viewer: i64,
    admin: bool,
    tid: i64,
    after: i64,
    now: i64,
) -> Result<Option<ReviewPage>> {
    let Some(row) = crate::db::query("SELECT a.owner_uid,a.kind,a.state,a.deadline,l.entry_mode,l.review,t.subject FROM gonof_forum_activity a JOIN gonof_forum_lottery l ON l.tid=a.tid JOIN gonof_forum_thread t ON t.tid=a.tid WHERE a.tid=?")
        .bind(tid)
        .fetch_optional(db.executor())
        .await?
    else {
        return Ok(None);
    };
    let owner: i64 = row.try_get("owner_uid")?;
    if owner != viewer && !admin {
        return Ok(None);
    }
    let entry_mode = crate::db::text(&row, "entry_mode")?;
    let table = entry_table(&entry_mode);
    let source = if entry_mode == "click" {
        "NULL"
    } else {
        "e.source_pid"
    };
    let rows = crate::db::query(&format!("SELECT e.uid,m.username,e.dateline,{source} AS source_pid,x.reason FROM {table} e STRAIGHT_JOIN gonof_common_member m ON m.uid=e.uid LEFT JOIN gonof_forum_lottery_exclusion x ON x.tid=e.tid AND x.uid=e.uid WHERE e.tid=? AND e.uid>? ORDER BY e.uid LIMIT ?"))
        .bind(tid)
        .bind(after.max(0))
        .bind(ENTRY_PAGE + 1)
        .fetch_all(db.executor())
        .await?;
    let mut entries = rows
        .iter()
        .map(|r| {
            Ok(ReviewEntry {
                uid: r.try_get("uid")?,
                username: crate::db::text(r, "username")?,
                dateline: r.try_get("dateline")?,
                source_pid: r.try_get("source_pid")?,
                excluded: r.try_get("reason")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let next_after = if entries.len() as i64 > ENTRY_PAGE {
        entries.truncate(ENTRY_PAGE as usize);
        entries.last().map(|e| e.uid)
    } else {
        None
    };
    let entries_total: i64 =
        crate::db::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE tid=?"))
            .bind(tid)
            .fetch_one(db.executor())
            .await?;
    let excluded_total: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_lottery_exclusion WHERE tid=?")
            .bind(tid)
            .fetch_one(db.executor())
            .await?;
    let state = crate::db::text(&row, "state")?;
    let review = row.try_get::<i64, _>("review")? == 1;
    Ok(Some(ReviewPage {
        tid,
        subject: crate::db::text(&row, "subject")?,
        can_edit: owner == viewer
            && review
            && state == "open"
            && now < row.try_get::<i64, _>("deadline")?,
        entry_mode,
        review,
        state,
        entries_total,
        excluded_total,
        entries,
        next_after,
    }))
}

/// One slot on the author's redemption-code page. `code` is decrypted to plaintext only for the author; administrators
/// see `None`.
#[derive(Clone, Debug)]
pub struct CodeSlot {
    pub slot: i64,
    pub code: Option<String>,
    /// Has ciphertext that cannot be decrypted (the key that encrypted it is no longer in the configuration).
    pub unreadable: bool,
    pub winner: Option<String>,
    pub assigned: bool,
    pub revised_at: i64,
}
#[derive(Clone, Debug)]
pub struct CodePrize {
    pub number: i64,
    pub name: String,
    pub kind: String,
    pub slots: Vec<CodeSlot>,
}
#[derive(Clone, Debug)]
pub struct CodeBoard {
    pub tid: i64,
    pub subject: String,
    pub state: String,
    pub owner: bool,
    pub can_replace: bool,
    pub prizes: Vec<CodePrize>,
}

/// How redemption-code and other prizes were handed out. The author sees plaintext, administrators only see who
/// received them; returns `None` for everyone else.
pub async fn code_board(
    db: &Database,
    ring: &TwoFactorConfig,
    viewer: i64,
    admin: bool,
    tid: i64,
) -> Result<Option<CodeBoard>> {
    let Some(row) = crate::db::query("SELECT a.owner_uid,a.state,t.subject FROM gonof_forum_activity a JOIN gonof_forum_lottery l ON l.tid=a.tid JOIN gonof_forum_thread t ON t.tid=a.tid WHERE a.tid=?")
        .bind(tid)
        .fetch_optional(db.executor())
        .await?
    else {
        return Ok(None);
    };
    let owner = row.try_get::<i64, _>("owner_uid")? == viewer;
    if !owner && !admin {
        return Ok(None);
    }
    let state = crate::db::text(&row, "state")?;
    let prizes = crate::db::query(
        "SELECT prize_no,name,kind FROM gonof_forum_lottery_prize WHERE tid=? ORDER BY prize_no",
    )
    .bind(tid)
    .fetch_all(db.executor())
    .await?;
    let slots = crate::db::query("SELECT a.prize_no,a.slot_no,a.code_ciphertext,a.uid,a.revised_at,m.username FROM gonof_forum_lottery_award a LEFT JOIN gonof_common_member m ON m.uid=a.uid WHERE a.tid=? ORDER BY a.prize_no,a.slot_no")
        .bind(tid)
        .fetch_all(db.executor())
        .await?;
    let mut board = Vec::new();
    for prize in &prizes {
        let kind = crate::db::text(prize, "kind")?;
        if kind == "credit" {
            continue;
        }
        let number: i64 = prize.try_get("prize_no")?;
        let mut list = Vec::new();
        for slot in slots
            .iter()
            .filter(|s| s.try_get::<i64, _>("prize_no").ok() == Some(number))
        {
            let slot_no: i64 = slot.try_get("slot_no")?;
            let sealed: Option<String> = slot.try_get("code_ciphertext")?;
            let code = match (&sealed, owner) {
                (Some(raw), true) => ring.open_lottery_code(tid, number, slot_no, raw),
                _ => None,
            };
            list.push(CodeSlot {
                slot: slot_no,
                unreadable: owner && sealed.is_some() && code.is_none(),
                code,
                winner: slot.try_get("username")?,
                assigned: slot.try_get::<Option<i64>, _>("uid")?.is_some(),
                revised_at: slot.try_get("revised_at")?,
            });
        }
        board.push(CodePrize {
            number,
            name: crate::db::text(prize, "name")?,
            kind,
            slots: list,
        });
    }
    Ok(Some(CodeBoard {
        tid,
        subject: crate::db::text(&row, "subject")?,
        can_replace: owner && state == "open",
        state,
        owner,
        prizes: board,
    }))
}

/// Locks all slots of this lottery and returns the plaintext of existing redemption codes except `skip`, for duplicate
/// checks. Codes that cannot be decrypted are skipped.
async fn other_codes(
    db: &Database,
    tx: &mut Tx<'_>,
    ring: &TwoFactorConfig,
    tid: i64,
    skip: impl Fn(i64, i64) -> bool,
) -> Result<BTreeSet<String>> {
    let rows = crate::db::query("SELECT prize_no,slot_no,code_ciphertext FROM gonof_forum_lottery_award WHERE tid=? ORDER BY prize_no,slot_no FOR UPDATE")
        .bind(tid)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    let mut codes = BTreeSet::new();
    for row in rows {
        let prize: i64 = row.try_get("prize_no")?;
        let slot: i64 = row.try_get("slot_no")?;
        if skip(prize, slot) {
            continue;
        }
        if let Some(raw) = row.try_get::<Option<String>, _>("code_ciphertext")?
            && let Some(code) = ring.open_lottery_code(tid, prize, slot, &raw)
        {
            codes.insert(code);
        }
    }
    Ok(codes)
}

/// Replaces the whole set of codes of one redemption-code prize before the draw; the count must still equal the slots.
pub async fn replace_codes(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    prize_no: i64,
    codes: &[String],
    now: i64,
) -> Result<()> {
    replace_inner(db, ring, actor, tid, prize_no, codes, Some(now)).await
}
pub async fn replace_codes_live(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    prize_no: i64,
    codes: &[String],
) -> Result<()> {
    retry_transaction(|| replace_inner(db, ring, actor, tid, prize_no, codes, None)).await
}
async fn replace_inner(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    prize_no: i64,
    codes: &[String],
    timestamp: Option<i64>,
) -> Result<()> {
    let codes: Vec<&str> = codes.iter().map(|c| c.trim()).collect();
    valid(
        codes.iter().all(|c| valid_code(c)),
        crate::i18n::current().activity_error_code_format(),
    )?;
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    lock_thread(db, &mut tx, tid).await?;
    let lottery = lottery_locked(db, &mut tx, tid).await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        lottery.kind == 3 && lottery.owner == actor.uid,
        crate::i18n::current().activity_error_codes_owner(),
    )?;
    valid(
        lottery.state == "open",
        crate::i18n::current().activity_error_codes_drawn(),
    )?;
    let prize = crate::db::query(
        "SELECT kind,winners FROM gonof_forum_lottery_prize WHERE tid=? AND prize_no=?",
    )
    .bind(tid)
    .bind(prize_no)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or_else(|| problem(crate::i18n::current().activity_error_prize()))?;
    valid(
        crate::db::text(&prize, "kind")? == "code",
        crate::i18n::current().activity_error_not_code_prize(),
    )?;
    valid(
        codes.len() as i64 == prize.try_get::<i64, _>("winners")?,
        crate::i18n::current().activity_error_code_count(),
    )?;
    let mut seen = other_codes(db, &mut tx, ring, tid, |p, _| p == prize_no).await?;
    for code in &codes {
        valid(
            seen.insert((*code).to_owned()),
            crate::i18n::current().activity_error_code_duplicate(),
        )?;
    }
    for (index, code) in codes.iter().enumerate() {
        let slot = index as i64 + 1;
        let sealed = ring.seal_lottery_code(tid, prize_no, slot, code)?;
        let changed = crate::db::query("UPDATE gonof_forum_lottery_award SET code_ciphertext=? WHERE tid=? AND prize_no=? AND slot_no=? AND uid IS NULL")
            .bind(sealed)
            .bind(tid)
            .bind(prize_no)
            .bind(slot)
            .execute(db.observe(&mut *tx))
            .await?;
        valid(
            changed.rows_affected() == 1,
            crate::i18n::current().activity_error_code_slots(),
        )?;
    }
    audit(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "activity.lottery.codes",
        serde_json::json!({"tid": tid, "prize_no": prize_no, "count": codes.len()}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Corrects one already issued redemption code after the draw. `expected` is the `revised_at` seen on the page; a
/// mismatch means it was just changed elsewhere, and the overwrite is refused. The winner's page shows that the author
/// corrected it and when; the old code is not kept.
#[allow(clippy::too_many_arguments)]
pub async fn correct_code(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    prize_no: i64,
    slot_no: i64,
    code: &str,
    expected: i64,
    now: i64,
) -> Result<()> {
    correct_inner(
        db,
        ring,
        actor,
        tid,
        (prize_no, slot_no),
        code,
        expected,
        Some(now),
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn correct_code_live(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    prize_no: i64,
    slot_no: i64,
    code: &str,
    expected: i64,
) -> Result<()> {
    retry_transaction(|| {
        correct_inner(
            db,
            ring,
            actor,
            tid,
            (prize_no, slot_no),
            code,
            expected,
            None,
        )
    })
    .await
}
#[allow(clippy::too_many_arguments)]
async fn correct_inner(
    db: &Database,
    ring: &TwoFactorConfig,
    actor: Actor,
    tid: i64,
    (prize_no, slot_no): (i64, i64),
    code: &str,
    expected: i64,
    timestamp: Option<i64>,
) -> Result<()> {
    let code = code.trim();
    valid(
        valid_code(code),
        crate::i18n::current().activity_error_code_format(),
    )?;
    let mut tx = db.begin_write().await?;
    guard_write(db, &mut tx).await?;
    lock_members(db, &mut tx, &[actor.uid]).await?;
    actor_locked(db, &mut tx, actor, false).await?;
    lock_thread(db, &mut tx, tid).await?;
    let lottery = lottery_locked(db, &mut tx, tid).await?;
    let now = match timestamp {
        Some(now) => now,
        None => locked_now(),
    };
    valid(
        lottery.kind == 3 && lottery.owner == actor.uid,
        crate::i18n::current().activity_error_codes_owner(),
    )?;
    let mut seen =
        other_codes(db, &mut tx, ring, tid, |p, s| p == prize_no && s == slot_no).await?;
    valid(
        seen.insert(code.to_owned()),
        crate::i18n::current().activity_error_code_duplicate(),
    )?;
    let slot = crate::db::query("SELECT uid,code_ciphertext FROM gonof_forum_lottery_award WHERE tid=? AND prize_no=? AND slot_no=?")
        .bind(tid)
        .bind(prize_no)
        .bind(slot_no)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or_else(|| problem(crate::i18n::current().activity_error_code_slot()))?;
    valid(
        slot.try_get::<Option<String>, _>("code_ciphertext")?
            .is_some(),
        crate::i18n::current().activity_error_not_code_slot(),
    )?;
    valid(
        slot.try_get::<Option<i64>, _>("uid")?.is_some(),
        crate::i18n::current().activity_error_code_unsent(),
    )?;
    let sealed = ring.seal_lottery_code(tid, prize_no, slot_no, code)?;
    let changed = crate::db::query("UPDATE gonof_forum_lottery_award SET code_ciphertext=?,revised_at=? WHERE tid=? AND prize_no=? AND slot_no=? AND uid IS NOT NULL AND revised_at=?")
        .bind(sealed)
        .bind(now.max(expected + 1))
        .bind(tid)
        .bind(prize_no)
        .bind(slot_no)
        .bind(expected)
        .execute(db.observe(&mut *tx))
        .await?;
    valid(
        changed.rows_affected() == 1,
        crate::i18n::current().activity_error_code_revised(),
    )?;
    audit(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "activity.lottery.correct",
        serde_json::json!({"tid": tid, "prize_no": prize_no, "slot_no": slot_no}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// One item in "My prizes": redemption-code prizes and other prizes. Credit prizes are recorded in the
/// "My {credit}" ledger.
#[derive(Clone, Debug)]
pub struct PrizeItem {
    pub tid: i64,
    /// Whether the winner can still open the thread; when not, `subject` is empty and the page shows a placeholder
    /// instead of the title and link. The prize itself stays listed.
    pub available: bool,
    pub subject: String,
    pub prize: String,
    pub kind: String,
    pub note: String,
    pub code: Option<String>,
    pub unreadable: bool,
    pub assigned_at: i64,
    pub revised_at: i64,
}

/// The member's own redemption-code and other prizes, by thread descending; `before` is the last tid of the previous
/// page (0 = first page). `STRAIGHT_JOIN` pins reading the `(uid, tid)` index in descending order: when the prize table
/// is very small, 8.4 switches to driving from the prize table and sorting.
///
/// A thread the winner can no longer open (moved to a board they cannot read, hidden, withdrawn) keeps its prize on
/// the list without its current title: the same "can it be opened" check as the favorite and notification lists.
pub async fn my_prizes(
    db: &Database,
    ring: &TwoFactorConfig,
    uid: i64,
    before: i64,
) -> Result<(Vec<PrizeItem>, Option<i64>)> {
    let before = if before > 0 { before } else { i64::MAX };
    let available = format!(
        "{} AND t.deleted_at=0 AND {}",
        crate::content::thread_visible_by_forum_sql("f", Some(uid)),
        crate::content::post_visible("firstpost")
    );
    let rows = crate::db::query(&format!("SELECT a.tid,a.prize_no,a.slot_no,a.code_ciphertext,a.assigned_at,a.revised_at,p.name,p.kind,p.note,t.subject,CASE WHEN {available} THEN 1 ELSE 0 END AS available FROM gonof_forum_lottery_award a STRAIGHT_JOIN gonof_forum_lottery_prize p ON p.tid=a.tid AND p.prize_no=a.prize_no STRAIGHT_JOIN gonof_forum_thread t ON t.tid=a.tid STRAIGHT_JOIN gonof_forum_forum f ON f.fid=t.fid STRAIGHT_JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE a.uid=? AND a.tid<? ORDER BY a.tid DESC LIMIT ?"))
        .bind(uid)
        .bind(before)
        .bind(PRIZE_PAGE + 1)
        .fetch_all(db.executor())
        .await?;
    let mut items = Vec::new();
    for row in &rows {
        let tid: i64 = row.try_get("tid")?;
        let sealed: Option<String> = row.try_get("code_ciphertext")?;
        let code = sealed.as_deref().and_then(|raw| {
            ring.open_lottery_code(
                tid,
                row.try_get("prize_no").ok()?,
                row.try_get("slot_no").ok()?,
                raw,
            )
        });
        let available = row.try_get::<i64, _>("available")? == 1;
        items.push(PrizeItem {
            tid,
            available,
            subject: if available {
                crate::db::text(row, "subject")?
            } else {
                String::new()
            },
            prize: crate::db::text(row, "name")?,
            kind: crate::db::text(row, "kind")?,
            note: crate::db::text(row, "note")?,
            unreadable: sealed.is_some() && code.is_none(),
            code,
            assigned_at: row.try_get("assigned_at")?,
            revised_at: row.try_get("revised_at")?,
        });
    }
    let next = if items.len() as i64 > PRIZE_PAGE {
        items.truncate(PRIZE_PAGE as usize);
        items.last().map(|item| item.tid)
    } else {
        None
    };
    Ok((items, next))
}

/// Report of re-encrypting redemption codes after a key change; `unreadable` lists slots that could not be decrypted
/// (tid, prize number, slot number).
#[derive(Debug, Default)]
pub struct CodeRekeyReport {
    pub rekeyed: usize,
    pub current: usize,
    pub unreadable: Vec<(i64, i64, i64)>,
}

/// Second half of the CLI command `rekey-two-factor`: re-encrypts redemption codes still encrypted with an old key
/// under the current key. One conditional update per row, so a simultaneous correction by the author cannot be
/// overwritten wrongly.
pub async fn rekey_lottery_codes(db: &Database, ring: &TwoFactorConfig) -> Result<CodeRekeyReport> {
    ring.validate()?;
    let mut report = CodeRekeyReport::default();
    let mut after = (0_i64, 0_i64, 0_i64);
    loop {
        let rows = crate::db::query("SELECT tid,prize_no,slot_no,code_ciphertext FROM gonof_forum_lottery_award WHERE code_ciphertext IS NOT NULL AND (tid>? OR (tid=? AND (prize_no>? OR (prize_no=? AND slot_no>?)))) ORDER BY tid,prize_no,slot_no LIMIT 100")
            .bind(after.0)
            .bind(after.0)
            .bind(after.1)
            .bind(after.1)
            .bind(after.2)
            .fetch_all(db.executor())
            .await?;
        let Some(last) = rows.last() else {
            break;
        };
        after = (
            last.try_get("tid")?,
            last.try_get("prize_no")?,
            last.try_get("slot_no")?,
        );
        for row in &rows {
            let key: (i64, i64, i64) = (
                row.try_get("tid")?,
                row.try_get("prize_no")?,
                row.try_get("slot_no")?,
            );
            let raw = crate::db::text(row, "code_ciphertext")?;
            if ring.lottery_code_is_current(&raw) {
                report.current += 1;
                continue;
            }
            let Some(code) = ring.open_lottery_code(key.0, key.1, key.2, &raw) else {
                report.unreadable.push(key);
                continue;
            };
            let sealed = ring.seal_lottery_code(key.0, key.1, key.2, &code)?;
            let changed = crate::db::query("UPDATE gonof_forum_lottery_award SET code_ciphertext=? WHERE tid=? AND prize_no=? AND slot_no=? AND code_ciphertext=?")
                .bind(sealed)
                .bind(key.0)
                .bind(key.1)
                .bind(key.2)
                .bind(&raw)
                .execute(db.executor())
                .await?;
            if changed.rows_affected() == 1 {
                report.rekeyed += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prize_lines_split_codes_in_prize_order() {
        // Type words are the language's ("Code", "Other"), the site's points name or the fixed identifiers.
        let prizes = "First prize | 1 | Code | | Annual membership\n\nSecond prize | 2 | Coins | 50\nSouvenir | 1 | Other | | Message the author | with a bar\nThird prize | 2 | code";
        let list = parse_prize_lines(prizes, "AAA\n\n BBB \nCCC", "Coins").unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(
            (list[0].kind.as_str(), list[0].codes.clone()),
            ("code", vec!["AAA".to_owned()])
        );
        assert_eq!((list[1].kind.as_str(), list[1].amount), ("credit", 50));
        assert_eq!(list[2].note, "Message the author | with a bar");
        assert_eq!(list[3].codes, vec!["BBB".to_owned(), "CCC".to_owned()]);
        assert!(parse_prize_lines("First prize | 1 | Code", "A\nB", "Coins").is_err());
        assert!(parse_prize_lines("First prize | one | Coins | 5", "", "Coins").is_err());
        assert!(parse_prize_lines("First prize | 1 | Silver | 5", "", "Coins").is_err());
        assert!(parse_prize_lines("Name only", "", "Coins").is_err());
        // Too few codes still goes to validation, which reports "A code prize needs exactly one code per winner".
        assert_eq!(
            parse_prize_lines("First prize | 3 | Code", "A", "Coins").unwrap()[0]
                .codes
                .len(),
            1
        );
    }

    #[test]
    fn codes_accept_visible_ascii_without_quotes() {
        assert!(valid_code("ABCD-EFGH-1234"));
        assert!(valid_code("  gift code 42  "));
        assert!(valid_code(&"x".repeat(255)));
        assert!(!valid_code(&"x".repeat(256)));
        assert!(!valid_code(""));
        assert!(!valid_code("   "));
        assert!(!valid_code("say\"hi"));
        assert!(!valid_code("back\\slash"));
        // "\u{4e2d}\u{6587}\u{7801}" is the Chinese for "Chinese code": not ASCII.
        assert!(!valid_code("\u{4e2d}\u{6587}\u{7801}"));
        assert!(!valid_code("tab\there"));
    }
}
