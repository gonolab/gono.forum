//! Private message e-mail reminders: eligibility is rechecked under the member row lock before delivery, and the
//! delivery is dropped if any condition fails.
use super::*;
use crate::{messages::mail::MessageMail, notifications::preferences};

pub(super) async fn deliver(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    let Ok(payload) = serde_json::from_str::<MessageMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("message mail payload is malformed"));
    };
    if payload.uid != mail.aggregate_id
        || payload.uid <= 0
        || payload.sender_uid <= 0
        || payload.uid == payload.sender_uid
        || payload.thread_id <= 0
        || payload.message_id <= 0
        || payload.email_hash.len() != 64
    {
        return Ok(Delivery::Skipped("message mail payload is malformed"));
    }
    let db = &mailer.database;
    let mut tx = db.begin_write().await?;
    // Same order as the sending transaction: lock both members in uid order.
    let mut recipient = None;
    let mut sender_name = String::new();
    let mut uids = [payload.uid, payload.sender_uid];
    uids.sort_unstable();
    for uid in uids {
        let row = crate::db::query("SELECT username,email,emailstatus,`freeze`,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
        let recipient_side = uid == payload.uid;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(Delivery::Skipped(if recipient_side {
                "the recipient account does not exist"
            } else {
                "the sender account does not exist"
            }));
        };
        if row.try_get::<i64, _>("freeze")? != 0 || db::text(&row, "account_state")? != "active" {
            tx.commit().await?;
            return Ok(Delivery::Skipped(if recipient_side {
                "the recipient account is frozen, deleted or inactive"
            } else {
                "the sender account is frozen, deleted or inactive"
            }));
        }
        if recipient_side {
            recipient = Some(row);
        } else {
            sender_name = db::text(&row, "username")?;
        }
    }
    let recipient = recipient.expect("recipient was locked");
    let to: Option<String> = recipient.try_get("email")?;
    let Some(to) = to.filter(|value| !value.is_empty()) else {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient account has no email address",
        ));
    };
    if recipient.try_get::<i64, _>("emailstatus")? != 1
        || crate::identity::token_hash(&to) != payload.email_hash
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient email address is unverified or changed",
        ));
    }
    let blocked: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
    )
    .bind(payload.uid)
    .bind(payload.sender_uid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if blocked > 0 {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient blocked the sender"));
    }
    if !preferences::locked(db, &mut tx, payload.uid)
        .await?
        .email_message
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient turned off message alerts by email",
        ));
    }
    let message: Option<i64> = crate::db::query_scalar(
        "SELECT sender_uid FROM gonof_home_message WHERE id=? AND thread_id=?",
    )
    .bind(payload.message_id)
    .bind(payload.thread_id)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    let state = crate::db::query(
        "SELECT unread,last_read_id FROM gonof_home_message_member WHERE thread_id=? AND uid=?",
    )
    .bind(payload.thread_id)
    .bind(payload.uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    let (Some(sender), Some(state)) = (message, state) else {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the conversation does not exist"));
    };
    if sender != payload.sender_uid {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the conversation does not exist"));
    }
    let unread: i64 = state.try_get("unread")?;
    if unread == 0 || state.try_get::<i64, _>("last_read_id")? >= payload.message_id {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient has read it"));
    }
    hold_lease(
        db,
        &mut tx,
        mail,
        clock(),
        "the message mail delivery lease expired",
    )
    .await?;
    // After the recheck and the lease renewal, commit first, then render, write the draft to disk and send outside the
    // lock (SQLite has a single writer for the whole site, so the write transaction neither reads templates nor writes
    // to disk). The lease is fully renewed, so no other deliverer can claim this e-mail; the draft is written to disk
    // before sending, and a write failure after the commit is retried as usual.
    tx.commit().await?;
    let username = db::text(&recipient, "username")?;
    let lang = mailer.site_language().await?;
    let site = mailer.site_name(None).await?;
    let link = crate::mail::site_link(
        &mailer.site_url().await?,
        &format!("/my/messages/{}", payload.thread_id),
    )?;
    let values = BTreeMap::from([
        ("username".into(), username.clone()),
        ("sender".into(), sender_name.clone()),
        ("count".into(), unread.to_string()),
        ("link".into(), link.to_string()),
        ("site_name".into(), site.clone()),
    ]);
    // No private message body: e-mail is not a private channel.
    let default_body = format!(
        "{}\r\n\r\n{}\r\n{link}\r\n\r\n{}\r\n{site}\r\n",
        lang.mail_greeting(username.as_str()),
        lang.mail_message_intro(sender_name.as_str(), unread),
        lang.mail_message_optout()
    );
    let (subject, body) = templates::render_in(
        db,
        "message",
        lang,
        &values,
        lang.mail_message_subject(),
        &default_body,
    )
    .await?;
    let draft = mailer.prepared(mail, lang, &site, subject, body).await?;
    mailer.publish_prepared(settings, mail, &to, &draft).await?;
    confirm_sent(db, mail).await?;
    Ok(Delivery::Sent)
}
