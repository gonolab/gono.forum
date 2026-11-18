//! Optional content mail always checks current access and preferences before submission.
use super::*;
use crate::{
    markdown,
    notifications::{
        delivery::{self, ContentMail},
        preferences::{self, Kind},
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn deliver(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    let Ok(payload) = serde_json::from_str::<ContentMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("content mail payload is malformed"));
    };
    if payload.uid != mail.aggregate_id
        || payload.uid <= 0
        || payload.actor_uid <= 0
        || payload.uid == payload.actor_uid
        || payload.pid <= 0
        || payload.quote_pids.len() > 50
        || payload.quote_pids.iter().any(|pid| *pid <= 0)
        || payload.candidates.is_empty()
        || payload.email_hash.len() != 64
        || payload.more_follows.len() > delivery::MORE_FOLLOWS
        || payload
            .follow
            .iter()
            .chain(&payload.more_follows)
            .any(|follow| {
                follow.id <= 0
                    || follow.target <= 0
                    || !matches!(follow.kind, Kind::Thread | Kind::Forum)
            })
    {
        return Ok(Delivery::Skipped("content mail payload is malformed"));
    }
    let db = &mailer.database;
    // The draft of an earlier attempt fixes the notification kind. It is read now, before any lock: a slow mail folder
    // must not hold up the member, forum and post rows below, nor every other write on SQLite.
    let draft = mailer.read_prepared(mail)?;
    let prepared_kind = draft
        .as_ref()
        .map(|draft| {
            draft
                .content_kind
                .context("the prepared content mail has no notification kind")
        })
        .transpose()?;
    let mut pids: BTreeSet<i64> = payload.quote_pids.iter().copied().collect();
    pids.insert(payload.pid);
    let discovered = delivery::discover(db, &pids).await?;
    let mut tx = db.begin_write().await?;
    let mut recipient = None;
    for uid in BTreeSet::from([payload.uid, payload.actor_uid]) {
        let row=crate::db::query("SELECT username,email,emailstatus,`freeze`,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid).fetch_optional(db.observe(&mut *tx)).await?;
        let recipient_side = uid == payload.uid;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(Delivery::Skipped(if recipient_side {
                "the recipient account does not exist"
            } else {
                "the author account does not exist"
            }));
        };
        if row.try_get::<i64, _>("freeze")? != 0 || db::text(&row, "account_state")? != "active" {
            tx.commit().await?;
            return Ok(Delivery::Skipped(if recipient_side {
                "the recipient account is frozen, deleted or inactive"
            } else {
                "the author account is frozen, deleted or inactive"
            }));
        }
        if recipient_side {
            recipient = Some(row);
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
    .bind(payload.actor_uid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if blocked > 0 {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient blocked the author"));
    }
    let targets = delivery::lock_sources(db, &mut tx, &discovered, payload.uid).await?;
    let Some(target) = targets
        .get(&payload.pid)
        .filter(|target| target.visible && target.authorid == Some(payload.actor_uid))
    else {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the notified content is hidden, deleted or not accessible",
        ));
    };
    let parent_fid: Option<i64> =
        crate::db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
            .bind(target.fid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    if crate::interaction::forum_filter::blocks_forum_locked(
        db,
        &mut tx,
        payload.uid,
        target.fid,
        parent_fid,
    )
    .await?
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient blocked the forum"));
    }
    let body=crate::db::query("SELECT p.message,t.deleted_at,firstpost.deleted_at AS first_deleted_at FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE p.pid=? FOR UPDATE")
        .bind(payload.pid).fetch_one(db.observe(&mut *tx)).await?;
    // Prepared messages may contain an earlier title. Withdrawn threads never send them.
    if body.try_get::<i64, _>("deleted_at")? > 0 || body.try_get::<i64, _>("first_deleted_at")? > 0
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the thread was withdrawn"));
    }
    let refs = markdown::references(&db::text(&body, "message")?)?;
    let mut candidates = BTreeSet::new();
    if payload.candidates.contains(&Kind::Reply)
        && delivery::reply_recipients(db, &mut tx, payload.pid)
            .await?
            .contains(&payload.uid)
    {
        candidates.insert(Kind::Reply);
    }
    if payload.candidates.contains(&Kind::Mention) && refs.members.contains(&payload.uid) {
        candidates.insert(Kind::Mention);
    }
    if payload.candidates.contains(&Kind::Quote)
        && payload.quote_pids.iter().any(|pid| {
            refs.quotes.contains(pid)
                && targets
                    .get(pid)
                    .is_some_and(|source| source.visible && source.authorid == Some(payload.uid))
        })
    {
        candidates.insert(Kind::Quote);
    }
    // Any one follow that still stands keeps the mail: one publication reaches a member who follows both a child
    // forum and its parent once, with both follows recorded.
    for follow in payload.follow.iter().chain(&payload.more_follows) {
        if candidates.contains(&follow.kind) {
            continue;
        }
        let table = match follow.kind {
            Kind::Thread if !target.first && follow.target == target.tid => {
                Some(("gonof_home_thread_follow", "tid"))
            }
            // The followed forum is the new thread's forum or its parent forum.
            Kind::Forum
                if target.first
                    && (follow.target == target.fid || Some(follow.target) == parent_fid) =>
            {
                Some(("gonof_home_forum_follow", "fid"))
            }
            _ => None,
        };
        if payload.candidates.contains(&follow.kind)
            && let Some((table, column)) = table
        {
            let exists: Option<i64> = crate::db::query_scalar(&format!(
                "SELECT id FROM {table} WHERE uid=? AND {column}=? AND id=?"
            ))
            .bind(payload.uid)
            .bind(follow.target)
            .bind(follow.id)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
            if exists.is_some() {
                candidates.insert(follow.kind);
            }
        }
    }
    let prefs = preferences::locked(db, &mut tx, payload.uid).await?;
    let Some(kind) = selected_kind(&prefs, &candidates, prepared_kind) else {
        tx.commit().await?;
        // Empty candidates (or the finalized kind not among the candidates) means the reply, quote, mention or follow
        // relation no longer holds; otherwise the preference was turned off.
        let lost = candidates.is_empty() || prepared_kind.is_some_and(|k| !candidates.contains(&k));
        return Ok(Delivery::Skipped(if lost {
            "the notification no longer applies (the reply, quote, mention or follow was undone)"
        } else {
            "the recipient turned off this kind of email notification"
        }));
    };
    hold_lease(
        db,
        &mut tx,
        mail,
        clock(),
        "the content mail delivery lease expired",
    )
    .await?;
    // Commit first to release the forum, thread, post and member row locks, then render, write the draft to disk and
    // send outside the locks: sending SMTP while holding locks would make posts and replies in that forum and its child
    // forums all queue behind this mail; on SQLite the whole site has a single writer, so the write transaction neither
    // reads templates nor writes to disk. The lease is fully renewed, so other deliverers cannot claim this mail; the
    // draft is written to disk before sending, and a disk write failure after commit is retried as usual. Hiding or
    // blocking committed after the re-check cannot stop this mail already in flight; when the link is opened it is
    // still displayed under the current permissions.
    tx.commit().await?;
    let username = db::text(&recipient, "username")?;
    let lang = mailer.site_language().await?;
    let site = mailer.site_name(None).await?;
    let action = match kind {
        Kind::Reply => lang.mail_content_reply(),
        Kind::Quote => lang.mail_content_quote(),
        Kind::Mention => lang.mail_content_mention(),
        Kind::Thread => lang.mail_content_thread(),
        Kind::Forum => lang.mail_content_forum(),
    };
    // Links use the canonical domain of the thread's forum (multi-domain sites section 7).
    let link = crate::mail::site_link(
        &mailer
            .forum_site_url(parent_fid.unwrap_or(target.fid))
            .await?,
        &format!("/posts/{}", payload.pid),
    )?;
    let values = BTreeMap::from([
        ("username".into(), username.clone()),
        ("title".into(), target.title.clone()),
        ("link".into(), link.to_string()),
        ("action".into(), action.into()),
        ("site_name".into(), site.clone()),
    ]);
    let default_body = format!(
        "{}\r\n\r\n{}\r\n{link}\r\n\r\n{}\r\n{site}\r\n",
        lang.mail_greeting(username.as_str()),
        lang.mail_content_line(action, target.title.as_str()),
        lang.mail_content_optout()
    );
    let (subject, body) = crate::mail::templates::render_in(
        db,
        "content",
        lang,
        &values,
        lang.mail_content_subject(),
        &default_body,
    )
    .await?;
    let draft = mailer
        .prepare_from(draft, mail, lang, &site, subject, body, Some(kind))
        .await?;
    mailer.publish_prepared(settings, mail, &to, &draft).await?;
    confirm_sent(db, mail).await?;
    Ok(Delivery::Sent)
}

// An unprepared event uses current preferences. Once its exact text is prepared,
// disabling/removing that category suppresses the event instead of relabeling it.
fn selected_kind(
    prefs: &preferences::Preferences,
    candidates: &BTreeSet<Kind>,
    prepared: Option<Kind>,
) -> Option<Kind> {
    match prepared {
        Some(kind) => (candidates.contains(&kind) && prefs.enabled(kind, true)).then_some(kind),
        None => prefs.highest(candidates, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_category_cannot_bypass_new_preferences_or_lost_content_eligibility() {
        let mut prefs = preferences::Preferences {
            email: [true; 5],
            ..Default::default()
        };
        let mut candidates = BTreeSet::from([Kind::Reply, Kind::Quote, Kind::Mention]);
        assert_eq!(selected_kind(&prefs, &candidates, None), Some(Kind::Reply));
        assert_eq!(
            selected_kind(&prefs, &candidates, Some(Kind::Quote)),
            Some(Kind::Quote)
        );
        prefs.email[0] = false;
        prefs.email[1] = false;
        assert_eq!(
            selected_kind(&prefs, &candidates, None),
            Some(Kind::Mention)
        );
        assert_eq!(selected_kind(&prefs, &candidates, Some(Kind::Quote)), None);
        prefs.email[1] = true;
        candidates.remove(&Kind::Quote);
        assert_eq!(selected_kind(&prefs, &candidates, Some(Kind::Quote)), None);
    }
}
