//! Durable owner-scoped exports and irreversible erasure. Stable identities remain for accounting.
use crate::db::Tx;
use crate::{
    db::{self, Database, DbRow},
    identity::{self, Actor, IdentityError},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
mod access;
mod export;
mod worker;
pub use access::*;
pub use worker::process_pending;
pub type Result<T> = std::result::Result<T, IdentityError>;
pub const COOLING_SECONDS: i64 = 7 * 86400;
pub const EXPORT_SECONDS: i64 = 7 * 86400;
/// Why the task of a request the member cancelled stops (the task's `last_error`; no page shows it).
const CANCELLED_REASON: &str = "Cancelled by the member";
const COLUMNS: &str = "id,uid,kind,content_mode,status,dateline,execute_after,finished_at,version,stage,cursor_pid,upper_pid,artifact_key,artifact_hash,artifact_bytes,artifact_expires_at,last_error";
#[derive(Clone, Debug, Serialize)]
pub struct DataRequest {
    pub id: i64,
    pub uid: i64,
    pub kind: String,
    pub content_mode: String,
    pub status: String,
    pub dateline: i64,
    pub execute_after: i64,
    pub finished_at: i64,
    pub version: i64,
    pub stage: String,
    pub cursor_pid: i64,
    pub upper_pid: i64,
    pub artifact_key: Option<String>,
    pub artifact_hash: Option<String>,
    pub artifact_bytes: Option<i64>,
    pub artifact_expires_at: Option<i64>,
    pub last_error: String,
}
impl DataRequest {
    pub fn label(&self) -> &str {
        let t = crate::i18n::current();
        match self.kind.as_str() {
            "export" => t.governance_data_label_export(),
            "erase" => t.governance_data_label_erase(),
            _ => t.governance_data_kind_close(),
        }
    }
    pub fn status_label(&self) -> &str {
        let t = crate::i18n::current();
        match self.status.as_str() {
            "cooling" => t.governance_data_status_cooling(),
            "pending" => t.governance_data_status_pending(),
            "processing" => t.governance_data_status_processing(),
            "ready" => t.governance_data_status_ready(),
            "completed" => t.governance_data_status_completed(),
            "cancelled" => t.governance_data_status_cancelled(),
            "expired" => t.governance_data_status_expired(),
            _ => t.governance_data_status_failed(),
        }
    }
    pub fn can_cancel(&self) -> bool {
        (self.kind == "export"
            && matches!(self.status.as_str(), "pending" | "processing" | "failed"))
            || (matches!(
                self.status.as_str(),
                "cooling" | "pending" | "processing" | "failed"
            ) && self.stage == "start")
    }
    pub fn can_download(&self) -> bool {
        self.status == "ready"
            && self
                .artifact_expires_at
                .is_some_and(|t| t > identity::now())
    }
    pub fn time(&self) -> String {
        crate::moderation::display_time(self.dateline)
    }
    pub fn due(&self) -> String {
        crate::moderation::display_time(self.execute_after)
    }
}
fn decode(r: &DbRow) -> Result<DataRequest> {
    Ok(DataRequest {
        id: r.try_get("id")?,
        uid: r.try_get("uid")?,
        kind: db::text(r, "kind")?,
        content_mode: db::text(r, "content_mode")?,
        status: db::text(r, "status")?,
        dateline: r.try_get("dateline")?,
        execute_after: r.try_get("execute_after")?,
        finished_at: r.try_get("finished_at")?,
        version: r.try_get("version")?,
        stage: db::text(r, "stage")?,
        cursor_pid: r.try_get("cursor_pid")?,
        upper_pid: r.try_get("upper_pid")?,
        artifact_key: r.try_get("artifact_key")?,
        artifact_hash: r.try_get("artifact_hash")?,
        artifact_bytes: r.try_get("artifact_bytes")?,
        artifact_expires_at: r.try_get("artifact_expires_at")?,
        last_error: db::text(r, "last_error")?,
    })
}
fn valid_key(v: &str) -> bool {
    crate::identity::valid_request_key(v)
}
/// Call while holding the member lock. Does not use freeze for account lifecycle.
pub async fn active_member(db: &Database, tx: &mut Tx<'_>, uid: i64) -> anyhow::Result<bool> {
    let state: Option<String> =
        db::query_scalar("SELECT account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    Ok(state.as_deref() == Some("active"))
}
/// Monetary entrypoints hold this same account lock, serializing admission with privacy requests.
pub async fn funding_allowed(db: &Database, tx: &mut Tx<'_>, uid: i64) -> anyhow::Result<bool> {
    if !active_member(db, tx, uid).await? {
        return Ok(false);
    }
    let current: Option<i64> = db::query_scalar("SELECT id FROM gonof_common_data_request WHERE uid=? AND kind IN ('close','erase') AND status IN ('cooling','pending','processing','failed') ORDER BY id LIMIT 1 FOR UPDATE")
        .bind(uid).fetch_optional(db.observe(&mut **tx)).await?;
    Ok(current.is_none())
}
async fn settled(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    let frozen: Option<i64> =
        db::query_scalar("SELECT frozen FROM gonof_common_credit_account WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    let activity:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_forum_activity a WHERE a.kind IN (1,2,3) AND a.state IN ('open','drawing') AND (a.owner_uid=? OR EXISTS (SELECT 1 FROM gonof_forum_lottery_entry e WHERE e.tid=a.tid AND e.uid=?) OR EXISTS (SELECT 1 FROM gonof_forum_lottery_signup s WHERE s.tid=a.tid AND s.uid=?) OR (a.kind=1 AND EXISTS(SELECT 1 FROM gonof_forum_post p WHERE p.tid=a.tid AND p.authorid=? AND p.first=0 AND p.erased_at=0)))").bind(uid).bind(uid).bind(uid).bind(uid).fetch_one(db.observe(&mut **tx)).await?;
    if frozen.unwrap_or(0) != 0 || activity != 0 {
        return Err(IdentityError::Conflict(UNSETTLED));
    }
    Ok(())
}
/// The refusal while funded activities, pending rewards or frozen points are open. It names the site's points, which
/// only the page knows: the data request page turns this marker into its sentence ([`unsettled_message`]); the
/// background task only logs it.
pub const UNSETTLED: &str = "funded activities, pending rewards or frozen points are still open";

/// The data request page's sentence for [`UNSETTLED`]; `credit` is the site's name for its points.
pub fn unsettled_message(lang: crate::i18n::Lang, credit: &str) -> String {
    lang.governance_error_unsettled(credit)
}

#[derive(Clone, Deserialize)]
pub struct NewRequest {
    pub kind: String,
    pub content_mode: String,
    pub request_key: String,
    pub password: String,
}
pub async fn submit(db: &Database, access: &Access, input: NewRequest, now: i64) -> Result<i64> {
    if !valid_key(&input.request_key)
        || now <= 0
        || !matches!(input.kind.as_str(), "export" | "close" | "erase")
        || !matches!(input.content_mode.as_str(), "retain" | "erase")
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().governance_error_data_request(),
        ));
    }
    // Password work happens outside the transaction; the exact hash is rechecked under the owner lock.
    let password = if let Access::Session(a) = access {
        let row = db::query("SELECT password FROM gonof_common_member WHERE uid=?")
            .bind(a.uid)
            .fetch_optional(db.executor())
            .await?
            .ok_or(IdentityError::Unauthorized)?;
        let hash: Option<String> = row.try_get("password")?;
        if input.password.len() > 512
            || !identity::verify_password(input.password.clone(), hash.clone()).await?
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().governance_error_wrong_password(),
            ));
        }
        Some(hash)
    } else {
        None
    };
    let mut tx = db.begin_content_write().await?;
    let uid = owner(db, &mut tx, access, now, false).await?;
    if let Some(expected) = password {
        let current: Option<String> =
            db::query_scalar("SELECT password FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
        if current != expected {
            return Err(IdentityError::Unauthorized);
        }
    }
    let mode = if input.kind == "erase" {
        "erase"
    } else {
        input.content_mode.as_str()
    };
    let hash =
        identity::token_hash(&serde_json::json!({"kind":input.kind,"mode":mode}).to_string());
    if let Some(row) = db::query(
        "SELECT id,request_hash FROM gonof_common_data_request WHERE uid=? AND request_key=?",
    )
    .bind(uid)
    .bind(&input.request_key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        if db::text(&row, "request_hash")? != hash {
            return Err(IdentityError::Conflict(
                crate::i18n::current().governance_error_data_key_used(),
            ));
        }
        return Ok(row.try_get("id")?);
    }
    let erasing:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_common_data_request WHERE uid=? AND kind<>'export' AND status IN ('processing','failed') AND stage<>'start'").bind(uid).fetch_one(db.observe(&mut *tx)).await?;
    if erasing > 0 {
        return Err(IdentityError::Conflict(
            crate::i18n::current().governance_error_data_erasing(),
        ));
    }
    if input.kind != "export" {
        let role: i64 = db::query_scalar("SELECT adminid FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
        if input.kind == "close" && role == 1 {
            return Err(IdentityError::Forbidden);
        }
        settled(db, &mut tx, uid).await?;
    }
    let active:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_common_data_request WHERE uid=? AND status IN ('pending','cooling','processing','failed') AND ((kind='export' AND ?='export') OR (kind<>'export' AND ?<>'export'))").bind(uid).bind(&input.kind).bind(&input.kind).fetch_one(db.observe(&mut *tx)).await?;
    if active > 0 {
        return Err(IdentityError::Conflict(
            crate::i18n::current().governance_error_data_active(),
        ));
    }
    let upper: Option<i64> =
        db::query_scalar("SELECT MAX(pid) FROM gonof_forum_post WHERE authorid=?")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    let due = if input.kind == "export" {
        now
    } else {
        now + COOLING_SECONDS
    };
    let status = if input.kind == "export" {
        "pending"
    } else {
        "cooling"
    };
    let result=db::query("INSERT INTO gonof_common_data_request(uid,kind,content_mode,status,request_key,request_hash,dateline,execute_after,upper_pid) VALUES(?,?,?,?,?,?,?,?,?)").bind(uid).bind(&input.kind).bind(mode).bind(status).bind(&input.request_key).bind(hash).bind(now).bind(due).bind(upper.unwrap_or(0)).execute(db.observe(&mut *tx)).await?;
    let id = result.last_insert_id() as i64;
    enqueue(db, &mut tx, id, uid, &input.kind, due, now).await?;
    audit(
        db,
        &mut tx,
        uid,
        "privacy.request",
        serde_json::json!({"request_id":id,"kind":input.kind,"content_mode":mode}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}
async fn enqueue(
    db: &Database,
    tx: &mut Tx<'_>,
    id: i64,
    uid: i64,
    kind: &str,
    due: i64,
    now: i64,
) -> Result<()> {
    db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'data_request',?,?,?,?) ON DUPLICATE KEY UPDATE eventid=eventid").bind(format!("privacy.request:{id}")).bind(if kind=="export" {"privacy.export"} else {"privacy.erase"}).bind(id).bind(serde_json::json!({"uid":uid}).to_string()).bind(now).bind(due).execute(db.observe(&mut **tx)).await?;
    Ok(())
}
async fn audit(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    kind: &str,
    value: serde_json::Value,
    now: i64,
) -> Result<()> {
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?, 'web',?)").bind(uid).bind(uid).bind(kind).bind(value.to_string()).bind(now).execute(db.observe(&mut **tx)).await?;
    Ok(())
}
pub async fn list(
    db: &Database,
    access: &Access,
    before: i64,
    now: i64,
) -> Result<(i64, Vec<DataRequest>)> {
    let mut tx = db.begin_write().await?;
    let uid = owner(db, &mut tx, access, now, false).await?;
    let rows=db::query(&format!("SELECT {COLUMNS} FROM gonof_common_data_request WHERE uid=? AND (?=0 OR id<?) ORDER BY id DESC LIMIT 21")).bind(uid).bind(before.max(0)).bind(before.max(0)).fetch_all(db.observe(&mut *tx)).await?;
    let result = rows.iter().map(decode).collect::<Result<_>>()?;
    tx.commit().await?;
    Ok((uid, result))
}
pub async fn cancel(db: &Database, access: &Access, id: i64, now: i64) -> Result<()> {
    let mut tx = db.begin_write().await?;
    let uid = owner(db, &mut tx, access, now, false).await?;
    let row = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_data_request WHERE id=? AND uid=? FOR UPDATE"
    ))
    .bind(id)
    .bind(uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(IdentityError::NotFound)?;
    let r = decode(&row)?;
    if r.status == "cancelled" {
        return Ok(());
    }
    if !r.can_cancel() {
        return Err(IdentityError::Conflict(
            crate::i18n::current().governance_error_data_started(),
        ));
    }
    db::query("UPDATE gonof_common_data_request SET status='cancelled',finished_at=?,version=version+1 WHERE id=?").bind(now).bind(id).execute(db.observe(&mut *tx)).await?;
    db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE event_key=?").bind(CANCELLED_REASON).bind(format!("privacy.request:{id}")).execute(db.observe(&mut *tx)).await?;
    audit(
        db,
        &mut tx,
        uid,
        "privacy.cancel",
        serde_json::json!({"request_id":id}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn manage(db: &Database, actor: Actor, before: i64) -> Result<Vec<DataRequest>> {
    if crate::manage::staff(db, actor).await? != 1 {
        return Err(IdentityError::Forbidden);
    }
    let rows=db::query(&format!("SELECT {COLUMNS} FROM gonof_common_data_request WHERE (?=0 OR id<?) ORDER BY id DESC LIMIT 21")).bind(before.max(0)).bind(before.max(0)).fetch_all(db.executor()).await?;
    rows.iter().map(decode).collect()
}
pub async fn retry(db: &Database, actor: Actor, id: i64, reason: &str, now: i64) -> Result<()> {
    if !(1..=500).contains(&reason.trim().chars().count()) {
        return Err(IdentityError::Validation(
            crate::i18n::current().governance_manage_error_retry_reason(),
        ));
    }
    if crate::manage::staff(db, actor).await? != 1 {
        return Err(IdentityError::Forbidden);
    }
    let mut tx = db.begin_write().await?;
    let target: i64 = db::query_scalar("SELECT uid FROM gonof_common_data_request WHERE id=?")
        .bind(id)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or(IdentityError::NotFound)?;
    for uid in std::collections::BTreeSet::from([actor.uid, target]) {
        db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    }
    let admin=db::query("SELECT adminid,account_state,credentials_version,`freeze` FROM gonof_common_member WHERE uid=? FOR UPDATE").bind(actor.uid).fetch_one(db.observe(&mut *tx)).await?;
    if admin.try_get::<i64, _>("adminid")? != 1
        || db::text(&admin, "account_state")? != "active"
        || admin.try_get::<i64, _>("freeze")? != 0
        || admin.try_get::<i64, _>("credentials_version")? != actor.credentials_version
    {
        return Err(IdentityError::Unauthorized);
    }
    let row = db::query(
        "SELECT status,execute_after FROM gonof_common_data_request WHERE id=? FOR UPDATE",
    )
    .bind(id)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if db::text(&row, "status")? != "failed" {
        return Err(IdentityError::Conflict(
            crate::i18n::current().governance_manage_error_retry_failed_only(),
        ));
    }
    let updated=db::query("UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),last_error=NULL WHERE event_key=? AND status IN ('pending','failed') AND lease_until<=?").bind(now.max(row.try_get("execute_after")?)).bind(format!("privacy.request:{id}")).bind(now).execute(db.observe(&mut *tx)).await?;
    if updated.rows_affected() != 1 {
        return Err(IdentityError::Conflict(
            crate::i18n::current().governance_manage_error_retry_leased(),
        ));
    }
    db::query("UPDATE gonof_common_data_request SET status='processing',last_error='',version=version+1 WHERE id=?").bind(id).execute(db.observe(&mut *tx)).await?;
    audit(
        db,
        &mut tx,
        actor.uid,
        "privacy.retry",
        serde_json::json!({"request_id":id,"target_uid":target,"reason":reason}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
pub fn directory(root: &Path, local: &Path) -> PathBuf {
    root.join(local).join("data-exports")
}
pub async fn download(
    db: &Database,
    access: &Access,
    id: i64,
    dir: &Path,
    now: i64,
) -> Result<(PathBuf, i64)> {
    let mut tx = db.begin_write().await?;
    let uid = owner(db, &mut tx, access, now, false).await?;
    let row = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_data_request WHERE id=? AND uid=? FOR UPDATE"
    ))
    .bind(id)
    .bind(uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(IdentityError::NotFound)?;
    let r = decode(&row)?;
    if r.status != "ready" || r.artifact_expires_at.is_none_or(|t| t <= now) {
        return Err(IdentityError::NotFound);
    }
    let key = r.artifact_key.ok_or(IdentityError::NotFound)?;
    if !safe_file(&key) {
        return Err(IdentityError::NotFound);
    }
    let path = dir.join(key).join("archive.zip");
    tx.commit().await?;
    Ok((path, r.artifact_bytes.unwrap_or(0)))
}
fn safe_file(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
}
