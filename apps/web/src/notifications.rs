//! Content notifications use an Outbox in the same transaction; the body text and the permission to follow the link
//! always read the current content.
use crate::db::Tx;
use anyhow::Context;

use crate::{
    content::{self, writes::ContentWriteError},
    db::{self, Database},
    identity::{self, Actor},
    interaction,
};

type Result<T> = std::result::Result<T, ContentWriteError>;
pub(crate) mod delivery;
pub mod preferences;
pub mod record;
use delivery::deliver;
pub(crate) use delivery::enqueue_content;
const LEASE_SECONDS: i64 = 30;
const BATCH_SIZE: usize = 50;

#[derive(Debug)]
pub struct Notification {
    pub id: i64,
    pub pid: Option<i64>,
    pub tid: Option<i64>,
    pub author: String,
    pub kind: String,
    pub subject: String,
    pub dateline: i64,
    pub read_at: i64,
    pub available: bool,
    pub report_id: Option<i64>,
    pub sanction_id: Option<i64>,
    /// For governance notifications: the stored reason, which for sanctions is completed into the whole sentence in this
    /// request's language when the list is read (`moderation::describe_notices`).
    pub reason: String,
    pub event_key: String,
}

impl Notification {
    pub fn governance(&self) -> bool {
        record::Kind::parse(&self.kind).is_some_and(record::Kind::governance)
    }
    pub fn can_open(&self) -> bool {
        self.available || self.report_id.is_some() || self.sanction_id.is_some()
    }
}

#[derive(Debug)]
pub struct NotificationPage {
    pub items: Vec<Notification>,
    pub total: i64,
    pub unread: i64,
    pub max_id: i64,
}

pub async fn unread_count(db: &Database, actor: Actor) -> Result<i64> {
    let row = crate::db::query(
        "SELECT unread_notifications, credentials_version, `freeze`,account_state FROM gonof_common_member WHERE uid=?",
    )
    .bind(actor.uid)
    .fetch_optional(db.executor())
    .await?
    .ok_or(ContentWriteError::Unauthorized)?;
    if row.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || row.try_get::<i64, _>("freeze")? != 0
        || db::text(&row, "account_state")? != "active"
    {
        return Err(ContentWriteError::Unauthorized);
    }
    Ok(row.try_get("unread_notifications")?)
}

/// Builds the value list for `IN ('a','b')`; all values come from [`record::Kind`] and contain no user input.
fn quoted(types: &[&str]) -> String {
    types
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Builds the `AND n.type IN (...)` fragment for a category.
fn type_filter(category: &str) -> String {
    let types = category_types(category);
    if types.is_empty() {
        return String::new();
    }
    format!(" AND n.type IN ({})", quoted(&types))
}

/// Maps the value of a notification category tab to notification types, grouped by [`record::Kind::category`].
///
/// An empty string means "All" and adds no type condition. Unknown values are treated as all, so a misspelled query
/// string does not empty the list.
pub fn category_types(category: &str) -> Vec<&'static str> {
    record::Kind::ALL
        .into_iter()
        .filter(|kind| kind.category() == category)
        .map(record::Kind::value)
        .collect()
}

fn governance_types() -> Vec<&'static str> {
    record::Kind::ALL
        .into_iter()
        .filter(|kind| kind.governance())
        .map(record::Kind::value)
        .collect()
}

/// Entries per notification page (a constant, not a setting).
pub const PAGE_SIZE: usize = 20;

/// All categories, without forum filtering; pages use [`list_in_category`].
pub async fn list(
    db: &Database,
    actor: Actor,
    page: usize,
    page_size: usize,
) -> Result<NotificationPage> {
    list_in_category(
        db,
        actor,
        page,
        page_size,
        "",
        &crate::interaction::forum_filter::ForumFilters::default(),
    )
    .await
}

/// `filters` is the member's own forum filter: content notifications in blocked forums show a placeholder; governance
/// and security notifications are not affected.
pub async fn list_in_category(
    db: &Database,
    actor: Actor,
    page: usize,
    page_size: usize,
    category: &str,
    filters: &crate::interaction::forum_filter::ForumFilters,
) -> Result<NotificationPage> {
    let unread = unread_count(db, actor).await?;
    if page == 0 || !(1..=100).contains(&page_size) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_notification_page(),
        ));
    }
    let count = crate::db::query(&format!(
        "SELECT COUNT(*) AS total,MAX(n.id) AS max_id FROM gonof_home_notification n WHERE n.uid=?{}",
        type_filter(category)
    ))
    .bind(actor.uid)
    .fetch_one(db.executor())
    .await?;
    let total: i64 = count.try_get("total")?;
    let max_id = count.try_get::<Option<i64>, _>("max_id")?.unwrap_or(0);
    if page > (total as usize).div_ceil(page_size).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    // Forum readability comes from the joined forum table, post visibility from the denormalized copies of this floor
    // and its parent floor; blocking is judged by joining the block table on its primary
    // key (anti-join), without an extra query.
    let visibility = format!(
        "{} AND {}",
        content::thread_visible_by_forum_sql("f", Some(actor.uid)),
        content::post_visible("p")
    );

    let rows = crate::db::query(&format!(
        "SELECT n.id,n.pid,p.tid,n.dateline,n.read_at,n.type,n.report_id,n.sanction_id,n.reason,n.event_key,
            CASE WHEN {visibility} AND (n.type IN ({governance}) OR blocked.uid IS NULL) THEN 1 ELSE 0 END AS available,
            actor.username AS author,actor.account_state AS author_state,t.subject,t.deleted_at,t.deleted_at AS first_deleted_at,t.fid,f.parent_fid
         FROM gonof_home_notification n
         LEFT JOIN gonof_forum_post p ON p.pid=n.pid
         LEFT JOIN gonof_forum_thread t ON t.tid=p.tid
         LEFT JOIN gonof_forum_forum f ON f.fid=t.fid
         LEFT JOIN gonof_common_member actor ON actor.uid=n.authorid
         LEFT JOIN gonof_home_member_block blocked ON blocked.uid=n.uid AND blocked.blocked_uid=n.authorid
         WHERE n.uid=? AND n.id<=?{} ORDER BY n.dateline DESC,n.id DESC LIMIT ? OFFSET ?",
        type_filter(category),
        governance = quoted(&governance_types()),
    ))
    .bind(actor.uid)
    .bind(max_id)
    .bind(page_size as i64)
    .bind(((page - 1) * page_size) as i64)
    .fetch_all(db.executor())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let kind = db::text(&row, "type")?;
            let governance = record::Kind::parse(&kind).is_some_and(record::Kind::governance);
            let forum_blocked = !governance
                && row.try_get::<Option<i64>, _>("fid")?.is_some_and(|fid| {
                    filters.blocks(fid, row.try_get("parent_fid").ok().flatten())
                });
            let available = row.try_get::<i64, _>("available")? != 0 && !forum_blocked;
            // Raw content stays in this private row until visibility is resolved.
            let author = if available || governance {
                if row.try_get::<Option<String>, _>("author_state")?.as_deref() == Some("deleted") {
                    crate::i18n::current().common_deleted_user().into()
                } else {
                    row.try_get::<Option<String>, _>("author")?
                        .unwrap_or_else(|| crate::i18n::current().my_notification_staff().into())
                }
            } else {
                String::new()
            };
            let subject = if available {
                if row.try_get::<i64, _>("deleted_at")? > 0
                    || row.try_get::<i64, _>("first_deleted_at")? > 0
                {
                    crate::i18n::current().common_withdrawn_title().into()
                } else {
                    db::text(&row, "subject")?
                }
            } else {
                String::new()
            };
            Ok(Notification {
                id: row.try_get("id")?,
                pid: if available {
                    Some(row.try_get("pid")?)
                } else {
                    None
                },
                tid: if available {
                    Some(row.try_get("tid")?)
                } else {
                    None
                },
                author,
                kind,
                subject,
                dateline: row.try_get("dateline")?,
                read_at: row.try_get("read_at")?,
                available,
                report_id: row.try_get("report_id")?,
                sanction_id: row.try_get("sanction_id")?,
                reason: db::text(&row, "reason")?,
                event_key: db::text(&row, "event_key")?,
            })
        })
        .collect::<anyhow::Result<Vec<Notification>>>()?;
    let mut items = items;
    crate::moderation::describe_notices(db, actor.uid, &mut items).await?;
    Ok(NotificationPage {
        items,
        total,
        unread,
        max_id,
    })
}

fn valid_read(id: i64, now: i64) -> Result<()> {
    if id <= 0 || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_notification_id(),
        ));
    }
    Ok(())
}

async fn owned_pid(db: &Database, tx: &mut Tx<'_>, uid: i64, id: i64) -> Result<Option<i64>> {
    crate::db::query_scalar("SELECT pid FROM gonof_home_notification WHERE uid=? AND id=?")
        .bind(uid)
        .bind(id)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)
}

async fn set_read(db: &Database, tx: &mut Tx<'_>, uid: i64, id: i64, now: i64) -> Result<()> {
    // `now > 0` (`valid_read`), so the written value is always greater than 0: the matched rows are exactly the rows that
    // really changed from unread to read.
    let result = crate::db::query("UPDATE gonof_home_notification SET read_at=CASE WHEN dateline>? THEN dateline ELSE ? END WHERE uid=? AND id=? AND read_at=0")
        .bind(now).bind(now).bind(uid).bind(id).execute(db.observe(&mut **tx)).await?;
    // Every caller has already locked the member's own row with `lock_actor`.
    let changed = i64::try_from(result.rows_affected()).map_err(anyhow::Error::from)?;
    record::adjust_unread(db, tx, uid, -changed).await?;
    Ok(())
}

pub async fn mark_read(db: &Database, actor: Actor, id: i64, now: i64) -> Result<()> {
    valid_read(id, now)?;
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, true).await?;
    owned_pid(db, &mut tx, actor.uid, id).await?;
    set_read(db, &mut tx, actor.uid, id, now).await?;
    tx.commit().await?;
    Ok(())
}

/// Marks the category of notifications seen at render time as read.
///
/// `max_id` is computed by the list page for the same category, so marking read is limited to that category too; an
/// empty string means all. Otherwise clicking "Mark all as read" under one tab would also mark notifications not yet
/// seen in other categories.
pub async fn mark_all_read(
    db: &Database,
    actor: Actor,
    max_id: i64,
    category: &str,
    now: i64,
) -> Result<()> {
    if max_id < 0 || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_notification_bound(),
        ));
    }
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, true).await?;
    // As in `set_read`: `now > 0`, so the number of matched rows is the number that changed from unread to read.
    let result = crate::db::query(&format!(
        // The alias is written with AS and the SET columns carry no table name: both backends accept that (SQLite
        // rejects an alias without AS and SET columns qualified with the table name).
        "UPDATE gonof_home_notification AS n SET read_at=CASE WHEN n.dateline>? THEN n.dateline ELSE ? END WHERE n.uid=? AND n.id<=? AND n.read_at=0{}",
        type_filter(category)
    ))
    .bind(now)
    .bind(now)
    .bind(actor.uid)
    .bind(max_id)
    .execute(db.observe(&mut *tx))
    .await?;
    record::adjust_unread(
        db,
        &mut tx,
        actor.uid,
        -i64::try_from(result.rows_affected()).map_err(anyhow::Error::from)?,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Marks as read after verifying current access; the caller builds the actual permanent floor URL from (tid,pid).
pub async fn open(db: &Database, actor: Actor, id: i64, now: i64) -> Result<(i64, i64)> {
    valid_read(id, now)?;
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, true).await?;
    let pid = owned_pid(db, &mut tx, actor.uid, id)
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let tid = open_content(db, &mut tx, actor.uid, id, pid, now).await?;
    tx.commit().await?;
    Ok((tid, pid))
}

/// The caller has already locked the authenticated member and verified ownership of the notification by uid/id.
async fn open_content(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    id: i64,
    pid: i64,
    now: i64,
) -> Result<i64> {
    let target = interaction::social::lock_target_for(db, tx, pid, uid).await?;
    if !target.visible {
        return Err(ContentWriteError::NotFound);
    }
    set_read(db, tx, uid, id, now).await?;
    Ok(target.tid)
}

/// Governance destinations are private records owned by this recipient, independent of content visibility.
pub async fn open_url(db: &Database, actor: Actor, id: i64, now: i64) -> Result<String> {
    // No page frame to borrow from: the site default reading preferences and floor pagination are read from the
    // database directly (a member who never chose a reply view follows the site default).
    let settings = crate::runtime::read(db).await.map_err(anyhow::Error::new)?;
    let flat = crate::reading::for_viewer(db, Some(actor.uid), &settings.reading)
        .await?
        .flat;
    open_url_in_view(
        db,
        actor,
        id,
        now,
        (flat, crate::urls::Paging::from_settings(&settings)),
    )
    .await
}

/// Same as [`open_url`], with the reply view and floor pagination supplied by the caller: the page frame of a web
/// request has already read the reading preferences and settings, so they are not read again.
pub async fn open_url_in_view(
    db: &Database,
    actor: Actor,
    id: i64,
    now: i64,
    (flat, paging): (bool, crate::urls::Paging),
) -> Result<String> {
    valid_read(id, now)?;
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, true).await?;
    let row = crate::db::query(
        "SELECT pid,report_id,sanction_id FROM gonof_home_notification WHERE uid=? AND id=?",
    )
    .bind(actor.uid)
    .bind(id)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(ContentWriteError::NotFound)?;
    let report: Option<i64> = row.try_get("report_id")?;
    let sanction: Option<i64> = row.try_get("sanction_id")?;
    let location = report
        .map(|id| format!("/my/reports/{id}"))
        .or_else(|| sanction.map(|_| "/my/sanctions".into()));
    if let Some(location) = location {
        set_read(db, &mut tx, actor.uid, id, now).await?;
        tx.commit().await?;
        return Ok(location);
    }
    let pid = row
        .try_get::<Option<i64>, _>("pid")?
        .ok_or(ContentWriteError::NotFound)?;
    let tid = open_content(db, &mut tx, actor.uid, id, pid, now).await?;
    tx.commit().await?;
    // The target floor is built from the member's own default reply view (reading preferences). Readable
    // forums read the forum table once.
    let forums = content::readable_forums(db, Some(actor.uid)).await?;
    let readable = content::Readable {
        uid: Some(actor.uid),
        forums: &forums,
    };
    content::post_location_for_viewer(db, tid as u64, pid, flat, readable, paging)
        .await?
        .ok_or(ContentWriteError::NotFound)
}

#[derive(Default, Debug)]
pub struct DeliveryReport {
    pub claimed: usize,
    pub delivered: usize,
    pub skipped: usize,
    pub failed: usize,
}

struct Claim {
    eventid: i64,
    pid: i64,
    event_key: String,
    event_type: String,
    payload: String,
    dateline: i64,
    attempts: i64,
    lease: String,
    started: std::time::Instant,
}

async fn claim(db: &Database, now: i64) -> anyhow::Result<Option<Claim>> {
    let started = std::time::Instant::now();
    let mut tx = db.begin_write().await?;
    let row = crate::db::query("SELECT eventid,aggregate_id,event_key,event_type,payload,dateline,attempts FROM gonof_common_outbox WHERE event_type='notification.content' AND status IN ('pending','processing') AND available_at<=? AND lease_until<=? ORDER BY eventid LIMIT 1 FOR UPDATE")
        .bind(now).bind(now).fetch_optional(db.observe(&mut *tx)).await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let claim = Claim {
        eventid: row.try_get("eventid")?,
        pid: row.try_get("aggregate_id")?,
        event_key: db::text(&row, "event_key")?,
        event_type: db::text(&row, "event_type")?,
        payload: db::text(&row, "payload")?,
        dateline: row.try_get("dateline")?,
        attempts: row.try_get::<i64, _>("attempts")?.saturating_add(1),
        lease: identity::random_token(),
        started,
    };
    crate::db::query("UPDATE gonof_common_outbox SET status='processing',attempts=attempts+1,lease_token=?,lease_until=? WHERE eventid=?")
        .bind(&claim.lease).bind(now.saturating_add(LEASE_SECONDS)).bind(claim.eventid).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(Some(claim))
}

async fn current_claim(
    db: &Database,
    tx: &mut Tx<'_>,
    claim: &Claim,
    now: i64,
) -> anyhow::Result<bool> {
    let row = crate::db::query("SELECT lease_token,lease_until FROM gonof_common_outbox WHERE eventid=? AND status='processing' FOR UPDATE")
        .bind(claim.eventid).fetch_optional(db.observe(&mut **tx)).await?;
    let checked_at =
        now.saturating_add(claim.started.elapsed().as_secs().min(i64::MAX as u64) as i64);
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(
        row.try_get::<Option<String>, _>("lease_token")?.as_deref() == Some(&claim.lease)
            && row.try_get::<i64, _>("lease_until")? > checked_at,
    )
}

async fn fail_claim(db: &Database, claim: &Claim, now: i64, reason: &str) -> anyhow::Result<()> {
    let mut tx = db.begin_write().await?;
    if !current_claim(db, &mut tx, claim, now).await? {
        tx.commit().await?;
        return Ok(());
    }
    crate::db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?")
        .bind(reason).bind(claim.eventid).bind(&claim.lease).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn deliver_pending(db: &Database, now: i64) -> anyhow::Result<DeliveryReport> {
    let mut report = DeliveryReport::default();
    for _ in 0..BATCH_SIZE {
        let Some(claim) = claim(db, now).await? else {
            break;
        };
        report.claimed += 1;
        match deliver(db, &claim, now).await {
            Ok(Some(inserted)) => report.delivered += inserted,
            Ok(None) => report.skipped += 1,
            Err(error) if claim.attempts >= crate::outbox::MAX_ATTEMPTS => {
                // Continuing to the next page resets attempts to zero, so this counts consecutive failures since the
                // last progress. Once exhausted, automatic retry stops; after fixing the fault an administrator can
                // schedule a retry in the admin area, and pages already written are not delivered twice.
                let description =
                    crate::outbox::exhausted_error(claim.attempts, &format!("{error:#}"));
                tracing::warn!(
                    eventid = claim.eventid,
                    attempts = claim.attempts,
                    error = %description,
                    "content notification retries exhausted, automatic retry stopped"
                );
                crate::db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?")
                    .bind(description).bind(claim.eventid).bind(&claim.lease).execute(db.executor()).await?;
                report.failed += 1;
            }
            Err(error) => {
                let delay = 5_i64
                    .saturating_mul(1_i64 << claim.attempts.min(9))
                    .min(3600);
                // Record the real cause: a fixed message would hide persistent faults (bad data, constraint
                // conflicts, database unavailable) in both the logs and the admin area. The error comes from the
                // database and this module's fixed messages, and contains no body text or e-mail addresses.
                let description: String = format!("{error:#}").chars().take(1000).collect();
                tracing::warn!(
                    eventid = claim.eventid,
                    attempts = claim.attempts,
                    retry_in = delay,
                    error = %description,
                    "content notification delivery failed, will retry"
                );
                crate::db::query("UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND status='processing' AND lease_token=?")
                    .bind(now.saturating_add(delay)).bind(description).bind(claim.eventid).bind(&claim.lease).execute(db.executor()).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn pending() -> (crate::test_support::TestFixture, Claim) {
        let services = enqueued().await;
        let claimed = claim(&services.db, 100).await.unwrap().unwrap();
        (services, claimed)
    }

    async fn enqueued() -> crate::test_support::TestFixture {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        let row=crate::db::query("SELECT p.pid,p.authorid FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.first=0 AND p.root_pid IS NULL AND p.authorid<>t.authorid LIMIT 1")
            .fetch_one(db.executor()).await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        enqueue_content(
            &db,
            &mut tx,
            row.try_get("pid").unwrap(),
            row.try_get("authorid").unwrap(),
            100,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        services
    }

    // Consecutive failures up to the limit stop automatic retry, and `last_error` states that the attempts are
    // exhausted.
    #[tokio::test]
    async fn notification_retries_stop_at_the_shared_limit() {
        let services = enqueued().await;
        let db = services.db.clone();
        crate::db::query(
            "UPDATE gonof_common_outbox SET attempts=? WHERE event_type='notification.content'",
        )
        .bind(crate::outbox::MAX_ATTEMPTS - 1)
        .execute(db.executor())
        .await
        .unwrap();
        crate::test_support::fail_when(
            &db,
            "notification_limit_contract",
            "BEFORE INSERT",
            "gonof_home_notification",
            "",
            "notification limit contract",
        )
        .await
        .unwrap();
        let report = deliver_pending(&db, 100).await.unwrap();
        assert_eq!(report.failed, 1);
        let row = crate::db::query(
            "SELECT status,last_error FROM gonof_common_outbox WHERE event_type='notification.content'",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(db::text(&row, "status").unwrap(), "failed");
        let error = db::text(&row, "last_error").unwrap();
        assert!(crate::outbox::is_exhausted(Some(&error)), "{error}");
        assert!(error.contains("notification limit contract"));
        services.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn replaced_notification_lease_cannot_insert_or_ack() {
        let (services, claim) = pending().await;
        let db = services.db.clone();
        crate::db::query("UPDATE gonof_common_outbox SET lease_token='new-worker' WHERE eventid=?")
            .bind(claim.eventid)
            .execute(db.executor())
            .await
            .unwrap();
        assert_eq!(deliver(&db, &claim, 100).await.unwrap(), None);
        let rows: i64 = crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification")
            .fetch_one(db.executor())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let lease: String =
            crate::db::query_scalar("SELECT lease_token FROM gonof_common_outbox WHERE eventid=?")
                .bind(claim.eventid)
                .fetch_one(db.executor())
                .await
                .unwrap();
        assert_eq!(lease, "new-worker");
        services.cleanup().await.unwrap();
    }

    #[tokio::test]
    async fn expired_notification_lease_cannot_insert_or_ack() {
        let (services, claim) = pending().await;
        let db = services.db.clone();
        assert_eq!(deliver(&db, &claim, 130).await.unwrap(), None);
        let rows: i64 = crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification")
            .fetch_one(db.executor())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let status: String =
            crate::db::query_scalar("SELECT status FROM gonof_common_outbox WHERE eventid=?")
                .bind(claim.eventid)
                .fetch_one(db.executor())
                .await
                .unwrap();
        assert_eq!(status, "processing");
        let recovered = super::claim(&db, 130).await.unwrap().unwrap();
        assert_eq!(deliver(&db, &recovered, 130).await.unwrap(), Some(1));
        services.cleanup().await.unwrap();
    }
}
