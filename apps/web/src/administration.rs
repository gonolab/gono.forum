//! Bounded administrator lists and audited, replay-safe maintenance actions.
use crate::{
    db::{self, Database},
    identity::{self, Actor, IdentityError},
    runtime,
};
use serde::Deserialize;
pub type Result<T> = std::result::Result<T, IdentityError>;
pub const PAGE_SIZE: i64 = 20;

pub fn local_time(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "—".into();
    }
    let text = crate::clock::datetime(timestamp, &crate::clock::current());
    if text.is_empty() { "—".into() } else { text }
}

pub async fn admin(db: &Database, actor: Actor) -> Result<()> {
    if crate::manage::staff(db, actor).await? != 1 {
        return Err(IdentityError::Forbidden);
    }
    Ok(())
}
fn paging(page: i64, total: i64) -> Result<()> {
    if page < 1 || page > ((total + 19) / 20).max(1) {
        return Err(IdentityError::NotFound);
    }
    Ok(())
}
pub fn valid_key(key: &str) -> bool {
    crate::identity::valid_request_key(key)
}
pub fn reason(value: &str) -> Result<&str> {
    let value = value.trim();
    if !(1..=500).contains(&value.chars().count()) || value.contains('\0') {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_reason(),
        ));
    }
    Ok(value)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ListQuery {
    pub q: String,
    pub status: String,
    #[serde(deserialize_with = "optional_number")]
    pub uid: Option<i64>,
    #[serde(deserialize_with = "optional_number")]
    pub role: Option<i64>,
    #[serde(deserialize_with = "optional_number")]
    pub page: Option<i64>,
}
fn optional_number<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<i64>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    value
        .filter(|v| !v.is_empty())
        .map(|v| v.parse().map_err(serde::de::Error::custom))
        .transpose()
}
impl ListQuery {
    pub fn page(&self) -> i64 {
        self.page.unwrap_or(1)
    }
    pub fn validate(&self) -> Result<()> {
        if self.q.chars().count() > 100
            || self.page() < 1
            || self.page() > i64::MAX / PAGE_SIZE
            || self.uid.is_some_and(|v| v <= 0)
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_manage_error_filter(),
            ));
        }
        Ok(())
    }
}
pub struct User {
    pub uid: i64,
    pub username: String,
    pub email: String,
    /// The masked mobile number; `None` when none is bound.
    pub mobile: Option<String>,
    /// Bound third-party providers ("Google, GitHub"); an empty string when there are none.
    pub external: String,
    pub role: i64,
    pub frozen: bool,
    pub account_state: String,
    pub verified: bool,
    pub regdate: i64,
}
impl User {
    pub fn state_label(&self) -> &'static str {
        match self.account_state.as_str() {
            "deleting" => crate::i18n::current().account_manage_state_deleting(),
            "deleted" => crate::i18n::current().account_manage_state_deleted(),
            _ if self.frozen => crate::i18n::current().account_manage_state_frozen(),
            _ => crate::i18n::current().account_manage_state_normal(),
        }
    }
    pub fn joined(&self) -> String {
        local_time(self.regdate)
    }
}
pub async fn users(db: &Database, actor: Actor, query: &ListQuery) -> Result<(Vec<User>, i64)> {
    admin(db, actor).await?;
    query.validate()?;
    if query.role.is_some_and(|v| !(0..=5).contains(&v))
        || !matches!(
            query.status.as_str(),
            "" | "active" | "frozen" | "unverified" | "deleted" | "deleting"
        )
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_user_filter(),
        ));
    }
    let pattern = format!(
        "%{}%",
        query
            .q
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    );
    let mut conditions = String::from(
        "FROM gonof_common_member WHERE (username LIKE ? ESCAPE '!' OR email LIKE ? ESCAPE '!')",
    );
    if let Some(uid) = query.uid {
        conditions.push_str(&format!(" AND uid={uid}"));
    }
    if let Some(role) = query.role {
        conditions.push_str(&format!(" AND adminid={role}"));
    }
    conditions.push_str(match query.status.as_str() {
        "active" => " AND account_state='active' AND freeze=0",
        "frozen" => " AND freeze=1",
        "unverified" => " AND emailstatus=0",
        "deleted" => " AND account_state='deleted'",
        "deleting" => " AND account_state='deleting'",
        _ => "",
    });
    let total: i64 = db::query_scalar(&format!("SELECT COUNT(*) {conditions}"))
        .bind(&pattern)
        .bind(&pattern)
        .fetch_one(db.executor())
        .await?;
    paging(query.page(), total)?;
    let rows=db::query(&format!("SELECT uid,username,email,mobile,adminid,freeze,account_state,emailstatus,regdate {conditions} ORDER BY uid DESC LIMIT ? OFFSET ?"))
        .bind(&pattern).bind(&pattern).bind(PAGE_SIZE).bind((query.page()-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    let rows = rows
        .iter()
        .map(|row| {
            Ok(User {
                uid: row.try_get("uid")?,
                username: db::text(row, "username")?,
                email: row
                    .try_get::<Option<String>, _>("email")?
                    .unwrap_or_default(),
                // The admin area only shows masked numbers.
                mobile: row
                    .try_get::<Option<String>, _>("mobile")?
                    .map(|mobile| crate::identity::phone::mask_mobile(&mobile)),
                external: String::new(),
                role: row.try_get("adminid")?,
                frozen: row.try_get::<i64, _>("freeze")? != 0,
                account_state: db::text(row, "account_state")?,
                verified: row.try_get::<i64, _>("emailstatus")? == 1,
                regdate: row.try_get("regdate")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Read the third-party bindings of this page's members in one go (by uid IN on the unique key; at most twice
    // as many rows as members on the page).
    let mut rows = rows;
    let uids: Vec<i64> = rows.iter().map(|row| row.uid).collect();
    let links = crate::identity::external::linked_providers(db, &uids).await?;
    for row in &mut rows {
        row.external = links
            .iter()
            .filter(|(uid, _)| *uid == row.uid)
            .map(|(_, provider)| provider.label())
            .collect::<Vec<_>>()
            .join(crate::i18n::current().common_list_separator());
    }
    Ok((rows, total))
}

pub struct Invite {
    pub id: i64,
    pub uid: i64,
    pub owner: String,
    pub code: String,
    pub status: String,
    pub expires: i64,
    pub used_by: Option<i64>,
    pub request_key: String,
}
impl Invite {
    pub fn status_label(&self) -> &'static str {
        match self.status.as_str() {
            "used" => crate::i18n::current().account_manage_invite_used(),
            "revoked" => crate::i18n::current().account_manage_invite_revoked(),
            _ if self.expires <= identity::now() => {
                crate::i18n::current().account_manage_invite_expired()
            }
            "frozen" => crate::i18n::current().account_manage_invite_frozen(),
            _ => crate::i18n::current().account_manage_invite_available(),
        }
    }
    pub fn expiry(&self) -> String {
        local_time(self.expires)
    }
    pub fn can_change(&self) -> bool {
        self.used_by.is_none()
            && self.expires > identity::now()
            && matches!(self.status.as_str(), "available" | "frozen")
    }
}
pub async fn invites(db: &Database, actor: Actor, q: &ListQuery) -> Result<(Vec<Invite>, i64)> {
    admin(db, actor).await?;
    q.validate()?;
    if !matches!(
        q.status.as_str(),
        "" | "available" | "frozen" | "revoked" | "used" | "expired"
    ) {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_invite_filter(),
        ));
    }
    let mut filter = String::from(
        "FROM gonof_common_invite i JOIN gonof_common_member m ON m.uid=i.uid WHERE 1=1",
    );
    if let Some(uid) = q.uid {
        filter.push_str(&format!(" AND i.uid={uid}"));
    }
    if q.status == "expired" {
        filter.push_str(&format!(
            " AND i.endtime<={} AND i.fuid IS NULL",
            identity::now()
        ));
    } else if !q.status.is_empty() {
        filter.push_str(&format!(" AND i.status='{}'", q.status));
    }
    let total: i64 = db::query_scalar(&format!("SELECT COUNT(*) {filter}"))
        .fetch_one(db.executor())
        .await?;
    paging(q.page(), total)?;
    let rows=db::query(&format!("SELECT i.id,i.uid,m.username,i.code,i.status,i.endtime,i.fuid {filter} ORDER BY i.id DESC LIMIT ? OFFSET ?"))
        .bind(PAGE_SIZE).bind((q.page()-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    Ok((
        rows.iter()
            .map(|r| {
                let status = db::text(r, "status")?;
                let code = if matches!(status.as_str(), "available" | "frozen")
                    && r.try_get::<i64, _>("endtime")? > identity::now()
                {
                    db::text(r, "code")?
                } else {
                    String::new()
                };
                Ok(Invite {
                    id: r.try_get("id")?,
                    uid: r.try_get("uid")?,
                    owner: db::text(r, "username")?,
                    code,
                    status,
                    expires: r.try_get("endtime")?,
                    used_by: r.try_get("fuid")?,
                    request_key: identity::random_token(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        total,
    ))
}
pub async fn create_invites(
    db: &Database,
    actor: Actor,
    count: i64,
    key: &str,
    why: &str,
) -> Result<()> {
    let why = reason(why)?;
    if !(1..=100).contains(&count) || !valid_key(key) {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_invite_count(),
        ));
    }
    let mut tx = db.begin_write().await?;
    let settings = runtime::locked(db, &mut tx).await?;
    runtime::admin_locked(db, &mut tx, actor).await?;
    if !settings.invite.enabled {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_invites_off(),
        ));
    }
    // The lifetime is the `settings.invite.ttl_days` read by the gate (written as an absolute time when issued);
    // it is not a request parameter and is not part of the digest.
    let ttl = settings.invite.ttl_days.saturating_mul(86_400);
    let hash = identity::token_hash(&format!("{count}:{why}"));
    // The batch's request id lives on the batch table, with the primary key `(uid, request_key)` as the backstop; a
    // different digest means the same request came back with different parameters.
    let previous: Option<String> = db::query_scalar(
        "SELECT request_hash FROM gonof_common_invite_batch WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if let Some(previous) = previous {
        if previous != hash {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_manage_error_invite_replay(),
            ));
        }
        tx.commit().await?;
        return Ok(());
    }
    let now = identity::now();
    let expires = now.checked_add(ttl).ok_or(IdentityError::Validation(
        crate::i18n::current().account_manage_error_invite_ttl(),
    ))?;
    for index in 0..count {
        let itemkey = identity::token_hash(&format!("invite-batch:{key}:{index}"));
        identity::rules::insert_invite(
            db,
            &mut tx,
            actor.uid,
            &identity::random_token(),
            &itemkey,
            now,
            expires,
        )
        .await?;
    }
    db::query("INSERT INTO gonof_common_invite_batch(uid,request_key,request_hash,invites,dateline) VALUES(?,?,?,?,?)")
        .bind(actor.uid)
        .bind(key)
        .bind(&hash)
        .bind(count)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    runtime::audit(
        db,
        &mut tx,
        actor,
        "invite.batch.create",
        serde_json::json!({"count":count,"request_key":key,"request_hash":hash,"reason":why}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn change_invite(
    db: &Database,
    actor: Actor,
    id: i64,
    expected: &str,
    desired: &str,
    key: &str,
) -> Result<()> {
    if id <= 0 || !valid_key(key) {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_invite_key(),
        ));
    }
    let hash = identity::token_hash(
        &serde_json::json!(["admin.invite.status", id, expected, desired]).to_string(),
    );
    use identity::rules::InviteStatus;
    if !matches!(
        InviteStatus::parse(desired),
        Some(InviteStatus::Available | InviteStatus::Frozen | InviteStatus::Revoked)
    ) || !matches!(
        InviteStatus::parse(expected),
        Some(InviteStatus::Available | InviteStatus::Frozen)
    ) {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_invite_action(),
        ));
    }
    let owner: Option<i64> = db::query_scalar("SELECT uid FROM gonof_common_invite WHERE id=?")
        .bind(id)
        .fetch_optional(db.executor())
        .await?;
    let owner = owner.ok_or(IdentityError::NotFound)?;
    let mut tx = db.begin_write().await?;
    for uid in std::collections::BTreeSet::from([actor.uid, owner]) {
        db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    }
    runtime::admin_locked(db, &mut tx, actor).await?;
    if let Some(previous) = db::query_scalar::<String>(
        "SELECT request_hash FROM gonof_home_interaction_request WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        if previous != hash {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_manage_error_key_used(),
            ));
        }
        tx.commit().await?;
        return Ok(());
    }
    let row =
        db::query("SELECT uid,status,endtime,fuid FROM gonof_common_invite WHERE id=? FOR UPDATE")
            .bind(id)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    let current = db::text(&row, "status")?;
    if row.try_get::<i64, _>("uid")? != owner
        || current != expected
        || row.try_get::<Option<i64>, _>("fuid")?.is_some()
        || row.try_get::<i64, _>("endtime")? <= identity::now()
    {
        return Err(IdentityError::Conflict(
            crate::i18n::current().account_manage_error_invite_changed(),
        ));
    }
    crate::identity::rules::insert_interaction_request(
        db,
        &mut tx,
        actor.uid,
        key,
        &hash,
        identity::now(),
    )
    .await?;
    // Only change codes that are unused and still in the state read under the lock: the conditional update guards
    // once more, so a used code (one with a user) is never turned back into an available one.
    let changed = db::query(
        "UPDATE gonof_common_invite SET status=? WHERE id=? AND status=? AND fuid IS NULL",
    )
    .bind(desired)
    .bind(id)
    .bind(expected)
    .execute(db.observe(&mut *tx))
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(IdentityError::Conflict(
            crate::i18n::current().account_manage_error_invite_changed(),
        ));
    }
    runtime::audit(
        db,
        &mut tx,
        actor,
        "invite.status.change",
        serde_json::json!({"invite_id":id,"target_uid":owner,"before":current,"result":desired}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub struct Task {
    pub id: i64,
    pub kind: String,
    pub target: String,
    pub status: String,
    pub attempts: i64,
    pub available_at: i64,
    pub lease_until: i64,
    pub error: String,
    /// Terminated after exhausting retries for temporary failures (see `outbox`); a retry can be scheduled from
    /// the admin area. Shares `failed` with terminations where "the eligibility no longer holds".
    pub exhausted: bool,
}
impl Task {
    pub fn status_label(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.status.as_str() {
            "processing" => t.maintenance_tasks_status_processing(),
            "sent" => t.maintenance_tasks_status_sent(),
            "failed" if self.exhausted => t.maintenance_tasks_status_exhausted(),
            "failed" => t.maintenance_tasks_status_failed(),
            _ => t.maintenance_tasks_status_pending(),
        }
    }
    pub fn available(&self) -> String {
        local_time(self.available_at)
    }
    pub fn lease(&self) -> String {
        local_time(self.lease_until)
    }
    pub fn kind_label(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.kind.as_str() {
            "media.process" => t.maintenance_tasks_kind_media_process(),
            "media.purge" => t.maintenance_tasks_kind_media_purge(),
            "notification.content" => t.maintenance_tasks_kind_notification(),
            kind if crate::mail::EVENT_TYPES.contains(&kind) => t.maintenance_tasks_kind_email(),
            kind if kind.starts_with("privacy.") => t.maintenance_tasks_kind_privacy(),
            _ => t.maintenance_tasks_kind_other(),
        }
    }
    pub fn can_retry(&self) -> bool {
        self.status == "pending"
            || (self.status == "processing" && self.lease_until <= identity::now())
            || (self.status == "failed" && self.exhausted)
    }
}
pub async fn tasks(db: &Database, actor: Actor, q: &ListQuery) -> Result<(Vec<Task>, i64)> {
    admin(db, actor).await?;
    q.validate()?;
    if !matches!(
        q.status.as_str(),
        "" | "pending" | "processing" | "sent" | "failed"
    ) {
        return Err(IdentityError::Validation(
            crate::i18n::current().maintenance_tasks_error_status(),
        ));
    }
    let mut filter = String::from("FROM gonof_common_outbox WHERE event_type LIKE ? ESCAPE '!'");
    let pattern = format!(
        "{}%",
        q.q.replace('!', "!!").replace('%', "!%").replace('_', "!_")
    );
    if !q.status.is_empty() {
        filter.push_str(&format!(" AND status='{}'", q.status));
    }
    if let Some(id) = q.uid {
        filter.push_str(&format!(" AND eventid={id}"));
    }
    let total: i64 = db::query_scalar(&format!("SELECT COUNT(*) {filter}"))
        .bind(&pattern)
        .fetch_one(db.executor())
        .await?;
    paging(q.page(), total)?;
    let rows=db::query(&format!("SELECT eventid,event_type,aggregate_type,aggregate_id,status,attempts,available_at,lease_until,last_error {filter} ORDER BY eventid DESC LIMIT ? OFFSET ?"))
        .bind(pattern).bind(PAGE_SIZE).bind((q.page()-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    let t = crate::i18n::current();
    let items = rows
        .iter()
        .map(|row| {
            let status = db::text(row, "status")?;
            let last_error: Option<String> = row.try_get("last_error")?;
            let has_error = last_error.is_some();
            // Only show a redacted label, never the raw last_error.
            let exhausted =
                status == "failed" && crate::outbox::is_exhausted(last_error.as_deref());
            Ok(Task {
                id: row.try_get("eventid")?,
                kind: db::text(row, "event_type")?,
                target: format!(
                    "{} #{}",
                    match db::text(row, "aggregate_type")?.as_str() {
                        "member" => t.maintenance_tasks_target_member(),
                        "media" | "attachment" => t.maintenance_tasks_target_attachment(),
                        "post" => t.maintenance_tasks_target_post(),
                        "thread" => t.maintenance_tasks_target_thread(),
                        "data_request" => t.maintenance_tasks_target_data_request(),
                        "forum" => t.maintenance_tasks_target_forum(),
                        _ => t.maintenance_tasks_target_other(),
                    },
                    row.try_get::<i64, _>("aggregate_id")?
                ),
                attempts: row.try_get("attempts")?,
                available_at: row.try_get("available_at")?,
                lease_until: row.try_get("lease_until")?,
                error: if has_error {
                    if exhausted {
                        t.maintenance_tasks_error_exhausted()
                    } else if status == "failed" {
                        t.maintenance_tasks_error_gone()
                    } else {
                        t.maintenance_tasks_error_temporary()
                    }
                    .into()
                } else {
                    String::new()
                },
                exhausted,
                status,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((items, total))
}
#[allow(clippy::too_many_arguments)]
pub async fn retry_task(
    db: &Database,
    actor: Actor,
    id: i64,
    attempts: i64,
    available_at: i64,
    lease_until: i64,
    expected: &str,
    why: &str,
) -> Result<()> {
    let why = reason(why)?;
    let mut tx = db.begin_write().await?;
    runtime::admin_locked(db, &mut tx, actor).await?;
    let row=db::query("SELECT status,attempts,available_at,lease_until,event_type,aggregate_type,aggregate_id,last_error,dateline FROM gonof_common_outbox WHERE eventid=? FOR UPDATE").bind(id).fetch_optional(db.observe(&mut *tx)).await?.ok_or(IdentityError::NotFound)?;
    let status = db::text(&row, "status")?;
    if status != expected
        || row.try_get::<i64, _>("attempts")? != attempts
        || row.try_get::<i64, _>("available_at")? != available_at
        || row.try_get::<i64, _>("lease_until")? != lease_until
    {
        return Err(IdentityError::Conflict(
            crate::i18n::current().maintenance_tasks_error_stale(),
        ));
    }
    let exhausted = status == "failed"
        && crate::outbox::is_exhausted(row.try_get::<Option<String>, _>("last_error")?.as_deref());
    if !(status == "pending"
        || (status == "processing" && lease_until <= identity::now())
        || exhausted)
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().maintenance_tasks_error_not_retryable(),
        ));
    }
    // A data request is not due before its own time: an account closure or erasure waits out its cooling period
    // whoever retries it, the same floor as the retry on the data requests page (`privacy::retry`). A private
    // message mail alert likewise waits out the reading window it was queued with: read within it, the message
    // needs no mail.
    let earliest = if db::text(&row, "aggregate_type")? == "data_request" {
        db::query_scalar::<i64>("SELECT execute_after FROM gonof_common_data_request WHERE id=?")
            .bind(row.try_get::<i64, _>("aggregate_id")?)
            .fetch_optional(db.observe(&mut *tx))
            .await?
            .unwrap_or(0)
    } else if db::text(&row, "event_type")? == crate::messages::mail::EVENT_TYPE {
        row.try_get::<i64, _>("dateline")?
            .saturating_add(crate::messages::mail::DELAY_SECONDS)
    } else {
        0
    };
    // Reset the count for tasks that ran out of attempts, otherwise the next failure hits the limit again.
    let statement = if exhausted {
        "UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),attempts=0,lease_token=NULL,lease_until=0 WHERE eventid=?"
    } else {
        "UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0 WHERE eventid=?"
    };
    db::query(statement)
        .bind(identity::now().max(earliest))
        .bind(id)
        .execute(db.observe(&mut *tx))
        .await?;
    runtime::audit(
        db,
        &mut tx,
        actor,
        "task.retry",
        serde_json::json!({"eventid":id,"event_type":db::text(&row,"event_type")?,"reason":why,"exhausted":exhausted}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
