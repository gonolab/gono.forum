use super::*;
use crate::db::Tx;
use serde::Deserialize;

#[derive(Debug)]
pub struct Sanction {
    pub id: i64,
    pub uid: i64,
    pub kind: String,
    pub reason: String,
    pub operator_uid: i64,
    pub starts_at: i64,
    pub expires_at: i64,
    pub revoked_at: i64,
    pub revoked_by: Option<i64>,
}
impl Sanction {
    pub fn label(&self) -> &str {
        kind_label(&self.kind)
    }
    /// Global moderators can only revoke mutes; warnings and freezes can only be revoked by administrators.
    pub fn revocable_by(&self, role: i64) -> bool {
        self.revoked_at == 0 && (role == 1 || (role == 2 && self.kind == MUTE))
    }
    pub fn status(&self) -> &str {
        let t = crate::i18n::current();
        if self.revoked_at == 0 {
            t.governance_sanction_active()
        } else if self.revoked_by.is_some() {
            t.governance_sanction_revoked()
        } else {
            t.governance_sanction_expired()
        }
    }
    pub fn start_time(&self) -> String {
        display_time(self.starts_at)
    }
    pub fn end_time(&self) -> String {
        if self.expires_at == 0 {
            crate::i18n::current()
                .governance_sanction_no_expiry()
                .into()
        } else {
            display_time(self.expires_at)
        }
    }
    pub fn revoked_time(&self) -> String {
        display_time(self.revoked_at)
    }
}
/// Mute (docs/roles.md, "Mutes"): the member can log in, read, favorite, export and appeal, but cannot
/// post threads, reply, edit, send private messages, interact, join activities, or change avatar, bio or username.
/// The sanctions table records a row with `kind='mute'`, and `mute_until` on the member row is its copy, fetched in the
/// same statement when write entry points do the locking read of the member row.
pub const MUTE: &str = "mute";
/// The value written to `mute_until` for an indefinite mute: later than any moment, so the check is just
/// `mute_until > now`.
pub const MUTE_FOREVER: i64 = i64::MAX;

/// Whether the mute copy on the member row is in effect right now.
pub fn muted(mute_until: i64, now: i64) -> bool {
    mute_until > now
}

/// The message when a write is refused while muted, stating the expiry; indefinite mutes say
/// "until a moderator lifts it". The banner under the header uses [`muted_notice`].
pub fn muted_message(mute_until: i64) -> String {
    if mute_until == MUTE_FOREVER {
        crate::i18n::current().governance_muted_forever().into()
    } else {
        crate::i18n::current().governance_muted_until(display_time(mute_until))
    }
}

/// The same notice in a page's language, for the muted banner of the page frame.
pub fn muted_notice(lang: crate::i18n::Lang, mute_until: i64) -> String {
    if mute_until == MUTE_FOREVER {
        lang.frame_muted_forever().to_owned()
    } else {
        lang.frame_muted_until(display_time(mute_until))
    }
}

fn kind_label(kind: &str) -> &'static str {
    let t = crate::i18n::current();
    match kind {
        "warning" => t.governance_sanction_warning(),
        MUTE => t.governance_sanction_mute(),
        _ => t.governance_sanction_freeze(),
    }
}

/// Sanction kinds and the roles that may impose them: warnings and freezes only administrators; mutes both
/// administrators and global moderators.
fn may_issue(role: i64, kind: &str) -> bool {
    role == 1 || (role == 2 && kind == MUTE)
}

/// Recomputes the mute copy on the member row from the mutes still in effect: 0 if none, [`MUTE_FOREVER`] if any is
/// indefinite, otherwise the latest expiry.
async fn sync_mute(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    if !crate::privacy::active_member(db, tx, uid).await? {
        return Ok(());
    }
    let ends: Vec<i64> = crate::db::query_scalar(
        "SELECT expires_at FROM gonof_common_member_sanction WHERE uid=? AND kind='mute' AND revoked_at=0",
    )
    .bind(uid)
    .fetch_all(db.observe(&mut **tx))
    .await?;
    let until = ends
        .into_iter()
        .map(|end| if end == 0 { MUTE_FOREVER } else { end })
        .max()
        .unwrap_or(0);
    crate::db::query("UPDATE gonof_common_member SET mute_until=? WHERE uid=?")
        .bind(until)
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

pub(super) fn decode(row: crate::db::DbRow) -> Result<Sanction> {
    Ok(Sanction {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        kind: db::text(&row, "kind")?,
        reason: db::text(&row, "reason")?,
        operator_uid: row.try_get("operator_uid")?,
        starts_at: row.try_get("starts_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
        revoked_by: row.try_get("revoked_by")?,
    })
}
#[derive(Clone, Debug, Deserialize)]
pub struct SanctionRequest {
    pub uid: i64,
    pub expected_version: i64,
    pub kind: String,
    pub hours: i64,
    pub reason: String,
    pub request_key: String,
}
#[derive(Debug)]
pub struct SanctionMember {
    pub uid: i64,
    pub username: String,
    pub version: i64,
    pub frozen: bool,
    pub role: i64,
    /// The mute expiry on the member row (0 for none).
    pub mute_until: i64,
}
impl SanctionMember {
    /// The mute state shown on pages; one that has expired but whose sanction the background task has not yet ended is
    /// shown as lifted.
    pub fn mute_state(&self) -> String {
        let now = crate::identity::now();
        let t = crate::i18n::current();
        if !muted(self.mute_until, now) {
            t.governance_manage_not_muted().into()
        } else if self.mute_until == MUTE_FOREVER {
            t.governance_manage_muted_forever().into()
        } else {
            t.governance_manage_muted_until(display_time(self.mute_until))
        }
    }
    /// Whether this member can be muted: administrators and global moderators cannot.
    pub fn mutable(&self) -> bool {
        !matches!(self.role, 1 | 2)
    }
}
pub async fn sanction_member(db: &Database, actor: Actor, uid: i64) -> Result<SanctionMember> {
    sanction_staff(db, actor).await?;
    let row=crate::db::query("SELECT uid,username,credentials_version,`freeze`,mute_until,adminid FROM gonof_common_member WHERE uid=?").bind(uid).fetch_optional(db.executor()).await?.ok_or(ContentWriteError::NotFound)?;
    Ok(SanctionMember {
        uid,
        username: db::text(&row, "username")?,
        version: row.try_get("credentials_version")?,
        frozen: row.try_get::<i64, _>("freeze")? != 0,
        role: row.try_get("adminid")?,
        mute_until: row.try_get("mute_until")?,
    })
}
pub async fn sanctions(
    db: &Database,
    actor: Actor,
    uid: i64,
    staff: bool,
    before: i64,
) -> Result<Vec<Sanction>> {
    if before < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_sanction_page(),
        ));
    }
    if staff {
        sanction_staff(db, actor).await?;
    } else {
        active(db, actor).await?;
        if uid != actor.uid {
            return Err(ContentWriteError::NotFound);
        }
    }
    let rows=crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE uid=? AND (?=0 OR id<?) ORDER BY id DESC LIMIT 21")
        .bind(uid).bind(before).bind(before).fetch_all(db.executor()).await?;
    rows.into_iter().map(decode).collect()
}

pub async fn issue_sanction(
    db: &Database,
    actor: Actor,
    request: &SanctionRequest,
    now: i64,
) -> Result<i64> {
    let text = reason(&request.reason, 500)?;
    if request.uid <= 0
        || request.uid == actor.uid
        || now <= 0
        || request.expected_version <= 0
        || !valid_key(&request.request_key)
        || !matches!(request.kind.as_str(), "warning" | "freeze" | MUTE)
        || !(0..=8760).contains(&request.hours)
        || (request.kind == "warning" && request.hours != 0)
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_sanction_input(),
        ));
    }
    let mut tx = db.begin_write().await?;
    let role = lock_accounts(db, &mut tx, actor, Some(request.uid)).await?;
    if !may_issue(role, &request.kind) {
        return Err(ContentWriteError::Forbidden(if role == 2 {
            crate::i18n::current().governance_manage_error_gmod_mute_only()
        } else {
            crate::i18n::current().governance_manage_error_sanction_staff()
        }));
    }
    let previous=crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE operator_uid=? AND request_key=? FOR UPDATE").bind(actor.uid).bind(&request.request_key).fetch_optional(db.observe(&mut *tx)).await?;
    if let Some(row) = previous {
        let s = decode(row)?;
        if s.uid != request.uid
            || s.kind != request.kind
            || s.reason != text
            || (if s.expires_at == 0 {
                0
            } else {
                (s.expires_at - s.starts_at) / 3600
            }) != request.hours
        {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_manage_error_sanction_replay(),
            ));
        }
        tx.commit().await?;
        return Ok(s.id);
    }
    let member = crate::db::query(
        "SELECT credentials_version,adminid,`freeze`,mute_until,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE",
    )
    .bind(request.uid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if db::text(&member, "account_state")? != "active" {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_sanction_deleting(),
        ));
    }
    let target_role: i64 = member.try_get("adminid")?;
    if target_role == 1 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_sanction_admin_target(),
        ));
    }
    if request.kind == MUTE && target_role == 2 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_mute_gmod(),
        ));
    }
    if member.try_get::<i64, _>("credentials_version")? != request.expected_version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_sanction_stale(),
        ));
    }
    if request.kind == "freeze" && member.try_get::<i64, _>("freeze")? != 0 {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_already_frozen(),
        ));
    }
    if request.kind == MUTE && muted(member.try_get("mute_until")?, now) {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_already_muted(),
        ));
    }
    let until = if request.hours == 0 {
        0
    } else {
        now.checked_add(request.hours * 3600)
            .ok_or(ContentWriteError::Validation(
                crate::i18n::current().governance_manage_error_sanction_term(),
            ))?
    };
    let id=crate::db::query("INSERT INTO gonof_common_member_sanction(uid,kind,reason,operator_uid,starts_at,expires_at,request_key) VALUES(?,?,?,?,?,?,?)")
        .bind(request.uid).bind(&request.kind).bind(text).bind(actor.uid).bind(now).bind(until).bind(&request.request_key).execute(db.observe(&mut *tx)).await?.last_insert_id() as i64;
    if request.kind == "freeze" {
        freeze_state(db, &mut tx, request.uid, true, now).await?;
    }
    if request.kind == MUTE {
        sync_mute(db, &mut tx, request.uid).await?;
    }
    // The notification and the mail keep only the reason the operator wrote; the wording
    // ("Your account was frozen: …; until …") is put together and translated when the notification list is shown
    // ([`describe_notices`]) and when the mail is delivered (`mail::governance`, docs/architecture.md, "Languages").
    notify(
        db,
        &mut tx,
        Notice {
            uid: request.uid,
            authorid: actor.uid,
            kind: crate::notifications::record::Kind::Sanction,
            target: crate::notifications::record::Target::Sanction(id),
            key: &format!("sanction.{id}.issued"),
            reason: text,
            now,
        },
    )
    .await?;
    appeals::queue_governance_mail(
        db,
        &mut tx,
        request.uid,
        id,
        None,
        "sanction_issued",
        &format!("sanction.{id}.issued"),
        text,
        now,
    )
    .await?;
    audit(
        db,
        &mut tx,
        actor.uid,
        request.uid,
        "sanction.issue",
        serde_json::json!({"sanction_id":id,"kind":request.kind,"expires_at":until,"reason":text}),
        now,
    )
    .await?;
    if request.kind == "freeze" {
        crate::content::copies::bump_site(db, &mut tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}
async fn freeze_state(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    frozen: bool,
    now: i64,
) -> Result<()> {
    // Historical sanctions may end after closure; they must not mutate closed identities.
    if !crate::privacy::active_member(db, tx, uid).await? {
        return Ok(());
    }
    crate::db::query("UPDATE gonof_common_member SET `freeze`=?,credentials_version=credentials_version+1 WHERE uid=?").bind(i64::from(frozen)).bind(uid).execute(db.observe(&mut **tx)).await?;
    // Existing tokens cannot become usable again after the freeze ends.
    crate::db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND consumed_at IS NULL AND revoked_at IS NULL")
        .bind(now).bind(uid).execute(db.observe(&mut **tx)).await?;
    Ok(())
}
pub async fn revoke_sanction(
    db: &Database,
    actor: Actor,
    id: i64,
    text: &str,
    now: i64,
) -> Result<i64> {
    let text = reason(text, 500)?;
    sanction_staff(db, actor).await?;
    let uid: i64 =
        crate::db::query_scalar("SELECT uid FROM gonof_common_member_sanction WHERE id=?")
            .bind(id)
            .fetch_optional(db.executor())
            .await?
            .ok_or(ContentWriteError::NotFound)?;
    let mut tx = db.begin_write().await?;
    let role = lock_accounts(db, &mut tx, actor, Some(uid)).await?;
    if !matches!(role, 1 | 2) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_revoke_admin(),
        ));
    }
    let s = decode(
        crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE id=? FOR UPDATE")
            .bind(id)
            .fetch_one(db.observe(&mut *tx))
            .await?,
    )?;
    if !may_issue(role, &s.kind) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_gmod_revoke_mute_only(),
        ));
    }
    if s.revoked_at != 0 {
        tx.commit().await?;
        return Ok(uid);
    }
    if now < s.starts_at {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_revoke_time(),
        ));
    }
    end_sanction(db, &mut tx, &s, Some(actor.uid), text, text, now).await?;
    tx.commit().await?;
    Ok(uid)
}
/// When and how a sanction may end (the allowed revocation combinations): not before it starts; with an
/// operator it is a revocation, and without an operator it can only be the automatic end of a fixed-term freeze or
/// mute after expiry.
fn can_end(s: &Sanction, operator: Option<i64>, now: i64) -> bool {
    now >= s.starts_at
        && match operator {
            Some(_) => true,
            None => {
                matches!(s.kind.as_str(), "freeze" | MUTE)
                    && s.expires_at > 0
                    && now >= s.expires_at
            }
        }
}

/// `text` is the reason for the mail and the audit log; `notice` is what the notification keeps: the operator's
/// reason, or for an expiry or an accepted appeal the system's sentence in the site language (the list shows those
/// two translated by kind, [`describe_notices`]).
pub(super) async fn end_sanction(
    db: &Database,
    tx: &mut Tx<'_>,
    s: &Sanction,
    operator: Option<i64>,
    text: &str,
    notice: &str,
    now: i64,
) -> Result<()> {
    if !can_end(s, operator, now) {
        return Err(ContentWriteError::Internal(anyhow::anyhow!(
            "sanction {} cannot end at this time or in this way",
            s.id
        )));
    }
    // End only once (guaranteed here, not by the schema): callers have all read `revoked_at=0` under lock, and the
    // conditional update guards once more; no match means concurrent re-entry or a missed check by the caller, and
    // the whole transaction rolls back.
    let ended = crate::db::query(
        "UPDATE gonof_common_member_sanction SET revoked_at=?,revoked_by=? WHERE id=? AND revoked_at=0",
    )
    .bind(now)
    .bind(operator)
    .bind(s.id)
    .execute(db.observe(&mut **tx))
    .await?
    .rows_affected();
    if ended != 1 {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_sanction_ended(),
        ));
    }
    if s.kind == "freeze" {
        let remaining:i64=crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_sanction WHERE uid=? AND kind='freeze' AND revoked_at=0")
            .bind(s.uid).fetch_one(db.observe(&mut **tx)).await?;
        if remaining == 0 {
            freeze_state(db, tx, s.uid, false, now).await?;
        }
    }
    if s.kind == MUTE {
        sync_mute(db, tx, s.uid).await?;
    }
    let operator_uid = operator.unwrap_or(s.operator_uid);
    // The notification and the mail keep only `notice`; the wording ("Your account freeze has expired. …") is put
    // together when shown.
    notify(
        db,
        tx,
        Notice {
            uid: s.uid,
            authorid: operator_uid,
            kind: crate::notifications::record::Kind::Sanction,
            target: crate::notifications::record::Target::Sanction(s.id),
            key: &format!("sanction.{}.ended", s.id),
            reason: notice,
            now,
        },
    )
    .await?;
    appeals::queue_governance_mail(
        db,
        tx,
        s.uid,
        s.id,
        None,
        "sanction_ended",
        &format!("sanction.{}.ended", s.id),
        notice,
        now,
    )
    .await?;
    audit(
        db,
        tx,
        operator_uid,
        s.uid,
        if operator.is_some() {
            "sanction.revoke"
        } else {
            "sanction.expire"
        },
        serde_json::json!({"sanction_id":s.id,"reason":text}),
        now,
    )
    .await?;
    if s.kind == "freeze" {
        crate::content::copies::bump_site(db, tx).await?;
    }
    Ok(())
}
/// Bounded expiry processing; the member row serializes issuance, manual revocation and expiry.
pub async fn expire_sanctions(db: &Database, now: i64) -> Result<usize> {
    let rows=crate::db::query("SELECT id,uid,operator_uid FROM gonof_common_member_sanction WHERE kind IN ('freeze','mute') AND revoked_at=0 AND expires_at>0 AND expires_at<=? ORDER BY expires_at,id LIMIT 20")
        .bind(now).fetch_all(db.executor()).await?;
    if rows.is_empty() {
        return Ok(0);
    }
    // A background task has no request: the notification keeps the system's sentence in the site language, read
    // before any write transaction starts.
    let lang = crate::runtime::read(db)
        .await
        .map_err(|error| anyhow::anyhow!("cannot read the site language: {error}"))?
        .site
        .lang();
    let mut ended = 0;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let uid: i64 = row.try_get("uid")?;
        let operator: i64 = row.try_get("operator_uid")?;
        let mut tx = db.begin_write().await?;
        for member in BTreeSet::from([uid, operator]) {
            crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
                .bind(member)
                .fetch_one(db.observe(&mut *tx))
                .await?;
        }
        let s = decode(
            crate::db::query("SELECT * FROM gonof_common_member_sanction WHERE id=? FOR UPDATE")
                .bind(id)
                .fetch_one(db.observe(&mut *tx))
                .await?,
        )?;
        if s.revoked_at == 0 && s.expires_at > 0 && s.expires_at <= now {
            // The audit log, the notification and the mail keep the same system sentence in the site language.
            let notice = if s.kind == MUTE {
                lang.governance_notice_mute_over()
            } else {
                lang.governance_notice_freeze_over()
            };
            end_sanction(db, &mut tx, &s, None, notice, notice, now).await?;
            ended += 1;
        }
        tx.commit().await?;
    }
    Ok(ended)
}

/// The wording of sanction notifications on one page of the notification list, in this request's language (part of
/// [`super::describe_notices`]). A notification row keeps the operator's reason; what happened
/// ("Your account was frozen", "Your mute was revoked", "Your appeal was accepted" …) and the expiry are read from the
/// sanction and its appeal and put together here, by event key:
///
/// - `sanction.{id}.issued`: by the sanction's kind and `expires_at`;
/// - `sanction.{id}.ended`: revoked (`revoked_by` set, as `operator.is_some()` when it ended) or expired; an expiry,
///   and a revocation by an accepted appeal (the appeal handled at the very moment the sanction ended), show the
///   system's sentence instead of the stored one;
/// - `sanction.{id}.appeal.{aid}.resolved`: by the appeal's status (one appeal per sanction).
///
/// One bounded query for the whole page (at most a page of sanction ids, joined with their appeal); other rows and
/// rows whose sanction is missing keep the stored text.
pub(super) async fn describe_sanctions(
    db: &Database,
    uid: i64,
    items: &mut [crate::notifications::Notification],
) -> anyhow::Result<()> {
    let ids: BTreeSet<i64> = items
        .iter()
        .filter(|n| n.kind == "sanction" && n.event_key.starts_with("sanction."))
        .filter_map(|n| n.sanction_id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "SELECT s.id,s.kind,s.expires_at,s.revoked_at,s.revoked_by,a.status AS appeal_status,a.handled_at AS appeal_handled_at FROM gonof_common_member_sanction s LEFT JOIN gonof_common_member_appeal a ON a.sanction_id=s.id WHERE s.uid=? AND s.id IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut query = crate::db::query(&sql).bind(uid);
    for id in &ids {
        query = query.bind(*id);
    }
    struct Row {
        kind: String,
        expires_at: i64,
        revoked_at: i64,
        revoked: bool,
        appeal_status: Option<String>,
        appeal_handled_at: Option<i64>,
    }
    let mut rows = std::collections::HashMap::new();
    for row in query.fetch_all(db.executor()).await? {
        rows.insert(
            row.try_get::<i64, _>("id")?,
            Row {
                kind: db::text(&row, "kind")?,
                expires_at: row.try_get("expires_at")?,
                revoked_at: row.try_get("revoked_at")?,
                revoked: row.try_get::<Option<i64>, _>("revoked_by")?.is_some(),
                appeal_status: row.try_get("appeal_status")?,
                appeal_handled_at: row.try_get("appeal_handled_at")?,
            },
        );
    }
    let t = crate::i18n::current();
    for notice in items.iter_mut() {
        let Some(s) = notice.sanction_id.and_then(|id| rows.get(&id)) else {
            continue;
        };
        if notice.kind != "sanction" {
            continue;
        }
        let reason = notice.reason.as_str();
        let text = if notice.event_key.ends_with(".issued") {
            let until = display_time(s.expires_at);
            match (s.kind.as_str(), s.expires_at) {
                ("warning", _) => t.governance_notice_warning(reason),
                (MUTE, 0) => t.governance_notice_mute_forever(reason),
                (MUTE, _) => t.governance_notice_mute(reason, until),
                (_, 0) => t.governance_notice_freeze_forever(reason),
                _ => t.governance_notice_freeze(reason, until),
            }
        } else if notice.event_key.ends_with(".ended") {
            let by_appeal = s.appeal_status.as_deref() == Some("accepted")
                && s.appeal_handled_at == Some(s.revoked_at);
            let reason = if by_appeal {
                t.governance_notice_appeal_ended()
            } else {
                reason
            };
            match (s.revoked, s.kind.as_str()) {
                (true, "freeze") => t.governance_notice_freeze_revoked(reason),
                (true, MUTE) => t.governance_notice_mute_revoked(reason),
                (true, _) => t.governance_notice_warning_revoked(reason),
                (false, MUTE) => t.governance_notice_mute_expired(t.governance_notice_mute_over()),
                (false, _) => t.governance_notice_freeze_expired(t.governance_notice_freeze_over()),
            }
        } else if notice.event_key.ends_with(".resolved") && notice.event_key.contains(".appeal.") {
            match s.appeal_status.as_deref() {
                Some("accepted") => t.governance_notice_appeal_accepted(reason),
                Some("rejected") => t.governance_notice_appeal_rejected(reason),
                _ => continue,
            }
        } else {
            continue;
        };
        notice.reason = text;
    }
    Ok(())
}

/// Called only after the current password has been verified and the member row is locked.
pub(crate) async fn frozen_notice(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
) -> anyhow::Result<Option<String>> {
    let row=crate::db::query("SELECT reason,expires_at FROM gonof_common_member_sanction WHERE uid=? AND kind='freeze' AND revoked_at=0 ORDER BY id DESC LIMIT 1")
        .bind(uid).fetch_optional(db.observe(&mut **tx)).await?;
    row.map(|r| -> anyhow::Result<String> {
        let end: i64 = r.try_get("expires_at")?;
        let reason = db::text(&r, "reason")?;
        Ok(if end == 0 {
            crate::i18n::current().governance_frozen_forever(reason)
        } else {
            crate::i18n::current().governance_frozen_until(display_time(end), reason)
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanctions_end_only_the_ways_the_trigger_allowed() {
        let sanction = |kind: &str, expires_at: i64| Sanction {
            id: 1,
            uid: 2,
            kind: kind.into(),
            reason: "Reason".into(),
            operator_uid: 1,
            starts_at: 100,
            expires_at,
            revoked_at: 0,
            revoked_by: None,
        };
        // Revocation: has an operator, not before the start.
        assert!(can_end(&sanction("warning", 0), Some(1), 100));
        assert!(!can_end(&sanction("warning", 0), Some(1), 99));
        // Automatic end: only fixed-term freezes, after expiry.
        assert!(can_end(&sanction("freeze", 200), None, 200));
        assert!(!can_end(&sanction("freeze", 200), None, 199));
        assert!(!can_end(&sanction("freeze", 0), None, 300));
        assert!(!can_end(&sanction("warning", 0), None, 300));
    }
}
