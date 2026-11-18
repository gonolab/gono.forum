//! Email appeal credentials only authorize the member's own sanction records; they never create an ordinary login
//! session.
use super::*;
use crate::db::Tx;

/// Validity of the appeal access token (a security parameter, kept constant). The appeal window and handling deadline
/// are the admin-area settings `settings.governance.appeal_window_days` and `appeal_handling_days`.
pub const APPEAL_ACCESS_TTL: i64 = 3600;

/// How long after a sanction an appeal is possible (seconds). Read from the write gate inside the appeal transaction
/// (this transaction's first lock) and computed live from the sanction's effective time: after shortening it, people
/// already sanctioned and still within the old window lose the right to appeal (docs/settings-and-caching.md, "When
/// a change takes effect").
async fn appeal_window(db: &Database, tx: &mut Tx<'_>) -> Result<i64> {
    let settings = crate::runtime::locked(db, tx)
        .await
        .map_err(crate::organization::identity_error)?;
    Ok(settings
        .governance
        .appeal_window_days
        .saturating_mul(86_400))
}

fn access_hash(raw: &str, mail_ticket: bool) -> String {
    if mail_ticket {
        identity::token_hash(raw)
    } else {
        identity::token_hash(&format!("appeal.cookie:{raw}"))
    }
}

pub enum AppealAccess {
    Session(Actor),
    Token(String),
}
#[derive(Debug)]
pub struct Appeal {
    pub id: i64,
    pub uid: i64,
    pub sanction_id: i64,
    pub reason: String,
    pub status: String,
    pub handler_uid: Option<i64>,
    pub resolution: String,
    pub dateline: i64,
    pub handled_at: i64,
    pub request_key: String,
}
impl Appeal {
    pub fn label(&self) -> &str {
        let t = crate::i18n::current();
        match self.status.as_str() {
            "accepted" => t.governance_status_accepted(),
            "rejected" => t.governance_status_rejected(),
            _ => t.governance_status_pending(),
        }
    }
    pub fn submitted_time(&self) -> String {
        display_time(self.dateline)
    }
    pub fn handled_time(&self) -> String {
        display_time(self.handled_at)
    }
    /// Expected handling deadline: the acceptance time plus `settings.governance.appeal_handling_days` days, only for
    /// display and for flagging overdue items.
    pub fn due_time(&self, handling_days: &i64) -> String {
        display_time(
            self.dateline
                .saturating_add(handling_days.saturating_mul(86_400)),
        )
    }
    pub fn overdue(&self, handling_days: &i64) -> bool {
        self.status == "pending"
            && identity::now()
                > self
                    .dateline
                    .saturating_add(handling_days.saturating_mul(86_400))
    }
}
fn decode(row: crate::db::DbRow) -> Result<Appeal> {
    Ok(Appeal {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        sanction_id: row.try_get("sanction_id")?,
        reason: db::text(&row, "reason")?,
        status: db::text(&row, "status")?,
        handler_uid: row.try_get("handler_uid")?,
        resolution: db::text(&row, "resolution")?,
        dateline: row.try_get("dateline")?,
        handled_at: row.try_get("handled_at")?,
        request_key: db::text(&row, "request_key")?,
    })
}
#[derive(Debug)]
pub struct AppealCase {
    pub sanction: Sanction,
    pub appeal: Option<Appeal>,
    pub can_submit: bool,
    /// The appeal window (seconds) in effect when this record was read.
    pub window_seconds: i64,
}
impl AppealCase {
    pub fn deadline(&self) -> String {
        display_time(self.sanction.starts_at.saturating_add(self.window_seconds))
    }
}

/// The appeal window for pages that only read, from the settings in their read-only transaction (no lock: see
/// [`appeal_window`] for writes).
async fn appeal_window_read(db: &Database, tx: &mut Tx<'_>) -> Result<i64> {
    let settings = crate::runtime::read_in(db, tx)
        .await
        .map_err(crate::organization::identity_error)?;
    Ok(settings
        .governance
        .appeal_window_days
        .saturating_mul(86_400))
}

/// One member lock serializes token rotation, requests, sanctions and account changes.
async fn owner(
    db: &Database,
    tx: &mut Tx<'_>,
    access: &AppealAccess,
    now: i64,
    mail_ticket: bool,
) -> Result<i64> {
    owner_checked(db, tx, access, now, mail_ticket, " FOR UPDATE").await
}

/// [`owner`] without locks, for the pages that only read (the appeal list and one case) in a read-only transaction:
/// MySQL refuses locking reads in a READ ONLY transaction, and a write transaction would hold SQLite's only writer
/// for an ordinary page view. The checks are the same, on the rows as the snapshot has them.
async fn owner_read(
    db: &Database,
    tx: &mut Tx<'_>,
    access: &AppealAccess,
    now: i64,
) -> Result<i64> {
    owner_checked(db, tx, access, now, false, "").await
}

async fn owner_checked(
    db: &Database,
    tx: &mut Tx<'_>,
    access: &AppealAccess,
    now: i64,
    mail_ticket: bool,
    lock: &str,
) -> Result<i64> {
    let uid = match access {
        AppealAccess::Session(actor) => actor.uid,
        AppealAccess::Token(raw) => {
            if !valid_key(raw) {
                return Err(ContentWriteError::Unauthorized);
            }
            crate::db::query_scalar("SELECT uid FROM gonof_common_member_token WHERE token_hash=? AND purpose='appeal_access'")
                .bind(access_hash(raw, mail_ticket)).fetch_optional(db.observe(&mut **tx)).await?.ok_or(ContentWriteError::Unauthorized)?
        }
    };
    let member = crate::db::query(&format!(
        "SELECT credentials_version,`freeze`,email,account_state FROM gonof_common_member WHERE uid=?{lock}"
    ))
    .bind(uid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .ok_or(ContentWriteError::Unauthorized)?;
    if db::text(&member, "account_state")? != "active" {
        return Err(ContentWriteError::Unauthorized);
    }
    match access {
        AppealAccess::Session(actor) => {
            if member.try_get::<i64, _>("freeze")? != 0
                || member.try_get::<i64, _>("credentials_version")? != actor.credentials_version
            {
                return Err(ContentWriteError::Unauthorized);
            }
        }
        AppealAccess::Token(raw) => {
            let token=crate::db::query(&format!("SELECT email,dateline,expires_at FROM gonof_common_member_token WHERE uid=? AND token_hash=? AND purpose='appeal_access' AND consumed_at IS NULL AND revoked_at IS NULL{lock}"))
                .bind(uid).bind(access_hash(raw, mail_ticket)).fetch_optional(db.observe(&mut **tx)).await?.ok_or(ContentWriteError::Unauthorized)?;
            if member.try_get::<Option<String>, _>("email")?.as_deref()
                != Some(db::text(&token, "email")?.as_str())
                || token.try_get::<i64, _>("dateline")? > now
                || token.try_get::<i64, _>("expires_at")? <= now
            {
                return Err(ContentWriteError::Unauthorized);
            }
        }
    }
    Ok(uid)
}

pub async fn request_appeal_access(db: &Database, email: &str, now: i64) -> Result<()> {
    let Ok(email) = identity::normalize_email(email) else {
        return Ok(());
    };
    let mut tx = db.begin_write().await?;
    let member = crate::db::query(
        "SELECT uid,username,account_state,password,emailstatus FROM gonof_common_member WHERE email=? FOR UPDATE",
    )
    .bind(&email)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    let Some(member) = member else {
        tx.commit().await?;
        return Ok(());
    };
    // The link opens the member's sanction records and their one appeal, so it follows the rule of the data request
    // link: an account without a password gets it only at a verified email, which a mistyped or foreign address never
    // is (docs/roles.md, "Appeals"). The mail worker checks this again before sending.
    if db::text(&member, "account_state")? != "active"
        || !crate::privacy::email_link_allowed(&member)?
    {
        tx.commit().await?;
        return Ok(());
    }
    let uid: i64 = member.try_get("uid")?;
    let eligible: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_sanction WHERE uid=?")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    let recent:i64=crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid=? AND purpose='appeal_access' AND dateline>?")
        .bind(uid).bind(now-60).fetch_one(db.observe(&mut *tx)).await?;
    if eligible == 0 || recent > 0 {
        tx.commit().await?;
        return Ok(());
    }
    crate::db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND purpose='appeal_access' AND revoked_at IS NULL AND consumed_at IS NULL")
        .bind(now).bind(uid).execute(db.observe(&mut *tx)).await?;
    let raw = identity::random_token();
    insert_access(
        db,
        &mut tx,
        uid,
        &email,
        &access_hash(&raw, true),
        now,
        now + APPEAL_ACCESS_TTL,
    )
    .await?;
    // The same payload as the other account mail, so the mail is written in the language of the domain the request
    // came from and links back to it.
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,'appeal_access','member',?,?,?,?)")
        .bind(format!("appeal.access.{}",identity::token_hash(&raw))).bind(uid)
        .bind(crate::mail::identity_payload(&email, &db::text(&member, "username")?, &raw))
        .bind(now).bind(now).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(())
}
async fn insert_access(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    email: &str,
    digest: &str,
    now: i64,
    until: i64,
) -> Result<()> {
    use identity::rules::{TokenPurpose, insert_token};
    insert_token(
        db,
        tx,
        uid,
        TokenPurpose::AppealAccess,
        digest,
        email,
        now,
        until,
    )
    .await?;
    Ok(())
}
/// GET checks only. POST consumes the emailed link and rotates it into a short-lived HttpOnly cookie.
pub async fn check_appeal_access(db: &Database, raw: &str, now: i64) -> Result<()> {
    let mut tx = db.begin_write().await?;
    owner(db, &mut tx, &AppealAccess::Token(raw.into()), now, true).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn exchange_appeal_access(db: &Database, raw: &str, now: i64) -> Result<(String, i64)> {
    let mut tx = db.begin_write().await?;
    let uid = owner(db, &mut tx, &AppealAccess::Token(raw.into()), now, true).await?;
    let row=crate::db::query("SELECT email,expires_at FROM gonof_common_member_token WHERE token_hash=? AND uid=? AND purpose='appeal_access' FOR UPDATE")
        .bind(identity::token_hash(raw)).bind(uid).fetch_one(db.observe(&mut *tx)).await?;
    let until: i64 = row.try_get("expires_at")?;
    // Redeemed only once (guaranteed here, not by the schema): `owner` was already checked under the lock, and the
    // conditional update blocks once more.
    let consumed = crate::db::query("UPDATE gonof_common_member_token SET consumed_at=? WHERE token_hash=? AND uid=? AND purpose='appeal_access' AND consumed_at IS NULL AND revoked_at IS NULL AND dateline<=? AND expires_at>?")
        .bind(now).bind(identity::token_hash(raw)).bind(uid).bind(now).bind(now).execute(db.observe(&mut *tx)).await?.rows_affected();
    if consumed != 1 {
        return Err(ContentWriteError::Unauthorized);
    }
    let next = identity::random_token();
    insert_access(
        db,
        &mut tx,
        uid,
        &db::text(&row, "email")?,
        &access_hash(&next, false),
        now,
        until,
    )
    .await?;
    tx.commit().await?;
    Ok((next, until - now))
}

async fn case_locked(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    id: i64,
    (now, window): (i64, i64),
) -> Result<AppealCase> {
    let row = crate::db::query(
        "SELECT * FROM gonof_common_member_sanction WHERE uid=? AND id=? FOR UPDATE",
    )
    .bind(uid)
    .bind(id)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .ok_or(ContentWriteError::NotFound)?;
    let sanction = sanctions::decode(row)?;
    let appeal = crate::db::query(
        "SELECT * FROM gonof_common_member_appeal WHERE sanction_id=? AND uid=? FOR UPDATE",
    )
    .bind(id)
    .bind(uid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .map(decode)
    .transpose()?;
    Ok(case_of(sanction, appeal, (now, window)))
}

fn case_of(sanction: Sanction, appeal: Option<Appeal>, (now, window): (i64, i64)) -> AppealCase {
    let can_submit = appeal.is_none()
        && now >= sanction.starts_at
        && now <= sanction.starts_at.saturating_add(window);
    AppealCase {
        sanction,
        appeal,
        can_submit,
        window_seconds: window,
    }
}

/// The member's sanctions among `rows` (already read, newest first) with their appeals, read without locks in one
/// statement: the read-only counterpart of [`case_locked`] for a whole list page.
async fn cases_read(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    rows: Vec<crate::db::DbRow>,
    (now, window): (i64, i64),
) -> Result<Vec<AppealCase>> {
    let sanctions = rows
        .into_iter()
        .map(sanctions::decode)
        .collect::<Result<Vec<_>>>()?;
    if sanctions.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT * FROM gonof_common_member_appeal WHERE uid=? AND sanction_id IN ({})",
        vec!["?"; sanctions.len()].join(",")
    );
    let mut query = crate::db::query(&sql).bind(uid);
    for sanction in &sanctions {
        query = query.bind(sanction.id);
    }
    let mut appeals = query
        .fetch_all(db.observe(&mut **tx))
        .await?
        .into_iter()
        .map(decode)
        .collect::<Result<Vec<_>>>()?;
    Ok(sanctions
        .into_iter()
        .map(|sanction| {
            let appeal = appeals
                .iter()
                .position(|appeal| appeal.sanction_id == sanction.id)
                .map(|at| appeals.swap_remove(at));
            case_of(sanction, appeal, (now, window))
        })
        .collect())
}
pub async fn appeal_case(
    db: &Database,
    access: &AppealAccess,
    id: i64,
    now: i64,
) -> Result<AppealCase> {
    // Reading a case writes nothing: a read-only transaction without locks (see `owner_read`).
    let mut tx = db.begin_read_only().await?;
    let window = appeal_window_read(db, &mut tx).await?;
    let uid = owner_read(db, &mut tx, access, now).await?;
    let rows = crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE uid=? AND id=?")
        .bind(uid)
        .bind(id)
        .fetch_all(db.observe(&mut *tx))
        .await?;
    let mut cases = cases_read(db, &mut tx, uid, rows, (now, window)).await?;
    tx.commit().await?;
    cases.pop().ok_or(ContentWriteError::NotFound)
}
pub async fn appeal_cases(
    db: &Database,
    access: &AppealAccess,
    before: i64,
    now: i64,
) -> Result<Vec<AppealCase>> {
    if before < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_page(),
        ));
    }
    // A page of the list is one read of the sanctions and one of their appeals, in a read-only transaction without
    // locks, however many records it shows.
    let mut tx = db.begin_read_only().await?;
    let window = appeal_window_read(db, &mut tx).await?;
    let uid = owner_read(db, &mut tx, access, now).await?;
    let rows = crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE uid=? AND (?=0 OR id<?) ORDER BY id DESC LIMIT 21")
        .bind(uid).bind(before).bind(before).fetch_all(db.observe(&mut *tx)).await?;
    let cases = cases_read(db, &mut tx, uid, rows, (now, window)).await?;
    tx.commit().await?;
    Ok(cases)
}
pub async fn submit_appeal(
    db: &Database,
    access: &AppealAccess,
    sanction_id: i64,
    text: &str,
    key: &str,
    now: i64,
) -> Result<i64> {
    let text = reason(text, 1000)?;
    if !valid_key(key) || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_appeal_key(),
        ));
    }
    let mut tx = db.begin_write().await?;
    let window = appeal_window(db, &mut tx).await?;
    let uid = owner(db, &mut tx, access, now, false).await?;
    let case = case_locked(db, &mut tx, uid, sanction_id, (now, window)).await?;
    if let Some(existing) = case.appeal {
        if existing.reason != text {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_error_appeal_exists(),
            ));
        }
        tx.commit().await?;
        return Ok(existing.id);
    }
    if !case.can_submit {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_appeal_window(),
        ));
    }
    let previous: Option<i64> = crate::db::query_scalar(
        "SELECT id FROM gonof_common_member_appeal WHERE uid=? AND request_key=?",
    )
    .bind(uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if previous.is_some() {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_error_appeal_key_used(),
        ));
    }
    let id=crate::db::query("INSERT INTO gonof_common_member_appeal(uid,sanction_id,reason,dateline,request_key) VALUES(?,?,?,?,?)")
        .bind(uid).bind(sanction_id).bind(text).bind(now).bind(key).execute(db.observe(&mut *tx)).await?.last_insert_id() as i64;
    audit(
        db,
        &mut tx,
        uid,
        uid,
        "appeal.submit",
        serde_json::json!({"appeal_id":id,"sanction_id":sanction_id}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}
pub async fn appeal_queue(
    db: &Database,
    actor: Actor,
    status: &str,
    before: i64,
) -> Result<Vec<Appeal>> {
    administrator(db, actor).await?;
    if !["pending", "accepted", "rejected"].contains(&status) || before < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_appeal_filter(),
        ));
    }
    let rows=crate::db::query("SELECT * FROM gonof_common_member_appeal WHERE status=? AND (?=0 OR id>?) ORDER BY id ASC LIMIT 21")
        .bind(status).bind(before).bind(before).fetch_all(db.executor()).await?;
    rows.into_iter().map(decode).collect()
}
pub async fn review_appeal(db: &Database, actor: Actor, id: i64, now: i64) -> Result<AppealCase> {
    administrator(db, actor).await?;
    let row = crate::db::query("SELECT uid,sanction_id FROM gonof_common_member_appeal WHERE id=?")
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let mut tx = db.begin_write().await?;
    let window = appeal_window(db, &mut tx).await?;
    let uid = row.try_get("uid")?;
    if lock_accounts(db, &mut tx, actor, Some(uid)).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_appeal_admin(),
        ));
    }
    let result = case_locked(db, &mut tx, uid, row.try_get("sanction_id")?, (now, window)).await?;
    tx.commit().await?;
    Ok(result)
}
pub async fn resolve_appeal(
    db: &Database,
    actor: Actor,
    id: i64,
    status: &str,
    resolution: &str,
    now: i64,
) -> Result<()> {
    let text = reason(resolution, 1000)?;
    if !["accepted", "rejected"].contains(&status) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_appeal_result(),
        ));
    }
    administrator(db, actor).await?;
    let row = crate::db::query("SELECT uid,sanction_id FROM gonof_common_member_appeal WHERE id=?")
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let uid = row.try_get("uid")?;
    let mut tx = db.begin_write().await?;
    let window = appeal_window(db, &mut tx).await?;
    if lock_accounts(db, &mut tx, actor, Some(uid)).await? != 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_appeal_admin(),
        ));
    }
    let case = case_locked(db, &mut tx, uid, row.try_get("sanction_id")?, (now, window)).await?;
    let appeal = case.appeal.ok_or(ContentWriteError::NotFound)?;
    if appeal.status != "pending" {
        if appeal.status != status || appeal.resolution != text {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_manage_error_appeal_handled(),
            ));
        }
        tx.commit().await?;
        return Ok(());
    }
    if now < appeal.dateline {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_appeal_time(),
        ));
    }
    if status == "accepted" && case.sanction.revoked_at == 0 {
        // The audit log, the notification and the mail keep the same system sentence, in the language of this
        // request (the site language, or the domain's).
        let ended = crate::i18n::current().governance_notice_appeal_ended();
        sanctions::end_sanction(
            db,
            &mut tx,
            &case.sanction,
            Some(actor.uid),
            ended,
            ended,
            now,
        )
        .await?;
    }
    // The handling conclusion is written only once (guaranteed here, not by the schema): pending was already checked
    // under the lock, and the conditional update blocks once more; the submitted content (appellant, sanction,
    // reason, time, request key) appears in no UPDATE (pinned by a source scan).
    let resolved = crate::db::query("UPDATE gonof_common_member_appeal SET status=?,handler_uid=?,resolution=?,handled_at=? WHERE id=? AND status='pending'")
        .bind(status).bind(actor.uid).bind(text).bind(now).bind(id).execute(db.observe(&mut *tx)).await?.rows_affected();
    if resolved != 1 {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_appeal_handled(),
        ));
    }
    let key = format!("sanction.{}.appeal.{id}.resolved", case.sanction.id);
    // The notification and the mail keep only the administrator's conclusion; "Your appeal was accepted: …" is put
    // together and translated when the list is shown (`describe_notices`) and when the mail is delivered
    // (`mail::governance`).
    notify(
        db,
        &mut tx,
        Notice {
            uid,
            authorid: actor.uid,
            kind: crate::notifications::record::Kind::Sanction,
            target: crate::notifications::record::Target::Sanction(case.sanction.id),
            key: &key,
            reason: text,
            now,
        },
    )
    .await?;
    queue_governance_mail(
        db,
        &mut tx,
        uid,
        case.sanction.id,
        Some(id),
        "appeal_result",
        &key,
        text,
        now,
    )
    .await?;
    audit(db,&mut tx,actor.uid,uid,"appeal.resolve",serde_json::json!({"appeal_id":id,"sanction_id":case.sanction.id,"result":status,"reason":text}),now).await?;
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn queue_governance_mail(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    sanction_id: i64,
    appeal_id: Option<i64>,
    event_type: &str,
    key: &str,
    reason: &str,
    now: i64,
) -> Result<()> {
    let member = crate::db::query(
        "SELECT email,emailstatus,username,account_state FROM gonof_common_member WHERE uid=?",
    )
    .bind(uid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    // The mail carries the operator's reason, so only a verified address gets it; the notification on the site is
    // written either way (docs/roles.md, "Account sanctions").
    if db::text(&member, "account_state")? != "active"
        || member.try_get::<i64, _>("emailstatus")? != 1
    {
        return Ok(());
    }
    let email: Option<String> = member.try_get("email")?;
    let Some(email) = email.filter(|email| !email.is_empty()) else {
        return Ok(());
    };
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'member',?,?,?,?)")
        .bind(format!("mail.{key}")).bind(event_type).bind(uid)
        .bind(serde_json::json!({"to":email,"username":db::text(&member,"username")?,"sanction_id":sanction_id,"appeal_id":appeal_id,"reason":reason}).to_string())
        .bind(now).bind(now).execute(db.observe(&mut **tx)).await?;
    Ok(())
}

/// Revocation is allowed even after expiry; possession only authorizes closing this grant.
pub async fn revoke_appeal_access(db: &Database, raw: &str, now: i64) -> Result<()> {
    if !valid_key(raw) {
        return Ok(());
    }
    let digest = access_hash(raw, false);
    let mut tx = db.begin_write().await?;
    let uid: Option<i64> = crate::db::query_scalar(
        "SELECT uid FROM gonof_common_member_token WHERE token_hash=? AND purpose='appeal_access'",
    )
    .bind(&digest)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if let Some(uid) = uid {
        crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
        crate::db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND token_hash=? AND purpose='appeal_access' AND consumed_at IS NULL AND revoked_at IS NULL")
            .bind(now).bind(uid).bind(digest).execute(db.observe(&mut *tx)).await?;
    }
    tx.commit().await?;
    Ok(())
}
