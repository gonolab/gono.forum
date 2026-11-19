//! One-to-one private messages: a conversation is unique per member pair, each side has one state row, and bodies
//! are plain text.
//!
//! The conversation list and unread counts query only the state table, with no `OR` and no sorting: who the other
//! side is and the latest activity time are each stored as a display copy on the state row. Conversations active in
//! the same second are ordered in code by their latest message ([`inbox`]).
use crate::db::Tx;
use anyhow::Result;

use crate::{
    content::writes::ContentWriteError,
    db::{Database, DbRow},
    identity::Actor,
};

pub(crate) mod mail;
pub mod rules;

pub const BODY_MAX: usize = 2000;
pub const PAGE_SIZE: usize = 20;

pub struct Conversation {
    pub thread_id: i64,
    pub counterpart_uid: i64,
    pub counterpart: String,
    pub counterpart_state: String,
    pub preview: String,
    pub unread: i64,
    pub last_dateline: i64,
}

pub struct Message {
    pub id: i64,
    pub mine: bool,
    pub body: String,
    pub dateline: i64,
}

pub struct Thread {
    pub thread_id: i64,
    pub counterpart_uid: i64,
    pub counterpart: String,
    pub counterpart_state: String,
    pub messages: Vec<Message>,
    pub total: i64,
    pub page: usize,
}

fn text(row: &DbRow, column: &str) -> Result<String, ContentWriteError> {
    crate::db::text(row, column).map_err(ContentWriteError::Internal)
}

/// The private message unread count on the member card: the number of unread conversations, not unread messages.
/// The user center card only needs a dot, so this only asks whether there are any rather than counting them; it
/// uses the gonof_message_member_unread index and stops at the first row hit.
pub async fn has_unread(db: &Database, uid: i64) -> Result<bool> {
    Ok(crate::db::query_scalar::<i64>(
        "SELECT 1 FROM gonof_home_message_member WHERE uid=? AND unread>0 LIMIT 1",
    )
    .bind(uid)
    .fetch_optional(db.executor())
    .await?
    .is_some())
}

pub async fn unread_conversations(db: &Database, uid: i64) -> Result<i64> {
    Ok(crate::db::query_scalar::<i64>(
        "SELECT COUNT(*) FROM gonof_home_message_member WHERE uid=? AND unread>0",
    )
    .bind(uid)
    .fetch_one(db.executor())
    .await?)
}

pub async fn inbox(
    db: &Database,
    uid: i64,
    page: usize,
    page_size: usize,
) -> Result<(Vec<Conversation>, i64), ContentWriteError> {
    if page == 0 || !(1..=100).contains(&page_size) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_page_number(),
        ));
    }
    let total = crate::db::query_scalar::<i64>(
        "SELECT COUNT(*) FROM gonof_home_message_member WHERE uid=?",
    )
    .bind(uid)
    .fetch_one(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if page > (total as usize).div_ceil(page_size).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    // The page in the order of the inbox index (uid,last_dateline,thread_id): the latest activity first. The time
    // has one-second precision, so conversations active in the same second are put in the order of their latest
    // message (the conversation's last_message_id, which is not on the state row) instead of by conversation id. The
    // page is read by the index, then the conversations of its first and last second are read in full, ordered in
    // code, and the page is taken from them at its place: only the page and its ties are read, never the whole list.
    let window = crate::db::query(
        "SELECT thread_id,last_dateline FROM gonof_home_message_member
         WHERE uid=? ORDER BY last_dateline DESC,thread_id DESC LIMIT ? OFFSET ?",
    )
    .bind(uid)
    .bind(page_size as i64)
    .bind(((page - 1) * page_size) as i64)
    .fetch_all(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let (Some(first), Some(last)) = (window.first(), window.last()) else {
        return Ok((Vec::new(), total));
    };
    let column = |row: &DbRow, name: &str| -> Result<i64, ContentWriteError> {
        row.try_get(name)
            .map_err(|e| ContentWriteError::Internal(e.into()))
    };
    let (newest, first_thread) = (column(first, "last_dateline")?, column(first, "thread_id")?);
    let oldest = column(last, "last_dateline")?;
    let rows = crate::db::query(
        "SELECT s.thread_id,s.counterpart_uid,s.unread,s.last_dateline,t.last_message_id,
                other.username AS counterpart,other.account_state AS counterpart_state,
                last.body AS preview
         FROM gonof_home_message_member s
         JOIN gonof_common_member other ON other.uid=s.counterpart_uid
         JOIN gonof_home_message_thread t ON t.id=s.thread_id
         LEFT JOIN gonof_home_message last ON last.id=t.last_message_id
         WHERE s.uid=? AND s.last_dateline>=? AND s.last_dateline<=?",
    )
    .bind(uid)
    .bind(oldest)
    .bind(newest)
    .fetch_all(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let mut list = rows
        .into_iter()
        .map(|row| {
            Ok((
                column(&row, "last_message_id")?,
                Conversation {
                    thread_id: column(&row, "thread_id")?,
                    counterpart_uid: column(&row, "counterpart_uid")?,
                    counterpart: text(&row, "counterpart")?,
                    counterpart_state: text(&row, "counterpart_state")?,
                    preview: row
                        .try_get::<Option<String>, _>("preview")
                        .map_err(|e| ContentWriteError::Internal(e.into()))?
                        .unwrap_or_default(),
                    unread: column(&row, "unread")?,
                    last_dateline: column(&row, "last_dateline")?,
                },
            ))
        })
        .collect::<Result<Vec<_>, ContentWriteError>>()?;
    // In index order the page starts after the conversations of its first second with a larger id; in the order
    // shown, those of that second still come first, so the page starts that many places into the ordered list.
    let before = list
        .iter()
        .filter(|(_, c)| c.last_dateline == newest && c.thread_id > first_thread)
        .count();
    list.sort_by(|(a_last, a), (b_last, b)| {
        (b.last_dateline, b_last, b.thread_id).cmp(&(a.last_dateline, a_last, a.thread_id))
    });
    let list = list
        .into_iter()
        .skip(before)
        .take(window.len())
        .map(|(_, conversation)| conversation)
        .collect();
    Ok((list, total))
}

/// Before sending, only conversation ownership is checked; bodies are not read and unread is not cleared.
pub async fn counterpart(
    db: &Database,
    uid: i64,
    thread_id: i64,
) -> Result<i64, ContentWriteError> {
    crate::db::query_scalar(
        "SELECT counterpart_uid FROM gonof_home_message_member WHERE thread_id=? AND uid=?",
    )
    .bind(thread_id)
    .bind(uid)
    .fetch_optional(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?
    .ok_or(ContentWriteError::NotFound)
}

/// One page of the messages up to `up_to` (the snapshot of the conversation's last message). `total` is the number of
/// those messages and the page number must already be bounds-checked. A page nearer the end is read backwards from
/// `up_to`, so the latest page, the one a conversation opens on, skips no rows.
async fn page_messages(
    db: &Database,
    (thread_id, up_to): (i64, i64),
    uid: i64,
    (page, page_size): (usize, usize),
    total: usize,
) -> Result<Vec<Message>, ContentWriteError> {
    let offset = (page - 1) * page_size;
    let on_page = total.saturating_sub(offset).min(page_size);
    let after = total.saturating_sub(offset + on_page);
    let backwards = after < offset;
    let rows = crate::db::query(if backwards {
        "SELECT id,sender_uid,body,dateline FROM gonof_home_message
         WHERE thread_id=? AND id<=? ORDER BY id DESC LIMIT ? OFFSET ?"
    } else {
        "SELECT id,sender_uid,body,dateline FROM gonof_home_message
         WHERE thread_id=? AND id<=? ORDER BY id ASC LIMIT ? OFFSET ?"
    })
    .bind(thread_id)
    .bind(up_to)
    .bind(if backwards { on_page } else { page_size } as i64)
    .bind(if backwards { after } else { offset } as i64)
    .fetch_all(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let mut messages = rows
        .into_iter()
        .map(|row| {
            let sender: i64 = row
                .try_get("sender_uid")
                .map_err(|e| ContentWriteError::Internal(e.into()))?;
            Ok(Message {
                id: row
                    .try_get("id")
                    .map_err(|e| ContentWriteError::Internal(e.into()))?,
                mine: sender == uid,
                body: text(&row, "body")?,
                dateline: row
                    .try_get("dateline")
                    .map_err(|e| ContentWriteError::Internal(e.into()))?,
            })
        })
        .collect::<Result<Vec<_>, ContentWriteError>>()?;
    if backwards {
        messages.reverse();
    }
    Ok(messages)
}

/// The number of messages of the conversation up to `up_to`, through the `(thread_id,id)` index.
async fn message_total(
    db: &Database,
    thread_id: i64,
    up_to: i64,
) -> Result<i64, ContentWriteError> {
    crate::db::query_scalar::<i64>(
        "SELECT COUNT(*) FROM gonof_home_message WHERE thread_id=? AND id<=?",
    )
    .bind(thread_id)
    .bind(up_to)
    .fetch_one(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))
}

/// Open a conversation: without a page number it starts at the latest page, still in ascending time order within
/// the page. Reads the messages and resets the member's unread to zero. Conversations not the member's own are
/// always treated as nonexistent.
///
/// What is shown is a snapshot: the conversation's last message id at the moment the read position was settled.
/// When there is something to mark read, a short write transaction takes the conversation lock (the one the send path
/// takes), reads the last message id and advances the read position to it; when the position has already caught
/// up, the id read with the state row is the snapshot and nothing is written. The messages are counted and read
/// after that, without holding any lock, and only up to the snapshot: a message whose send commits meanwhile is not
/// shown, and since its send raised unread again it stays unread instead of being shown as read.
pub async fn conversation(
    db: &Database,
    uid: i64,
    thread_id: i64,
    page: Option<usize>,
    page_size: usize,
) -> Result<Thread, ContentWriteError> {
    if page == Some(0) || !(1..=100).contains(&page_size) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_page_number(),
        ));
    }
    let state = crate::db::query(
        "SELECT s.counterpart_uid,s.unread,s.last_read_id,t.last_message_id,
                other.username AS counterpart,other.account_state AS counterpart_state
         FROM gonof_home_message_member s
         JOIN gonof_common_member other ON other.uid=s.counterpart_uid
         JOIN gonof_home_message_thread t ON t.id=s.thread_id
         WHERE s.thread_id=? AND s.uid=?",
    )
    .bind(thread_id)
    .bind(uid)
    .fetch_optional(db.executor())
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?
    .ok_or(ContentWriteError::NotFound)?;
    let counterpart_uid: i64 = state
        .try_get("counterpart_uid")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let unread: i64 = state
        .try_get("unread")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let last_read_id: i64 = state
        .try_get("last_read_id")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let observed_last_id: i64 = state
        .try_get("last_message_id")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let counterpart = text(&state, "counterpart")?;
    let counterpart_state = text(&state, "counterpart_state")?;
    let pages = |total: i64| (total as usize).div_ceil(page_size).max(1);
    // With a page number, check the bounds first: out-of-range requests need not pay for a write transaction and the
    // conversation lock. Messages only grow, so this count can only underestimate the pages, and a page number that
    // passes here is still in range for the snapshot below.
    let mut counted = None;
    if let Some(page) = page {
        let total = message_total(db, thread_id, observed_last_id).await?;
        if page > pages(total) {
            return Err(ContentWriteError::NotFound);
        }
        counted = Some(total);
    }
    let snapshot_last_id = if unread == 0 && last_read_id >= observed_last_id {
        observed_last_id
    } else {
        // The read snapshot and clearing unread happen under the conversation lock, so a message arriving after the
        // snapshot is not swallowed by unread=0: its send waits for the lock and raises unread again.
        let mut tx = db
            .begin_write()
            .await
            .map_err(ContentWriteError::Internal)?;
        let last_message_id: i64 = crate::db::query_scalar(
            "SELECT last_message_id FROM gonof_home_message_thread WHERE id=? FOR UPDATE",
        )
        .bind(thread_id)
        .fetch_one(db.observe(&mut *tx))
        .await
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
        // Opening marks the conversation read up to this snapshot regardless of the page viewed.
        if last_message_id > 0 {
            crate::db::query(
                "UPDATE gonof_home_message_member SET unread=0,last_read_id=GREATEST(last_read_id,?)
                 WHERE thread_id=? AND uid=?",
            )
            .bind(last_message_id)
            .bind(thread_id)
            .bind(uid)
            .execute(db.observe(&mut *tx))
            .await
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        }
        tx.commit()
            .await
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        last_message_id
    };
    let total = match counted {
        Some(total) if snapshot_last_id == observed_last_id => total,
        _ => message_total(db, thread_id, snapshot_last_id).await?,
    };
    let page = page.unwrap_or_else(|| pages(total));
    let messages = page_messages(
        db,
        (thread_id, snapshot_last_id),
        uid,
        (page, page_size),
        total as usize,
    )
    .await?;
    Ok(Thread {
        thread_id,
        counterpart_uid,
        counterpart,
        counterpart_state,
        messages,
        total,
        page,
    })
}

async fn locked_member(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
) -> Result<(), ContentWriteError> {
    let row = crate::db::query(
        "SELECT credentials_version,`freeze`,mute_until,emailstatus,mobile,adminid,account_state
         FROM gonof_common_member WHERE uid=? FOR UPDATE",
    )
    .bind(actor.uid)
    .fetch_optional(db.observe(&mut **tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?
    .ok_or(ContentWriteError::Unauthorized)?;
    let version: i64 = row
        .try_get("credentials_version")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let frozen: i64 = row
        .try_get("freeze")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if version != actor.credentials_version
        || frozen != 0
        || text(&row, "account_state")? != "active"
    {
        return Err(ContentWriteError::Unauthorized);
    }
    let admin: i64 = row
        .try_get("adminid")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let verified: i64 = row
        .try_get("emailstatus")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let mobile: Option<String> = row
        .try_get("mobile")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let participation = crate::login::Participation::current();
    if admin != 1
        && !crate::login::permits(
            db.observe(&mut **tx),
            actor.uid,
            verified == 1,
            mobile.is_some(),
        )
        .await
        .map_err(|e| ContentWriteError::Internal(e.into()))?
    {
        return Err(ContentWriteError::Forbidden(
            participation.message(crate::i18n::current().common_need_email()),
        ));
    }
    // No private messages while muted; both new conversations and replies are stopped here (docs/roles.md, "Mutes").
    let mute_until: i64 = row
        .try_get("mute_until")
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if crate::moderation::muted(mute_until, crate::identity::now()) {
        return Err(ContentWriteError::Restricted(
            crate::moderation::muted_message(mute_until),
        ));
    }
    Ok(())
}

/// The recipient is in the account deletion cooling-off period: say plainly that they are deleting their account; their
/// profile page already shows "Being deleted". The profile page's message entry uses this sentence when disabled, too.
pub fn recipient_deleting() -> &'static str {
    crate::i18n::current().my_error_recipient_deleting()
}
/// Blocked by the recipient: the block itself is not revealed; the profile page's message entry uses this sentence
/// when disabled, too.
pub fn recipient_refuses() -> &'static str {
    crate::i18n::current().my_error_recipient_refuses()
}

/// CRLF and lone CR line breaks become LF: the form of a textarea's value that the page counts (its API value).
fn normalize_line_breaks(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\r', "\n")
}

/// Send a private message, creating the conversation if it does not exist. Returns the conversation id.
///
/// When blocked by the recipient the hint is always "This member does not accept your private messages", without
/// revealing the block itself; when the recipient is in the account deletion cooling-off period it says plainly "This
/// member is closing their account and cannot receive private messages", and when already deleted "This member's
/// account has been deleted; you can no longer send them private messages".
///
/// Server-confirmed deadlocks and lock wait timeouts retry the whole send, at most 3 times: a replay with the same
/// request id only returns the original conversation, so retries are idempotent. If it still fails it returns a
/// conflict (409) asking to retry later, no longer a 404 or 500.
pub async fn send(
    db: &Database,
    actor: Actor,
    counterpart_uid: i64,
    body: &str,
    request_key: &str,
) -> Result<i64, ContentWriteError> {
    for attempt in 0..3 {
        match send_once(db, actor, counterpart_uid, body, request_key).await {
            Err(error) if error.retryable() => {
                if attempt == 2 {
                    tracing::warn!(%error, uid = actor.uid, "send kept hitting lock conflicts");
                    return Err(ContentWriteError::Conflict(
                        crate::i18n::current().my_error_message_busy(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(20 << attempt)).await;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt returns its result")
}

/// Whether a failed send leaves its request key free, so the form refilled after the failure can carry the same key:
/// if an earlier submission with that key was in fact delivered (its response lost), sending the refilled form returns
/// that message instead of a second copy. Only a key already used for another body has to be replaced.
pub fn keeps_request_key(error: &ContentWriteError) -> bool {
    !matches!(error, ContentWriteError::Conflict(message)
        if *message == crate::i18n::current().my_error_message_key_used())
}

/// Mapping when locking sender and recipient fails: only a nonexistent account is a 404; database errors (deadlocks
/// included) are passed through as is, for the caller to judge whether to retry.
fn lock_failure(error: anyhow::Error) -> ContentWriteError {
    if error
        .downcast_ref::<crate::activities::ActivityFailure>()
        .is_some()
    {
        ContentWriteError::NotFound
    } else {
        ContentWriteError::Internal(error)
    }
}

async fn send_once(
    db: &Database,
    actor: Actor,
    counterpart_uid: i64,
    body: &str,
    request_key: &str,
) -> Result<i64, ContentWriteError> {
    // Browsers send a textarea's line breaks as CRLF but count each as one character, and the hint promises
    // characters as typed: count and store with LF line breaks, so a message that fits on the page fits here.
    let body = normalize_line_breaks(body);
    let body = body.trim();
    if body.is_empty() || body.chars().count() > BODY_MAX {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_message_length(),
        ));
    }
    if counterpart_uid == actor.uid {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_message_self(),
        ));
    }
    if !crate::identity::valid_request_key(request_key) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_request_invalid(),
        ));
    }
    let mut tx = db
        .begin_write()
        .await
        .map_err(ContentWriteError::Internal)?;
    crate::runtime::guard_write(db, &mut tx, false)
        .await
        .map_err(crate::organization::identity_error)?;
    crate::activities::lock_members(db, &mut tx, &[actor.uid, counterpart_uid])
        .await
        .map_err(lock_failure)?;
    locked_member(db, &mut tx, actor).await?;
    let target =
        // A frozen recipient still receives messages (a freeze only stops sending); only whether they were deleted
        // matters. The e-mail and freeze status are for private message mail alerts and are fetched with the
        // same row, without another query.
        crate::db::query("SELECT account_state,email,emailstatus,`freeze` FROM gonof_common_member WHERE uid=?")
            .bind(counterpart_uid)
            .fetch_optional(db.observe(&mut *tx))
            .await
            .map_err(|e| ContentWriteError::Internal(e.into()))?
            .ok_or(ContentWriteError::NotFound)?;
    match text(&target, "account_state")?.as_str() {
        "active" => {}
        "deleting" => return Err(ContentWriteError::Forbidden(recipient_deleting())),
        _ => {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().my_error_recipient_deleted(),
            ));
        }
    }
    let blocked: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
    )
    .bind(counterpart_uid)
    .bind(actor.uid)
    .fetch_one(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if blocked > 0 {
        return Err(ContentWriteError::Forbidden(recipient_refuses()));
    }
    let now = crate::identity::now();
    let key = rules::pair_key(actor.uid, counterpart_uid).map_err(ContentWriteError::Internal)?;
    crate::db::query(
        "INSERT INTO gonof_home_message_thread(pair_key,dateline) VALUES(?,?)
         ON DUPLICATE KEY UPDATE id=id",
    )
    .bind(&key)
    .bind(now)
    .execute(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let thread_id: i64 = crate::db::query_scalar(
        "SELECT id FROM gonof_home_message_thread WHERE pair_key=? FOR UPDATE",
    )
    .bind(&key)
    .fetch_one(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    for (uid, other) in [(actor.uid, counterpart_uid), (counterpart_uid, actor.uid)] {
        crate::db::query(
            "INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,last_dateline)
             VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE thread_id=thread_id",
        )
        .bind(thread_id)
        .bind(uid)
        .bind(other)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    }
    // The two state rows' last_dateline may only move forward.
    // The send time is the later of the wall clock and the existing time: with a clock set back or skew between
    // machines, a new message is still recorded after the previous one.
    // This must be a locking read: the transaction's snapshot may predate the moment it waited for the member row
    // lock, and a plain read would see old values.
    // The recipient's previous unread count is also taken in this locking read: a private message mail is queued
    // only when it goes from 0 to 1.
    // The same read returns each row's counterpart, so the participants are checked without another query.
    let previous = crate::db::query(
        "SELECT uid,counterpart_uid,unread,last_dateline FROM gonof_home_message_member WHERE thread_id=? FOR UPDATE",
    )
    .bind(thread_id)
    .fetch_all(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let mut recipient_unread = 0;
    let mut latest = now;
    let mut participants = Vec::with_capacity(previous.len());
    for row in &previous {
        let uid: i64 = row
            .try_get("uid")
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        let other: i64 = row
            .try_get("counterpart_uid")
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        participants.push((uid, other));
        let dateline: i64 = row
            .try_get("last_dateline")
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        latest = latest.max(dateline);
        if uid == counterpart_uid {
            recipient_unread = row
                .try_get("unread")
                .map_err(|e| ContentWriteError::Internal(e.into()))?;
        }
    }
    // Participants: the two sides named by the conversation id are the sender and the recipient;
    // the two state rows are exactly this pair of uids, each the other's counterpart.
    rules::check_participants(&key, actor.uid, counterpart_uid, &participants)
        .map_err(ContentWriteError::Internal)?;
    let now = latest;
    let event_key = format!("dm:{}:{thread_id}:{request_key}", actor.uid);
    // The member rows and conversation row are locked, so sends in the same conversation run serially. SQLx enables
    // FOUND_ROWS, so an empty ON DUPLICATE KEY update also returns 1 and cannot tell whether a message was added.
    let existing = crate::db::query("SELECT body FROM gonof_home_message WHERE event_key=?")
        .bind(&event_key)
        .fetch_optional(db.observe(&mut *tx))
        .await
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if let Some(existing) = existing {
        // The same request key may only replay the same message; a different body means the form was reused, and we
        // cannot pretend the send succeeded.
        if text(&existing, "body")? != body {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().my_error_message_key_used(),
            ));
        }
        tx.commit()
            .await
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
        return Ok(thread_id);
    }
    let inserted = crate::db::query(
        "INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key)
         VALUES(?,?,?,?,?)",
    )
    .bind(thread_id)
    .bind(actor.uid)
    .bind(body)
    .bind(now)
    .bind(&event_key)
    .execute(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    let message_id = i64::try_from(inserted.last_insert_id())
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    // The conversation's last message only grows: a new message's id comes from the auto-increment primary key and
    // is necessarily larger; the conditional update states this, and if nothing matches the transaction rolls back.
    let advanced = crate::db::query(
        "UPDATE gonof_home_message_thread SET last_message_id=? WHERE id=? AND last_message_id<?",
    )
    .bind(message_id)
    .bind(thread_id)
    .bind(message_id)
    .execute(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?
    .rows_affected();
    if advanced != 1 {
        return Err(ContentWriteError::Internal(anyhow::anyhow!(
            "the last message of conversation {thread_id} did not advance to {message_id}"
        )));
    }
    crate::db::query(
        "UPDATE gonof_home_message_member SET last_dateline=GREATEST(last_dateline,?),unread=0,last_read_id=GREATEST(last_read_id,?)
         WHERE thread_id=? AND uid=?",
    )
    .bind(now)
    .bind(message_id)
    .bind(thread_id)
    .bind(actor.uid)
    .execute(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    crate::db::query(
        "UPDATE gonof_home_message_member SET last_dateline=GREATEST(last_dateline,?),unread=unread+1
         WHERE thread_id=? AND uid=?",
    )
    .bind(now)
    .bind(thread_id)
    .bind(counterpart_uid)
    .execute(db.observe(&mut *tx))
    .await
    .map_err(|e| ContentWriteError::Internal(e.into()))?;
    if recipient_unread == 0 {
        mail::enqueue(
            db,
            &mut tx,
            mail::Recipient::from_row(&target, counterpart_uid)?,
            actor.uid,
            thread_id,
            message_id,
            now,
        )
        .await
        .map_err(ContentWriteError::Internal)?;
    }
    tx.commit()
        .await
        .map_err(|e| ContentWriteError::Internal(e.into()))?;
    Ok(thread_id)
}

/// Prefill for the new message page `?to=<uid>`: only members who can receive messages (`account_state='active'`)
/// get their username back; nonexistent, deleted or deleting accounts are not prefilled, and the page leaves the
/// field empty for the member to fill in.
pub async fn recipient_name(db: &Database, uid: i64) -> anyhow::Result<Option<String>> {
    let row =
        crate::db::query("SELECT username,account_state FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.executor())
            .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    (crate::db::text(&row, "account_state")? == "active")
        .then(|| crate::db::text(&row, "username"))
        .transpose()
}

/// The same normalization as address resolution: NFKC, then lowercase, with surrounding whitespace trimmed.
pub fn normalized_username(username: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    username.trim().nfkc().collect::<String>().to_lowercase()
}

/// Find the recipient by username; not found or deleted both count as nonexistent. Accounts in the deletion cooling-off
/// period are found as usual and the send transaction says plainly "This member is closing their account …", the same
/// sentence as the profile page's message entry.
pub async fn find_member(db: &Database, username: &str) -> Result<i64, ContentWriteError> {
    let normalized = normalized_username(username);
    let row =
        crate::db::query("SELECT uid,account_state FROM gonof_common_member WHERE username=?")
            .bind(&normalized)
            .fetch_optional(db.executor())
            .await
            .map_err(|e| ContentWriteError::Internal(e.into()))?;
    match row {
        Some(row) if text(&row, "account_state")? != "deleted" => row
            .try_get("uid")
            .map_err(|e| ContentWriteError::Internal(e.into())),
        _ => Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_member_not_found(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Database errors (deadlocks included) while locking sender and recipient must not be reported as
    // "the target does not exist".
    #[tokio::test]
    async fn lock_failures_are_not_reported_as_missing_members() {
        // Only verifies the MySQL deadlock error number forged with SIGNAL.
        crate::mysql_only!();
        let fixture = crate::test_support::TestFixture::new().await.unwrap();
        let deadlock = crate::db::raw_sql(
            "SIGNAL SQLSTATE '40001' SET MYSQL_ERRNO=1213, MESSAGE_TEXT='message lock contract'",
        )
        .execute(&fixture.db.pool)
        .await
        .unwrap_err();
        let mapped = lock_failure(anyhow::Error::new(deadlock));
        assert!(
            matches!(mapped, ContentWriteError::Internal(_)) && mapped.retryable(),
            "a deadlock must be recognised as retryable"
        );
        let missing = lock_failure(anyhow::anyhow!(crate::activities::ActivityFailure(
            "The account does not exist".into()
        )));
        assert!(matches!(missing, ContentWriteError::NotFound));
        fixture.cleanup().await.unwrap();
    }
}
