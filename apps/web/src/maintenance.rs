//! Deletes background records that are no longer needed, by retention period.
//!
//! Each round deletes one bounded batch of each kind, at most [`BATCH`] rows:
//!
//! - Outbox: deletes only content notification and mail events in `sent` status that are past the retention period.
//!   `media.*` and `privacy.*` are never deleted: media quotas and cleanup look up events by `media.process:{aid}`,
//!   and data requests also advance through event records. The Outbox has no update time column, so it judges by
//!   `available_at`: the time the event was last queued, which a successful send does not change.
//!   Because the media and data request events stay for good, the sent events are walked with a cursor kept from
//!   round to round (`OutboxCursor`), in the order of the `gonof_outbox_delivery` index: a round reads on until it has
//!   [`BATCH`] candidates or has read [`SCAN`] events, and the next round carries on after the last one read. A new
//!   pass starts at the end of the range and whenever the process starts; an event the pass has already gone by
//!   (one whose local file could not be removed, or one sent long after it was queued) is deleted in the next pass.
//!   For the kinds of events deleted, no later operation inserts the same event key again (checked kind by kind), so
//!   this never resends the same mail or the same notification.
//! - `mail/prepared/` drafts: deletes only `.eml` files whose event is already `sent` or `failed` (or whose event row
//!   was deleted above) and that were written longer ago than the retention period. Drafts of events still retrying
//!   are kept, because a retry must send a byte-for-byte identical mail. The directory is walked with a cursor kept
//!   from round to round (`DirectoryCursor`): a round reads on until it has [`BATCH`] candidates or has read
//!   [`SCAN`] entries, and the next round carries on where it stopped, so drafts that must stay never hold back the
//!   ones after them. A new pass starts at the end of the directory and whenever the process starts.
//! - Temporary mail files: a draft or a locally delivered mail is first written to `.{eventid}.{lease}.tmp` in its
//!   folder and then linked into place (`mail.rs`, `persist`). A process that stops in between leaves the temporary
//!   file behind, with the whole message in it and no event that names it. The walks over `mail/` and
//!   `mail/prepared/` remove any such file older than [`TEMPORARY_MAIL_AGE`], without looking at the database: the
//!   write it belonged to ended long ago, and its event may be gone.
//! - Request records (`gonof_forum_post_request`, `gonof_home_interaction_request`, both with an indexed
//!   `dateline`): deleted when past the retention period. Submitting again later with the same request key is
//!   treated as a new request.
//!   Rows with the column default `dateline` 0 are deleted by the first cleanup.
//! - SMS verification codes (`gonof_common_sms_code`, indexed `dateline`): deleted [`SMS_CODE_RETENTION`] after they
//!   were sent. Local delivery wrote each code with its number in plain text to `sms/{id}.txt` ([`crate::sms`]); that
//!   file goes first, and a row whose file cannot be removed stays for the next round. The `sms/` folder is also
//!   walked like the drafts and any code file older than the same period is removed whatever became of its row: files
//!   left by earlier versions, which deleted only the rows, and files of codes sent to a number while its account was
//!   being deleted (account deletion removes the files of the codes it finds, `privacy/worker.rs`).
//! - Site notifications (`gonof_home_notification`): a read notification is deleted
//!   `notification_read_retention_days` after it was read (`read_at`), an unread one
//!   `notification_unread_retention_days` after it arrived (`dateline`). The table's indexes all start with the
//!   member, so the walk goes member by member, [`MEMBER_SCAN`] members a round in uid order, with a cursor kept from
//!   round to round (`NotificationCursor`): each round looks the expired rows of those members up through
//!   `gonof_notification_unread (uid, read_at, dateline, id)`, at most [`BATCH`] of each kind. A round that finds a
//!   full batch looks at the same members again next time, so a member with many expired rows is emptied before the
//!   walk moves on. Unread ones are deleted member by member under the member row lock, the lock that marking read
//!   and new notifications take, and the member's unread count goes down by the rows deleted.
//!
//! The steps do not depend on each other: one that fails (an unreadable folder, a database error) is logged and
//! listed in [`MaintenanceReport::failed`], and the others still run, so a broken mail folder does not keep expired
//! SMS codes or request records past their retention period.
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::db::{self, Database};

/// At most this many rows are deleted per kind per round.
pub const BATCH: usize = 500;

/// How long SMS code rows and their local files are kept (seconds): the per-number daily count only looks at the
/// current day, so rows from two days ago no longer take part in any limit.
pub const SMS_CODE_RETENTION: i64 = 2 * 86_400;

/// How old a temporary mail file must be before it is removed (seconds). Writing one takes a moment and the mail
/// worker's lease lasts a minute, so a file this old belongs to a write that stopped.
pub const TEMPORARY_MAIL_AGE: i64 = 3600;

/// Outbox event types that can be deleted by retention period: content notifications and every kind of mail
/// ([`crate::mail::EVENT_TYPES`]).
fn deletable_events() -> impl Iterator<Item = &'static str> {
    std::iter::once("notification.content").chain(crate::mail::EVENT_TYPES)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub outbox: u64,
    pub prepared_mail: usize,
    pub post_requests: u64,
    pub interaction_requests: u64,
    pub sms_codes: u64,
    /// Site notifications past their retention period (read and unread together).
    pub notifications: u64,
    /// Local SMS files removed by the walk over `sms/` (those removed with their rows are not counted).
    pub sms_files: usize,
    /// Temporary mail files left by an interrupted write, removed from `mail/` and `mail/prepared/`.
    pub temporary_mail: usize,
    /// The steps that failed this round; each was logged, and the other steps ran anyway.
    pub failed: Vec<&'static str>,
}

/// At most this many entries of one directory, or sent Outbox events, are read per round.
pub const SCAN: usize = 2_000;

/// At most this many members have their notifications looked at per round.
pub const MEMBER_SCAN: usize = 1_000;

/// Cleanup state kept from round to round: the local private directory and how far the walks over the sent Outbox
/// events, the drafts and the SMS files have got. The worker keeps one and runs one round at a time.
pub struct Maintenance {
    /// `identity.local_directory`: delivered local mail in `mail/`, drafts in `mail/prepared/`, SMS in `sms/`.
    directory: PathBuf,
    outbox: OutboxCursor,
    notifications: NotificationCursor,
    mail: DirectoryCursor,
    prepared: DirectoryCursor,
    sms: DirectoryCursor,
}

impl Maintenance {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            outbox: OutboxCursor::default(),
            notifications: NotificationCursor::default(),
            mail: DirectoryCursor::default(),
            prepared: DirectoryCursor::default(),
            sms: DirectoryCursor::default(),
        }
    }

    /// One round. The retention periods are the admin settings `settings.maintenance.*` ("Tools & maintenance ›
    /// Maintenance", docs/settings-and-caching.md, "Keys and groups"); the worker takes them from the in-process cache
    /// once per round and passes them in.
    ///
    /// Every step runs even when an earlier one fails; the report lists the failed steps.
    pub async fn run(
        &mut self,
        database: &Database,
        config: &crate::runtime::MaintenanceSettings,
        now: i64,
    ) -> MaintenanceReport {
        let mut report = MaintenanceReport::default();
        let failed = &mut report.failed;
        let mail_directory = self.directory.join("mail");
        let temporary_cutoff = now.saturating_sub(TEMPORARY_MAIL_AGE);
        report.outbox = settle(
            failed,
            "outbox",
            purge_outbox(
                database,
                &mail_directory,
                &mut self.outbox,
                now.saturating_sub(config.outbox_retention_days.saturating_mul(86_400)),
            )
            .await,
        );
        let (prepared_mail, prepared_temporary) = settle(
            failed,
            "mail drafts",
            purge_prepared_mail(
                database,
                &mail_directory.join("prepared"),
                &mut self.prepared,
                now.saturating_sub(config.prepared_mail_retention_days.saturating_mul(86_400)),
                temporary_cutoff,
            )
            .await,
        );
        report.prepared_mail = prepared_mail;
        let delivered_temporary = settle(
            failed,
            "temporary mail files",
            sweep_temporary_mail(&mail_directory, &mut self.mail, temporary_cutoff).await,
        );
        report.temporary_mail = prepared_temporary + delivered_temporary;
        let cutoff = now.saturating_sub(config.request_retention_days.saturating_mul(86_400));
        report.post_requests = settle(
            failed,
            "post requests",
            purge_requests(database, "gonof_forum_post_request", cutoff).await,
        );
        report.interaction_requests = settle(
            failed,
            "interaction requests",
            purge_requests(database, "gonof_home_interaction_request", cutoff).await,
        );
        let days = |days: i64| now.saturating_sub(days.saturating_mul(86_400));
        report.notifications = settle(
            failed,
            "notifications",
            purge_notifications(
                database,
                &mut self.notifications,
                days(config.notification_read_retention_days),
                days(config.notification_unread_retention_days),
                MEMBER_SCAN,
            )
            .await,
        );
        let sms_directory = crate::sms::local_folder(&self.directory);
        let sms_cutoff = now.saturating_sub(SMS_CODE_RETENTION);
        report.sms_codes = settle(
            failed,
            "SMS codes",
            purge_sms_codes(database, &sms_directory, sms_cutoff).await,
        );
        report.sms_files = settle(
            failed,
            "SMS files",
            sweep_sms_files(&sms_directory, &mut self.sms, sms_cutoff).await,
        );
        report
    }
}

/// The count of a step that worked; a failed step is logged, added to `failed` and counts nothing.
fn settle<T: Default>(failed: &mut Vec<&'static str>, step: &'static str, result: Result<T>) -> T {
    result.unwrap_or_else(|error| {
        tracing::warn!(
            step,
            error = %error,
            "a retention cleanup step failed; the other steps still run"
        );
        failed.push(step);
        T::default()
    })
}

/// How far the walk over the sent Outbox events has got: the `(available_at, lease_until, eventid)` of the last event
/// read, in the order of `gonof_outbox_delivery`; `None` starts a pass from the oldest. A sent event has `lease_until`
/// 0, so this is in effect `(available_at, eventid)`; the column is kept so the walk follows the index exactly.
#[derive(Default)]
struct OutboxCursor {
    after: Option<(i64, i64, i64)>,
}

/// How far the walk over the members' notifications has got: the last uid looked at; 0 starts a pass.
#[derive(Default)]
struct NotificationCursor {
    after: i64,
}

/// A walk over one directory that carries on from round to round: the open directory stream is kept, so each round
/// reads only a bounded number of entries and the walk still reaches every file. Entries created or removed during a
/// pass may or may not show up in it (POSIX `readdir`); those not seen are seen in the next pass.
#[derive(Default)]
struct DirectoryCursor {
    open: Option<tokio::fs::ReadDir>,
}

impl DirectoryCursor {
    /// The next entry of the current pass over `directory`, starting a pass when none is under way. `None` ends the
    /// pass (a directory that does not exist is an empty pass); the call after that starts again from the top. An error
    /// also ends the pass.
    async fn next(&mut self, directory: &Path) -> std::io::Result<Option<tokio::fs::DirEntry>> {
        if self.open.is_none() {
            match tokio::fs::read_dir(directory).await {
                Ok(entries) => self.open = Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        let entries = self.open.as_mut().expect("opened above");
        let next = entries.next_entry().await;
        if !matches!(next, Ok(Some(_))) {
            self.open = None;
        }
        next
    }
}

/// Takes a batch of expired verification code rows via the `(dateline)` index, then deletes by primary key, checking
/// the time condition once more. The local file of each code goes first; a row whose file cannot be removed stays
/// for the next round.
async fn purge_sms_codes(database: &Database, directory: &Path, cutoff: i64) -> Result<u64> {
    let candidates: Vec<i64> = db::query_scalar(&format!(
        "SELECT id FROM gonof_common_sms_code WHERE dateline<? ORDER BY dateline LIMIT {BATCH}"
    ))
    .bind(cutoff)
    .fetch_all(database.executor())
    .await?;
    let mut ids = Vec::with_capacity(candidates.len());
    for id in candidates {
        match remove_if_present(&crate::sms::local_file(directory, id)).await {
            Ok(()) => ids.push(id),
            Err(error) => tracing::warn!(
                id,
                error = %error,
                "could not remove a local SMS file; keeping its code row"
            ),
        }
    }
    if ids.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM gonof_common_sms_code WHERE id IN ({}) AND dateline<?",
        vec!["?"; ids.len()].join(",")
    );
    let mut delete = db::query(&sql);
    for id in &ids {
        delete = delete.bind(id);
    }
    Ok(delete
        .bind(cutoff)
        .execute(database.executor())
        .await?
        .rows_affected())
}

/// Takes a batch of expired primary keys via the `(dateline)` index, then deletes by primary key, checking the time
/// condition once more.
async fn purge_requests(database: &Database, table: &str, cutoff: i64) -> Result<u64> {
    let rows = db::query(&format!(
        "SELECT uid,request_key FROM {table} WHERE dateline<? LIMIT {BATCH}"
    ))
    .bind(cutoff)
    .fetch_all(database.executor())
    .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM {table} WHERE (uid,request_key) IN ({}) AND dateline<?",
        vec!["(?,?)"; rows.len()].join(",")
    );
    let mut delete = db::query(&sql);
    for row in &rows {
        delete = delete
            .bind(row.try_get::<i64, _>("uid")?)
            .bind(db::text(row, "request_key")?);
    }
    Ok(delete
        .bind(cutoff)
        .execute(database.executor())
        .await?
        .rows_affected())
}

/// Deletes the notifications of the next `scan` members ([`MEMBER_SCAN`]) that are past their retention period: read ones
/// whose `read_at` is before `read_cutoff`, unread ones whose `dateline` is before `unread_cutoff`. The candidates are
/// taken through the `(uid, read_at, dateline, id)` index of the members in the batch and deleted by primary key with
/// the condition checked once more, so a notification marked read in between is not deleted as unread.
async fn purge_notifications(
    database: &Database,
    cursor: &mut NotificationCursor,
    read_cutoff: i64,
    unread_cutoff: i64,
    scan: usize,
) -> Result<u64> {
    let members: Vec<i64> = db::query_scalar(&format!(
        "SELECT uid FROM gonof_common_member WHERE uid>? ORDER BY uid LIMIT {scan}"
    ))
    .bind(cursor.after)
    .fetch_all(database.executor())
    .await?;
    let Some(&last) = members.last() else {
        cursor.after = 0;
        return Ok(0);
    };
    let read =
        expired_notifications(database, &members, "read_at>0 AND read_at<?", read_cutoff).await?;
    let unread = expired_notifications(
        database,
        &members,
        "read_at=0 AND dateline<?",
        unread_cutoff,
    )
    .await?;
    // A full batch may have left more behind: the next round looks at the same members again.
    if read.len() < BATCH && unread.len() < BATCH {
        cursor.after = if members.len() < scan { 0 } else { last };
    }
    let read: Vec<i64> = read.into_iter().map(|(_, id)| id).collect();
    let mut deleted =
        crate::notifications::record::delete_expired_read(database, &read, read_cutoff).await?;
    let mut by_member = std::collections::BTreeMap::<i64, Vec<i64>>::new();
    for (uid, id) in unread {
        by_member.entry(uid).or_default().push(id);
    }
    for (uid, ids) in by_member {
        deleted +=
            crate::notifications::record::delete_expired_unread(database, uid, &ids, unread_cutoff)
                .await?;
    }
    Ok(deleted)
}

/// Up to [`BATCH`] `(uid, id)` of the notifications of `members` matching `condition`, whose one parameter is
/// `cutoff`: one range of `gonof_notification_unread` per member. The index is named: on a small table MySQL would
/// rather read the whole index than seek each member's range.
async fn expired_notifications(
    database: &Database,
    members: &[i64],
    condition: &str,
    cutoff: i64,
) -> Result<Vec<(i64, i64)>> {
    let sql = format!(
        "SELECT uid,id FROM gonof_home_notification FORCE INDEX (gonof_notification_unread) WHERE uid IN ({}) AND {condition} LIMIT {BATCH}",
        vec!["?"; members.len()].join(",")
    );
    let mut query = db::query(&sql);
    for uid in members {
        query = query.bind(*uid);
    }
    query
        .bind(cutoff)
        .fetch_all(database.executor())
        .await?
        .iter()
        .map(|row| Ok((row.try_get("uid")?, row.try_get("id")?)))
        .collect()
}

async fn purge_outbox(
    database: &Database,
    mail_directory: &Path,
    cursor: &mut OutboxCursor,
    cutoff: i64,
) -> Result<u64> {
    // Walks `gonof_outbox_delivery (status, available_at, lease_until, eventid)` in index order from the cursor, so
    // the scan stops after SCAN entries instead of reading every sent media and data request event to find the
    // deletable ones. `event_type` is not in the index and is checked here. The extra `available_at>=?` gives both
    // engines the start of the range; the OR after it only drops the rows of that first `available_at` up to the
    // cursor. Deletes by primary key with the condition checked once more, so rows whose status changed between the
    // two steps are not deleted.
    let (at, lease, last) = cursor.after.unwrap_or((i64::MIN, i64::MIN, i64::MIN));
    let rows = db::query(&format!(
        "SELECT eventid,event_type,available_at,lease_until FROM gonof_common_outbox FORCE INDEX (gonof_outbox_delivery) WHERE status='sent' AND available_at<? AND available_at>=? AND (available_at>? OR lease_until>? OR (lease_until=? AND eventid>?)) ORDER BY available_at,lease_until,eventid LIMIT {SCAN}"
    ))
    .bind(cutoff)
    .bind(at)
    .bind(at)
    .bind(lease)
    .bind(lease)
    .bind(last)
    .fetch_all(database.executor())
    .await?;
    let mut candidates = Vec::new();
    let mut read = 0;
    for row in &rows {
        read += 1;
        let eventid: i64 = row.try_get("eventid")?;
        cursor.after = Some((
            row.try_get("available_at")?,
            row.try_get("lease_until")?,
            eventid,
        ));
        let kind = db::text(row, "event_type")?;
        if deletable_events().any(|deletable| deletable == kind) {
            candidates.push(eventid);
            if candidates.len() == BATCH {
                break;
            }
        }
    }
    if read == rows.len() && rows.len() < SCAN {
        // The end of the range: the next round starts a new pass from the oldest event.
        cursor.after = None;
    }
    // A mail event is the only record of whom its local files are for: account deletion finds `mail/{eventid}.eml`
    // and the draft in `prepared/` through the member's events. So the files go first, and an event whose files
    // cannot be removed stays for the next round.
    let mut ids = Vec::with_capacity(candidates.len());
    for eventid in candidates {
        match remove_mail_files(mail_directory, eventid).await {
            Ok(()) => ids.push(eventid),
            Err(error) => tracing::warn!(
                eventid,
                error = %error,
                "could not remove a local mail file; keeping its event"
            ),
        }
    }
    if ids.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM gonof_common_outbox WHERE eventid IN ({}) AND status='sent' AND available_at<?",
        vec!["?"; ids.len()].join(",")
    );
    let mut delete = db::query(&sql);
    for id in &ids {
        delete = delete.bind(*id);
    }
    Ok(delete
        .bind(cutoff)
        .execute(database.executor())
        .await?
        .rows_affected())
}

/// Removes the delivered local mail and the draft of one event; files that are not there count as removed.
async fn remove_mail_files(directory: &Path, eventid: i64) -> std::io::Result<()> {
    let name = format!("{eventid}.eml");
    for path in [
        directory.join(&name),
        directory.join("prepared").join(&name),
    ] {
        remove_if_present(&path).await?;
    }
    Ok(())
}

/// Removes one file; a file that is not there counts as removed.
async fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Removes local SMS files (`{id}.txt`) last written before `cutoff`, walking the folder from round to round: at most
/// [`BATCH`] removals and [`SCAN`] entries read per round.
async fn sweep_sms_files(
    directory: &Path,
    cursor: &mut DirectoryCursor,
    cutoff: i64,
) -> Result<usize> {
    let cutoff = std::time::UNIX_EPOCH + std::time::Duration::from_secs(cutoff.max(0) as u64);
    let mut removed = 0;
    let mut read = 0;
    while removed < BATCH && read < SCAN {
        let Some(entry) = cursor.next(directory).await? else {
            break;
        };
        read += 1;
        let name = entry.file_name();
        let written_here = name
            .to_str()
            .and_then(|name| name.strip_suffix(".txt"))
            .is_some_and(|id| id.parse::<i64>().is_ok());
        if !written_here {
            continue;
        }
        // Removed with its row or by account deletion since the walk listed it.
        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_file() && metadata.modified()? < cutoff {
            remove_if_present(&entry.path()).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The name `persist` in `mail.rs` gives a file while it is being written: `.{eventid}.{lease}.tmp`.
fn temporary_mail(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
        .and_then(|name| name.split_once('.'))
        .is_some_and(|(eventid, lease)| eventid.parse::<i64>().is_ok() && !lease.is_empty())
}

/// Removes `entry` when it is a temporary mail file last written before `cutoff`; whether it did.
async fn remove_stale_temporary(
    entry: &tokio::fs::DirEntry,
    cutoff: std::time::SystemTime,
) -> Result<bool> {
    if !entry.file_name().to_str().is_some_and(temporary_mail) {
        return Ok(false);
    }
    // The write it belongs to may have finished and removed it since the walk listed it.
    let metadata = match entry.metadata().await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.modified()? >= cutoff {
        return Ok(false);
    }
    remove_if_present(&entry.path()).await?;
    Ok(true)
}

/// Removes temporary files of locally delivered mail in `mail/` written before `cutoff`, walking the folder from
/// round to round: at most [`BATCH`] removals and [`SCAN`] entries read per round. Those in `mail/prepared/` go in the
/// walk over the drafts.
async fn sweep_temporary_mail(
    directory: &Path,
    cursor: &mut DirectoryCursor,
    cutoff: i64,
) -> Result<usize> {
    let cutoff = std::time::UNIX_EPOCH + std::time::Duration::from_secs(cutoff.max(0) as u64);
    let mut removed = 0;
    let mut read = 0;
    while removed < BATCH && read < SCAN {
        let Some(entry) = cursor.next(directory).await? else {
            break;
        };
        read += 1;
        if remove_stale_temporary(&entry, cutoff).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Drafts removed, and temporary files removed on the way (written before `temporary_cutoff`).
async fn purge_prepared_mail(
    database: &Database,
    directory: &Path,
    cursor: &mut DirectoryCursor,
    cutoff: i64,
    temporary_cutoff: i64,
) -> Result<(usize, usize)> {
    let cutoff = std::time::UNIX_EPOCH + std::time::Duration::from_secs(cutoff.max(0) as u64);
    let temporary_cutoff =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(temporary_cutoff.max(0) as u64);
    let mut candidates = Vec::new();
    let mut temporary = 0;
    let mut read = 0;
    while candidates.len() < BATCH && read < SCAN {
        let Some(entry) = cursor.next(directory).await? else {
            break;
        };
        read += 1;
        if remove_stale_temporary(&entry, temporary_cutoff).await? {
            temporary += 1;
            continue;
        }
        let name = entry.file_name();
        let Some(eventid) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".eml"))
            .and_then(|id| id.parse::<i64>().ok())
        else {
            continue;
        };
        // The mail worker removes the draft of a mail that has just finished, so it may be gone by now.
        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_file() && metadata.modified()? < cutoff {
            candidates.push((eventid, entry.path()));
        }
    }
    if candidates.is_empty() {
        return Ok((0, temporary));
    }
    let sql = format!(
        "SELECT eventid,status,last_error FROM gonof_common_outbox WHERE eventid IN ({})",
        vec!["?"; candidates.len()].join(",")
    );
    let mut query = db::query(&sql);
    for (eventid, _) in &candidates {
        query = query.bind(*eventid);
    }
    // A draft stays while its mail may still go out: not finished yet, or failed only because the automatic retries
    // ran out, which an administrator can retry from the task list (`administration::retry_task`). The retry has to
    // send the same mail, not one rendered again from a template changed since.
    let mut pending = std::collections::HashSet::new();
    for row in query.fetch_all(database.executor()).await? {
        let retryable = match db::text(&row, "status")?.as_str() {
            "sent" => false,
            "failed" => crate::outbox::is_exhausted(
                row.try_get::<Option<String>, _>("last_error")?.as_deref(),
            ),
            _ => true,
        };
        if retryable {
            pending.insert(row.try_get::<i64, _>("eventid")?);
        }
    }
    let mut removed = 0;
    for (eventid, path) in candidates {
        if pending.contains(&eventid) {
            continue;
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok((removed, temporary))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SMS verification codes are deleted by the cleanup task after two days, in batches via the `(dateline)` index,
    /// and the files local delivery wrote for them go with them. Code files older than two days are removed even when
    /// their row is already gone; newer ones and files the SMS module did not write stay.
    #[tokio::test]
    async fn sms_codes_expire_after_two_days() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let hash = "a".repeat(64);
        let folder = crate::sms::local_folder(&services.root.join(".local"));
        std::fs::create_dir_all(&folder).unwrap();
        let mut files = Vec::new();
        for (mobile, dateline) in [
            ("+8613800000001", now - SMS_CODE_RETENTION - 1),
            ("+8613800000002", now - SMS_CODE_RETENTION - 3600),
            ("+8613800000003", now - SMS_CODE_RETENTION + 60),
            ("+8613800000004", now),
        ] {
            let id = db::query("INSERT INTO gonof_common_sms_code(mobile,purpose,code_hash,dateline,expires_at) VALUES(?,'login',?,?,?)")
                .bind(mobile)
                .bind(&hash)
                .bind(dateline)
                .bind(dateline + 300)
                .execute(db.executor())
                .await
                .unwrap()
                .last_insert_id() as i64;
            // All written just now: the files of the expired rows go because their rows do, not by their age.
            let path = crate::sms::local_file(&folder, id);
            std::fs::write(&path, format!("Mobile number: {mobile}\n")).unwrap();
            files.push(path);
        }
        let aged = |name: &str, age: i64| {
            let path = folder.join(name);
            std::fs::File::create(&path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(age as u64),
                )
                .unwrap();
            path
        };
        let old = SMS_CODE_RETENTION + 3600;
        // A file whose row an earlier version already deleted, one whose row is gone but which is recent, and a file
        // with a name local delivery does not write.
        let leftover = aged("999999999.txt", old);
        let recent = aged("999999998.txt", 60);
        let foreign = aged("notes.txt", old);
        let config = crate::runtime::read(db).await.unwrap().maintenance;
        let mut maintenance = Maintenance::new(services.root.join(".local"));
        let report = maintenance.run(db, &config, now).await;
        assert_eq!((report.sms_codes, report.sms_files), (2, 1));
        let left: Vec<String> =
            db::query_scalar("SELECT mobile FROM gonof_common_sms_code ORDER BY mobile")
                .fetch_all(db.executor())
                .await
                .unwrap();
        assert_eq!(left, ["+8613800000003", "+8613800000004"]);
        assert!(
            !files[0].exists() && !files[1].exists(),
            "the files of expired codes go with their rows"
        );
        assert!(files[2].exists() && files[3].exists());
        assert!(
            !leftover.exists(),
            "an old code file without a row is swept up"
        );
        assert!(recent.exists() && foreign.exists());
        let again = maintenance.run(db, &config, now).await;
        assert_eq!((again.sms_codes, again.sms_files), (0, 0));
        services.cleanup().await.unwrap();
    }

    async fn enqueue(db: &Database, key: &str, kind: &str, status: &str, at: i64) -> i64 {
        db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES(?,?,'test',1,'{}',?,?,?)")
            .bind(key)
            .bind(kind)
            .bind(at)
            .bind(at)
            .bind(status)
            .execute(db.executor())
            .await
            .unwrap()
            .last_insert_id() as i64
    }

    async fn exists(db: &Database, key: &str) -> bool {
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key=?")
            .bind(key)
            .fetch_one(db.executor())
            .await
            .unwrap()
            == 1
    }

    /// Retention: sent content notification and mail events past the retention period are deleted in batches; those
    /// not yet expired, not yet sent, and media and data request events are all kept. Drafts are deleted only when
    /// their event has finished for good and they are past the retention period: a mail out of automatic retries
    /// keeps its draft for an administrator's retry.
    #[tokio::test]
    async fn retention_purges_only_finished_mail_and_notification_events() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let old = now - 31 * 86_400;
        let recent = now - 29 * 86_400;
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let reset = enqueue(db, "old-reset", "password_reset", "sent", old).await;
        enqueue(db, "old-content", "notification.content", "sent", old).await;
        enqueue(db, "old-digest", "notification.email", "sent", old).await;
        let recent_mail = enqueue(db, "recent-mail", "notification.email", "sent", recent).await;
        enqueue(db, "old-media", "media.process", "sent", old).await;
        enqueue(db, "old-export", "privacy.export", "sent", old).await;
        let waiting = enqueue(db, "old-pending", "notification.email", "pending", old).await;
        let failed = enqueue(db, "old-failed", "password_reset", "failed", old).await;
        // Out of automatic retries: an administrator can still retry it, with the same draft.
        let exhausted = enqueue(db, "old-exhausted", "login_alert", "failed", old).await;
        db::query("UPDATE gonof_common_outbox SET last_error=? WHERE eventid=?")
            .bind(crate::outbox::exhausted_error(20, "connection timed out"))
            .bind(exhausted)
            .execute(db.executor())
            .await
            .unwrap();
        // Drafts: deleted when the event has finished and the retention period has passed; drafts still retrying, not
        // yet expired, and unrecognized files are all kept.
        let mut maintenance = Maintenance::new(services.root.join("maintenance-local"));
        let prepared = services.root.join("maintenance-local/mail/prepared");
        std::fs::create_dir_all(&prepared).unwrap();
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
        let write = |name: String, aged: bool| {
            let path = prepared.join(name);
            let file = std::fs::File::create(&path).unwrap();
            if aged {
                file.set_modified(stale).unwrap();
            }
            path
        };
        let gone = [
            write(format!("{reset}.eml"), true),
            write(format!("{failed}.eml"), true),
            write("999999999.eml".into(), true),
        ];
        let kept = [
            write(format!("{waiting}.eml"), true),
            write(format!("{exhausted}.eml"), true),
            write(format!("{recent_mail}.eml"), false),
            write("notes.txt".into(), true),
        ];
        let report = maintenance.run(db, &config, now).await;
        // The draft of old-reset goes with its event, before the draft cleanup counts the other two.
        assert_eq!(
            report,
            MaintenanceReport {
                outbox: 3,
                prepared_mail: 2,
                ..Default::default()
            }
        );
        for key in ["old-reset", "old-content", "old-digest"] {
            assert!(
                !exists(db, key).await,
                "{key} is past retention and sent, so it is deleted"
            );
        }
        for key in [
            "recent-mail",
            "old-media",
            "old-export",
            "old-pending",
            "old-failed",
            "old-exhausted",
        ] {
            assert!(exists(db, key).await, "{key} must be kept");
        }
        assert!(gone.iter().all(|path| !path.exists()));
        assert!(kept.iter().all(|path| path.exists()));

        // Each round deletes at most BATCH rows; the rest are deleted in the next round.
        for n in 0..(BATCH + 20) {
            enqueue(
                db,
                &format!("bulk-{n}"),
                "notification.content",
                "sent",
                old,
            )
            .await;
        }
        assert_eq!(maintenance.run(db, &config, now).await.outbox, BATCH as u64);
        assert_eq!(maintenance.run(db, &config, now).await.outbox, 20);
        assert_eq!(maintenance.run(db, &config, now).await.outbox, 0);
        services.cleanup().await.unwrap();
    }

    /// A sent mail event past retention takes its delivered local mail and its draft with it. Account deletion finds a
    /// member's mail files through their events, so files whose event is gone could never be found again. When a file
    /// cannot be removed, the event stays and the next round tries again.
    #[cfg(unix)]
    #[tokio::test]
    async fn purged_mail_events_take_their_local_files() {
        use std::os::unix::fs::PermissionsExt;
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let mut maintenance = Maintenance::new(services.root.join("purged-local"));
        let directory = services.root.join("purged-local/mail");
        let prepared = directory.join("prepared");
        std::fs::create_dir_all(&prepared).unwrap();
        let eventid = enqueue(db, "sent-alert", "login_alert", "sent", now - 31 * 86_400).await;
        let delivered = directory.join(format!("{eventid}.eml"));
        let draft = prepared.join(format!("{eventid}.eml"));
        std::fs::write(&delivered, "To: member@example.test").unwrap();
        std::fs::write(&draft, "{}").unwrap();

        // A directory the process cannot write to; root can, so that part only runs for other accounts.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let locked = std::fs::write(directory.join("probe"), "").is_err();
        if locked {
            let report = maintenance.run(db, &config, now).await;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(report.outbox, 0);
            assert!(
                exists(db, "sent-alert").await,
                "the event stays while its file does"
            );
            assert!(delivered.exists());
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(maintenance.run(db, &config, now).await.outbox, 1);
        assert!(!exists(db, "sent-alert").await);
        assert!(
            !delivered.exists(),
            "the delivered mail goes with its event"
        );
        assert!(!draft.exists(), "the draft goes with its event");
        services.cleanup().await.unwrap();
    }

    /// The media and data request events are kept for good, so the Outbox step does not read them all again every
    /// round: it walks the sent events with a cursor, reading at most SCAN per round and carrying on after the last
    /// one read. An event the pass has gone by waits for the next pass, which starts once the walk reaches the end.
    #[tokio::test]
    async fn outbox_cleanup_carries_on_past_the_events_it_keeps() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let old = now - 31 * 86_400;
        let config = crate::runtime::settings::tests::seeded().maintenance;
        // SCAN sent media events, older than the mail queued after them.
        let media = old - 600;
        for chunk in (0..SCAN).collect::<Vec<_>>().chunks(BATCH) {
            let values: Vec<String> = chunk
                .iter()
                .map(|n| {
                    format!("('media.process:{n}','media.process','attachment',{n},'{{}}',{media},{media},'sent')")
                })
                .collect();
            db::query(&format!(
                "INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES {}",
                values.join(",")
            ))
            .execute(db.executor())
            .await
            .unwrap();
        }
        enqueue(db, "after-media", "password_reset", "sent", old).await;
        let mut maintenance = Maintenance::new(services.root.join("cursor-local"));
        let first = maintenance.run(db, &config, now).await;
        assert_eq!(
            (first.outbox, first.failed.len()),
            (0, 0),
            "the first round reads only the media events"
        );
        assert!(exists(db, "after-media").await);

        // Sent long after it was queued, so it sorts before the media events the walk has gone by.
        enqueue(db, "behind", "notification.email", "sent", media - 600).await;
        assert_eq!(maintenance.run(db, &config, now).await.outbox, 1);
        assert!(
            !exists(db, "after-media").await,
            "the second round carries on after the media events"
        );
        assert!(
            exists(db, "behind").await,
            "an event behind the cursor waits for the next pass"
        );

        assert_eq!(maintenance.run(db, &config, now).await.outbox, 1);
        assert!(
            !exists(db, "behind").await,
            "the walk reached the end, so this round started again from the oldest event"
        );
        let media_left: i64 = db::query_scalar(
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='media.process'",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(media_left, SCAN as i64, "media events are never deleted");
        services.cleanup().await.unwrap();
    }

    /// Both request tables are cleaned up by `dateline`: expired rows (including rows with the default `dateline` 0)
    /// are deleted and unexpired ones are kept; within the retention period, replaying the same request key still
    /// returns the original result, and after it the request is treated as new. The candidate query uses the
    /// `(dateline)` index.
    #[tokio::test]
    async fn request_records_expire_after_the_retention_period() {
        use crate::content::writes::{ContentWriter, NewReply, WriteConfig};
        use crate::debug::trace::{self, QuerySource, RequestProfile};
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        crate::content::seed_forums(db).await.unwrap();
        crate::content::seed_fixture(db).await.unwrap();
        let now = crate::identity::now();
        let uid: i64 = db::query_scalar("SELECT MIN(uid) FROM gonof_common_member")
            .fetch_one(db.executor())
            .await
            .unwrap();
        db::query(
            "UPDATE gonof_common_member SET adminid=1,password='maintenance-test-hash' WHERE uid=?",
        )
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
        crate::test_support::raise_posting_quotas(db).await.unwrap();
        let writer = ContentWriter::new(db.clone(), WriteConfig::default());
        let key = crate::identity::random_token();
        let reply = |body: &'static str| NewReply {
            tid: 103,
            root_pid: None,
            reply_to_pid: None,
            body: body.into(),
            attachment_ids: vec![],
            request_key: key.clone(),
        };
        let first = writer
            .reply_at(uid, reply("A reply within the retention period"), now)
            .await
            .unwrap();
        let recorded: i64 = db::query_scalar(
            "SELECT dateline FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
        )
        .bind(uid)
        .bind(&key)
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(recorded, now, "the request record holds the request time");
        // A row with the default dateline 0 and an expired follow request.
        let pid: i64 = db::query_scalar("SELECT MIN(pid) FROM gonof_forum_post")
            .fetch_one(db.executor())
            .await
            .unwrap();
        db::query(
            "INSERT INTO gonof_forum_post_request(uid,request_key,request_hash,pid) VALUES(?,?,?,?)",
        )
        .bind(uid)
        .bind(crate::identity::random_token())
        .bind(crate::identity::random_token())
        .bind(pid)
        .execute(db.executor())
        .await
        .unwrap();
        for (dateline, tag) in [(now - 31 * 86_400, 'a'), (now - 86_400, 'b')] {
            db::query("INSERT INTO gonof_home_interaction_request(uid,request_key,request_hash,dateline) VALUES(?,?,?,?)")
                .bind(uid)
                .bind(tag.to_string().repeat(64))
                .bind(crate::identity::random_token())
                .bind(dateline)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let mut maintenance = Maintenance::new(services.root.join("maintenance-requests"));
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let profile = std::sync::Arc::new(RequestProfile::new());
        let report =
            trace::scope(profile.clone(), Box::pin(maintenance.run(db, &config, now))).await;
        assert_eq!((report.post_requests, report.interaction_requests), (1, 1));
        let left: i64 = db::query_scalar(
            "SELECT COUNT(*) FROM gonof_home_interaction_request WHERE request_key=?",
        )
        .bind("b".repeat(64))
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(left, 1, "the unexpired interaction request is kept");
        let replay = writer
            .reply_at(uid, reply("A reply within the retention period"), now + 60)
            .await
            .unwrap();
        assert!(
            replay.replayed && replay.pid == first.pid,
            "a replay within the retention period returns the original result"
        );
        // The candidate query uses the new index; deletion is by primary key.
        // The execution plan is checked only on MySQL (SQLite's EXPLAIN QUERY PLAN is a different system).
        let explained = db.pool.as_mysql().is_some();
        for (table, index) in [
            ("gonof_forum_post_request", "gonof_post_request_dateline"),
            (
                "gonof_home_interaction_request",
                "gonof_interaction_request_dateline",
            ),
        ] {
            let records: Vec<_> = profile
                .snapshot()
                .into_iter()
                .filter(|r| {
                    explained && r.source == QuerySource::Application && r.sql.contains(table)
                })
                .collect();
            for record in records {
                let plan = sqlx::query(&format!("EXPLAIN {}", record.expanded_sql))
                    .persistent(false)
                    .fetch_all(
                        db.pool
                            .as_mysql()
                            .expect("EXPLAIN is checked only on MySQL"),
                    )
                    .await
                    .unwrap();
                let key = {
                    use sqlx::{Column, Row};
                    let ordinal = plan[0]
                        .columns()
                        .iter()
                        .position(|c| c.name() == "key")
                        .unwrap();
                    plan[0]
                        .try_get::<Option<Vec<u8>>, _>(ordinal)
                        .unwrap()
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_default()
                };
                let expected = if record.sql.starts_with("DELETE") {
                    "PRIMARY"
                } else {
                    index
                };
                assert_eq!(key, expected, "{}", record.sql);
            }
        }
        // Past the retention period: submitting the same request key again is a new request and posts another reply.
        let later = now + 31 * 86_400;
        assert_eq!(maintenance.run(db, &config, later).await.post_requests, 1);
        let fresh = writer
            .reply_at(uid, reply("A new reply after the retention period"), later)
            .await
            .unwrap();
        assert!(!fresh.replayed && fresh.pid != first.pid);
        services.cleanup().await.unwrap();
    }

    /// The walk over a directory reads on from where the last call stopped, ends each pass with `None` and then starts
    /// again from the top; a directory that does not exist is an empty pass.
    #[tokio::test]
    async fn directory_cursor_carries_on_and_starts_over() {
        let root =
            std::env::temp_dir().join(format!("gonof-cursor-{}", crate::identity::random_token()));
        let mut cursor = DirectoryCursor::default();
        assert!(cursor.next(&root).await.unwrap().is_none());
        std::fs::create_dir_all(&root).unwrap();
        for n in 0..5 {
            std::fs::write(root.join(format!("{n}.txt")), "").unwrap();
        }
        async fn pass(cursor: &mut DirectoryCursor, root: &Path) -> Vec<String> {
            let mut names = Vec::new();
            while let Some(entry) = cursor.next(root).await.unwrap() {
                names.push(entry.file_name().into_string().unwrap());
            }
            names.sort();
            names
        }
        let expected: Vec<String> = (0..5).map(|n| format!("{n}.txt")).collect();
        assert_eq!(pass(&mut cursor, &root).await, expected);
        assert_eq!(
            pass(&mut cursor, &root).await,
            expected,
            "the next pass starts from the top"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Drafts that must stay do not hold back the ones after them: with more retained drafts than one batch, the
    /// walk carries on in the next round and reaches the drafts to delete wherever the directory lists them.
    #[tokio::test]
    async fn prepared_mail_cleanup_gets_past_drafts_it_keeps() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let mut maintenance = Maintenance::new(services.root.join("walked-local"));
        let prepared = services.root.join("walked-local/mail/prepared");
        std::fs::create_dir_all(&prepared).unwrap();
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
        let write = |name: String| {
            let path = prepared.join(name);
            std::fs::File::create(&path)
                .unwrap()
                .set_modified(stale)
                .unwrap();
            path
        };
        // Old drafts of mail still waiting to go out, two batches of them: the old cleanup took the first batch the
        // directory listed every round, so drafts listed after them were never looked at.
        let waiting = 2 * BATCH;
        let sql = format!(
            "INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES {}",
            vec!["(?,'notification.email','test',1,'{}',?,?,'pending')"; waiting].join(",")
        );
        let mut insert = db::query(&sql);
        for n in 0..waiting {
            insert = insert.bind(format!("waiting-{n}")).bind(now).bind(now);
        }
        insert.execute(db.executor()).await.unwrap();
        let kept: Vec<_> = db::query_scalar::<i64>(
            "SELECT eventid FROM gonof_common_outbox WHERE status='pending' ORDER BY eventid",
        )
        .fetch_all(db.executor())
        .await
        .unwrap()
        .into_iter()
        .map(|eventid| write(format!("{eventid}.eml")))
        .collect();
        assert_eq!(kept.len(), waiting);
        // Old drafts whose events are gone.
        let orphans: Vec<_> = (0..5)
            .map(|n| write(format!("{}.eml", 900_000_000 + n)))
            .collect();
        let mut removed = 0;
        for _ in 0..3 {
            removed += maintenance.run(db, &config, now).await.prepared_mail;
        }
        assert_eq!(
            removed,
            orphans.len(),
            "three rounds of one batch each cover the whole directory"
        );
        assert!(orphans.iter().all(|path| !path.exists()));
        assert!(kept.iter().all(|path| path.exists()));
        services.cleanup().await.unwrap();
    }

    /// A write that stopped between the temporary file and the link into place leaves the whole message in a `.tmp`
    /// file that no event names. Those older than an hour are removed from both mail folders whatever the database
    /// says; newer ones may still be in use and stay, as do files the mail worker does not write.
    #[tokio::test]
    async fn stale_temporary_mail_files_are_removed_without_their_events() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let mut maintenance = Maintenance::new(services.root.join("temporary-local"));
        let delivered = services.root.join("temporary-local/mail");
        let prepared = delivered.join("prepared");
        std::fs::create_dir_all(&prepared).unwrap();
        let write = |folder: &Path, name: &str, age: u64| {
            let path = folder.join(name);
            std::fs::write(&path, "To: member@example.test\r\n\r\nThe whole message").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age))
                .unwrap();
            path
        };
        let stale = (TEMPORARY_MAIL_AGE + 60) as u64;
        let gone = [
            write(&delivered, ".41.leasetoken.tmp", stale),
            write(&prepared, ".42.leasetoken.tmp", stale),
        ];
        let kept = [
            write(&delivered, ".43.leasetoken.tmp", 60),
            write(&prepared, ".44.leasetoken.tmp", 60),
            write(&delivered, "notes.tmp", stale),
            write(&prepared, ".notes.tmp", stale),
        ];
        let report = maintenance.run(db, &config, now).await;
        assert_eq!((report.temporary_mail, report.failed.len()), (2, 0));
        assert!(gone.iter().all(|path| !path.exists()));
        assert!(kept.iter().all(|path| path.exists()));
        assert_eq!(maintenance.run(db, &config, now).await.temporary_mail, 0);
        services.cleanup().await.unwrap();
    }

    /// One step that fails does not stop the others: with `mail/prepared` unreadable (as after restoring a backup
    /// with the wrong owner), expired SMS codes are still removed, and the report names the step that failed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_step_does_not_stop_the_other_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let config = crate::runtime::settings::tests::seeded().maintenance;
        let old = now - SMS_CODE_RETENTION - 60;
        db::query("INSERT INTO gonof_common_sms_code(mobile,purpose,code_hash,dateline,expires_at) VALUES('+8613800000009','login',?,?,?)")
            .bind("b".repeat(64))
            .bind(old)
            .bind(old + 300)
            .execute(db.executor())
            .await
            .unwrap();
        let mut maintenance = Maintenance::new(services.root.join("isolated-local"));
        let prepared = services.root.join("isolated-local/mail/prepared");
        std::fs::create_dir_all(&prepared).unwrap();
        std::fs::set_permissions(&prepared, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway, so the rest only runs for other accounts.
        let unreadable = std::fs::read_dir(&prepared).is_err();
        let report = maintenance.run(db, &config, now).await;
        std::fs::set_permissions(&prepared, std::fs::Permissions::from_mode(0o700)).unwrap();
        if unreadable {
            assert_eq!(report.failed, ["mail drafts"]);
        }
        assert_eq!(report.sms_codes, 1, "{report:?}");
        services.cleanup().await.unwrap();
    }

    /// A notification member, its author and a post for notifications to point at.
    async fn notification_fixture() -> (crate::test_support::TestFixture, i64) {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        crate::content::seed_forums(db).await.unwrap();
        crate::content::seed_fixture(db).await.unwrap();
        for uid in 9501..=9506 {
            db::query("INSERT INTO gonof_common_member(uid,username,regdate) VALUES(?,?,1)")
                .bind(uid)
                .bind(format!("retention-{uid}"))
                .execute(db.executor())
                .await
                .unwrap();
        }
        let pid = db::query_scalar("SELECT pid FROM gonof_forum_post ORDER BY pid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        (services, pid)
    }

    /// Writes a reply notification for `uid` through the regular insert (which counts it unread), then marks it read
    /// at `read_at` when that is not 0, taking it off the unread count as marking read does.
    async fn notification(
        db: &Database,
        uid: i64,
        pid: i64,
        key: &str,
        dateline: i64,
        read_at: i64,
    ) -> i64 {
        use crate::notifications::record::{Kind, NewNotice, Target};
        let id = crate::test_support::insert_notification(
            db,
            &NewNotice {
                uid,
                authorid: 9506,
                kind: Kind::Reply,
                target: Target::Post(pid),
                event_key: key,
                reason: "",
                dateline,
            },
        )
        .await
        .unwrap();
        if read_at > 0 {
            db::query("UPDATE gonof_home_notification SET read_at=? WHERE id=?")
                .bind(read_at)
                .bind(id)
                .execute(db.executor())
                .await
                .unwrap();
            db::query("UPDATE gonof_common_member SET unread_notifications=unread_notifications-1 WHERE uid=?")
                .bind(uid)
                .execute(db.executor())
                .await
                .unwrap();
        }
        id
    }

    async fn notification_ids(db: &Database) -> Vec<i64> {
        db::query_scalar("SELECT id FROM gonof_home_notification ORDER BY id")
            .fetch_all(db.executor())
            .await
            .unwrap()
    }

    async fn unread_of(db: &Database, uid: i64) -> i64 {
        db::query_scalar("SELECT unread_notifications FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await
            .unwrap()
    }

    /// A read notification goes 30 days (the default) after it was read, however old it is; an unread one 180 days
    /// after it arrived, and the member's unread count goes down with it.
    #[tokio::test]
    async fn notifications_expire_by_when_they_were_read_or_arrived() {
        let (services, pid) = notification_fixture().await;
        let db = &services.db;
        let now = crate::identity::now();
        let day = 86_400;
        notification(db, 9501, pid, "a", now - 40 * day, now - 31 * day).await;
        let read_lately = notification(db, 9501, pid, "b", now - 400 * day, now - 29 * day).await;
        notification(db, 9501, pid, "c", now - 181 * day, 0).await;
        let unread_recent = notification(db, 9501, pid, "d", now - 179 * day, 0).await;
        notification(db, 9502, pid, "e", now - 200 * day, 0).await;
        assert_eq!(
            (unread_of(db, 9501).await, unread_of(db, 9502).await),
            (2, 1)
        );
        let config = crate::runtime::read(db).await.unwrap().maintenance;
        assert_eq!(
            (
                config.notification_read_retention_days,
                config.notification_unread_retention_days
            ),
            (30, 180),
            "the defaults the migration writes"
        );
        let mut maintenance = Maintenance::new(services.root.join(".local"));
        let report = maintenance.run(db, &config, now).await;
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(report.notifications, 3);
        assert_eq!(notification_ids(db).await, [read_lately, unread_recent]);
        assert_eq!(
            (unread_of(db, 9501).await, unread_of(db, 9502).await),
            (1, 0)
        );
        assert_eq!(maintenance.run(db, &config, now).await.notifications, 0);
        services.cleanup().await.unwrap();
    }

    /// The walk takes a few members a round and carries on from where it stopped; a member with more expired
    /// notifications than one batch is looked at again until none are left.
    #[tokio::test]
    async fn notification_cleanup_walks_members_and_finishes_full_batches() {
        let (services, pid) = notification_fixture().await;
        let db = &services.db;
        let now = crate::identity::now();
        let old = now - 200 * 86_400;
        for uid in 9501..=9505 {
            notification(db, uid, pid, "only", old, 0).await;
        }
        let members: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_member")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let mut cursor = NotificationCursor::default();
        let mut deleted = 0;
        let mut rounds = 0;
        loop {
            deleted += purge_notifications(db, &mut cursor, now, old + 1, 2)
                .await
                .unwrap();
            rounds += 1;
            if cursor.after == 0 {
                break;
            }
            assert!(rounds <= members, "the walk does not end");
        }
        assert_eq!(deleted, 5);
        // Two members a round, and one more round that finds the end when the count is even.
        assert_eq!(rounds, members / 2 + 1);
        for uid in 9501..=9505 {
            assert_eq!(unread_of(db, uid).await, 0);
        }
        // More than one batch for one member: the next round stays on the same members.
        let mut values = Vec::new();
        for n in 0..BATCH + 5 {
            values.push(format!("(9501,9506,{pid},'reply','bulk-{n}',{old},0)"));
        }
        db::query(&format!(
            "INSERT INTO gonof_home_notification(uid,authorid,pid,type,event_key,dateline,read_at) VALUES {}",
            values.join(",")
        ))
        .execute(db.executor())
        .await
        .unwrap();
        db::query("UPDATE gonof_common_member SET unread_notifications=? WHERE uid=9501")
            .bind((BATCH + 5) as i64)
            .execute(db.executor())
            .await
            .unwrap();
        let mut cursor = NotificationCursor::default();
        assert_eq!(
            purge_notifications(db, &mut cursor, now, old + 1, MEMBER_SCAN)
                .await
                .unwrap(),
            BATCH as u64
        );
        assert_eq!(unread_of(db, 9501).await, 5);
        assert_eq!(cursor.after, 0, "a full batch keeps the walk where it was");
        assert_eq!(
            purge_notifications(db, &mut cursor, now, old + 1, MEMBER_SCAN)
                .await
                .unwrap(),
            5
        );
        assert_eq!(unread_of(db, 9501).await, 0);
        assert!(notification_ids(db).await.is_empty());
        services.cleanup().await.unwrap();
    }

    /// The candidate lookups are ranges of `gonof_notification_unread` per member on both backends, not a scan of
    /// the notification table.
    #[tokio::test]
    async fn notification_cleanup_reads_the_unread_index() {
        let (services, pid) = notification_fixture().await;
        let db = &services.db;
        let members = [9501_i64, 9502, 9503];
        // Enough rows, read and unread, for the planners to weigh the ranges against reading the whole index.
        let mut values = Vec::new();
        for uid in 9501..=9505 {
            for n in 0..400 {
                let read_at = if n % 2 == 0 { 0 } else { 2000 + n };
                values.push(format!(
                    "({uid},9506,{pid},'reply','plan-{n}',{},{read_at})",
                    1000 + n
                ));
            }
        }
        db::query(&format!(
            "INSERT INTO gonof_home_notification(uid,authorid,pid,type,event_key,dateline,read_at) VALUES {}",
            values.join(",")
        ))
        .execute(db.executor())
        .await
        .unwrap();
        db::raw_sql(if db.backend() == db::Backend::Sqlite {
            "ANALYZE"
        } else {
            "ANALYZE TABLE gonof_home_notification"
        })
        .execute(&db.pool)
        .await
        .unwrap();
        for condition in ["read_at>0 AND read_at<?", "read_at=0 AND dateline<?"] {
            // The statement as the cleanup runs it: parameters expanded, and rewritten for SQLite.
            let profile = std::sync::Arc::new(crate::debug::trace::RequestProfile::new());
            crate::debug::trace::scope(
                profile.clone(),
                expired_notifications(db, &members, condition, 1500),
            )
            .await
            .unwrap();
            let sql = profile.snapshot().pop().unwrap().expanded_sql;
            if db.backend() == db::Backend::Sqlite {
                let plan = crate::test_support::sqlite_plan(db, &sql).await.unwrap();
                assert!(
                    plan.iter()
                        .any(|step| step.contains("gonof_notification_unread")),
                    "{sql}: {plan:?}"
                );
                assert!(
                    !plan.iter().any(|step| step.starts_with("SCAN")),
                    "{sql}: {plan:?}"
                );
            } else {
                let rows = db::query(&format!("EXPLAIN FORMAT=JSON {sql}"))
                    .fetch_all(db.executor())
                    .await
                    .unwrap();
                let plan: String = rows[0].try_get(0).unwrap();
                let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
                let table = &plan["query_block"]["table"];
                assert_eq!(table["access_type"], "range", "{sql}: {plan}");
                assert_eq!(table["key"], "gonof_notification_unread", "{sql}: {plan}");
            }
        }
        services.cleanup().await.unwrap();
    }
}
