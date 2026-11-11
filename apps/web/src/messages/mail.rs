//! Enqueuing private message e-mail alerts.
//!
//! One mail is queued only when this conversation goes from "no unread" to "has unread" for the recipient, and is
//! delivered after [`DELAY_SECONDS`]; at delivery time it is dropped if the recipient has already read the message
//! (see `mail/message.rs`). The mail carries no message body. Off by default;
//! at most [`HOURLY_LIMIT`] mails per recipient per hour, anything beyond is not enqueued.
use crate::db::Tx;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    content::writes::ContentWriteError,
    db::{Database, DbRow},
    notifications::preferences,
};

pub(crate) const EVENT_TYPE: &str = "message.email";
/// The wait from enqueue to delivery: if the message is read on the site within this time, no mail is sent.
pub(crate) const DELAY_SECONDS: i64 = 600;
/// The most private message mails per recipient per hour, so many accounts taking turns to write cannot flood the
/// mailbox.
pub(crate) const HOURLY_LIMIT: i64 = 10;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageMail {
    pub thread_id: i64,
    pub message_id: i64,
    pub sender_uid: i64,
    pub uid: i64,
    /// Digest of the recipient e-mail at enqueue time; if the address has changed by delivery, the mail is dropped.
    pub email_hash: String,
}

/// Recipient state already locked and read in the sending transaction.
pub(crate) struct Recipient {
    uid: i64,
    email: Option<String>,
    verified: bool,
    frozen: bool,
}

impl Recipient {
    pub(crate) fn from_row(row: &DbRow, uid: i64) -> Result<Self, ContentWriteError> {
        let internal = |e: sqlx::Error| ContentWriteError::Internal(e.into());
        Ok(Self {
            uid,
            email: row.try_get("email").map_err(internal)?,
            verified: row.try_get::<i64, _>("emailstatus").map_err(internal)? == 1,
            frozen: row.try_get::<i64, _>("freeze").map_err(internal)? != 0,
        })
    }
}

/// `event_key` is segmented by recipient: `message.email:{uid}:{message id}`; the hourly limit counts over this
/// prefix range, using the `event_key` unique index.
pub(crate) fn key_range(uid: i64) -> (String, String) {
    (
        format!("{EVENT_TYPE}:{uid}:"),
        // ';' is the character right after ':', so the range covers exactly this prefix.
        format!("{EVENT_TYPE}:{uid};"),
    )
}

/// The caller has locked the member rows of sender and recipient and the conversation state row, and confirmed the
/// recipient had no unread messages before.
pub(crate) async fn enqueue(
    db: &Database,
    tx: &mut Tx<'_>,
    recipient: Recipient,
    sender_uid: i64,
    thread_id: i64,
    message_id: i64,
    now: i64,
) -> Result<()> {
    let Some(email) = recipient.email.filter(|email| !email.is_empty()) else {
        return Ok(());
    };
    if !recipient.verified || recipient.frozen {
        return Ok(());
    }
    if !preferences::locked(db, tx, recipient.uid)
        .await?
        .email_message
    {
        return Ok(());
    }
    let (low, high) = key_range(recipient.uid);
    let recent: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key>=? AND event_key<? AND dateline>=?",
    )
    .bind(&low)
    .bind(&high)
    .bind(now.saturating_sub(3600))
    .fetch_one(db.observe(&mut **tx))
    .await?;
    if recent >= HOURLY_LIMIT {
        return Ok(());
    }
    let payload = serde_json::to_string(&MessageMail {
        thread_id,
        message_id,
        sender_uid,
        uid: recipient.uid,
        email_hash: crate::identity::token_hash(&email),
    })
    .context("cannot build the private message mail event")?;
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'member',?,?,?,?)")
        .bind(format!("{low}{message_id}"))
        .bind(EVENT_TYPE)
        .bind(recipient.uid)
        .bind(payload)
        .bind(now)
        .bind(now.saturating_add(DELAY_SECONDS))
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_range_covers_one_recipient_only() {
        let (low, high) = key_range(42);
        let inside = |key: &str| key >= low.as_str() && key < high.as_str();
        assert!(inside("message.email:42:1") && inside("message.email:42:999999"));
        for key in [
            "message.email:4:1",
            "message.email:420:1",
            "message.email:43:1",
            "message.email:42",
        ] {
            assert!(!inside(key), "{key} must not fall in the range of 42");
        }
    }
}
