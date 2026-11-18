//! One publication produces at most one notification per recipient, across all kinds.
use crate::db::Tx;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::*;
use crate::{
    markdown,
    notifications::preferences::{self, Kind},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentEvent {
    pid: i64,
    actor_uid: i64,
    recipient_uids: Vec<i64>,
    mention_uids: Vec<i64>,
    quote_pids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subscription: Option<Subscription>,
    /// What is sent after the current follow pages are done: when a new thread is posted in a child forum, the parent
    /// forum's followers (following a parent forum includes new threads in its child forums).
    /// Someone following both levels gets only one notification: if one with the same event key already exists, none is
    /// written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    then: Option<Subscription>,
    #[serde(default, skip_serializing_if = "is_false")]
    direct_done: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Subscription {
    kind: Kind,
    target: i64,
    highwater: i64,
    cursor: i64,
}
impl Subscription {
    fn storage(&self) -> Option<(&'static str, &'static str)> {
        match self.kind {
            Kind::Thread => Some(("gonof_home_thread_follow", "tid")),
            Kind::Forum => Some(("gonof_home_forum_follow", "fid")),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FollowCandidate {
    pub kind: Kind,
    pub target: i64,
    pub id: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentMail {
    pub pid: i64,
    pub actor_uid: i64,
    pub uid: i64,
    pub candidates: BTreeSet<Kind>,
    pub quote_pids: Vec<i64>,
    pub follow: Option<FollowCandidate>,
    /// Further follows that reached the member with this publication, added when a later page of the fan-out finds
    /// the mail already queued (a new thread in a child forum, for someone following the child forum and its parent).
    /// Delivery sends the mail while any one of the follows still stands. Absent from mail with a single follow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more_follows: Vec<FollowCandidate>,
    pub email_hash: String,
}

/// At most this many follows in [`ContentMail::more_follows`]; a publication reaches a member through at most two
/// forum follows today.
pub(crate) const MORE_FOLLOWS: usize = 8;

/// Adds this page's kinds and follow to the member's content mail queued by an earlier page of the same publication.
/// Only a mail still waiting is changed: one being sent or finished is left as it is.
async fn merge_content_mail(
    db: &Database,
    tx: &mut Tx<'_>,
    eventid: i64,
    kinds: &BTreeSet<Kind>,
    follow: Option<FollowCandidate>,
) -> anyhow::Result<()> {
    let row = crate::db::query(
        "SELECT status,payload FROM gonof_common_outbox WHERE eventid=? FOR UPDATE",
    )
    .bind(eventid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    if db::text(&row, "status")? != "pending" {
        return Ok(());
    }
    let Ok(mut mail) = serde_json::from_str::<ContentMail>(&db::text(&row, "payload")?) else {
        return Ok(());
    };
    let mut changed = false;
    for kind in kinds {
        changed |= mail.candidates.insert(*kind);
    }
    if let Some(follow) = follow {
        if mail.follow.is_none() {
            mail.follow = Some(follow);
            changed = true;
        } else if mail.follow.as_ref() != Some(&follow)
            && !mail.more_follows.contains(&follow)
            && mail.more_follows.len() < MORE_FOLLOWS
        {
            mail.more_follows.push(follow);
            changed = true;
        }
    }
    if changed {
        crate::db::query(
            "UPDATE gonof_common_outbox SET payload=? WHERE eventid=? AND status='pending'",
        )
        .bind(serde_json::to_string(&mail)?)
        .bind(eventid)
        .execute(db.observe(&mut **tx))
        .await?;
    }
    Ok(())
}

pub(crate) async fn reply_recipients(
    db: &Database,
    tx: &mut Tx<'_>,
    pid: i64,
) -> Result<BTreeSet<i64>> {
    let row = crate::db::query("SELECT p.first,p.tid,p.root_pid,p.reply_to_pid,t.authorid AS thread_author FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=?")
        .bind(pid).fetch_one(db.observe(&mut **tx)).await?;
    let mut recipients = BTreeSet::new();
    if row.try_get::<i64, _>("first")? == 1 {
        return Ok(recipients);
    }
    if let Some(root) = row.try_get::<Option<i64>, _>("root_pid")? {
        let target: i64 = row.try_get("reply_to_pid")?;
        let authors = crate::db::query(
            "SELECT authorid FROM gonof_forum_post WHERE tid=? AND (pid=? OR pid=?)",
        )
        .bind(row.try_get::<i64, _>("tid")?)
        .bind(root)
        .bind(target)
        .fetch_all(db.observe(&mut **tx))
        .await?;
        for author in authors {
            recipients.extend(author.try_get::<Option<i64>, _>("authorid")?);
        }
    } else {
        recipients.extend(row.try_get::<Option<i64>, _>("thread_author")?);
    }
    Ok(recipients)
}

async fn active_member(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<bool> {
    let row =
        crate::db::query("SELECT `freeze`,account_state FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    Ok(if let Some(row) = row {
        row.try_get::<i64, _>("freeze")? == 0 && db::text(&row, "account_state")? == "active"
    } else {
        false
    })
}

/// The caller holds the publishing account and content locks. Candidate reads do
/// not lock other accounts or quote sources in reverse order; delivery rechecks them.
pub(crate) async fn enqueue_content(
    db: &Database,
    tx: &mut Tx<'_>,
    pid: i64,
    actor_uid: i64,
    timestamp: i64,
) -> Result<()> {
    let row = crate::db::query("SELECT first,fid,tid,message FROM gonof_forum_post WHERE pid=?")
        .bind(pid)
        .fetch_one(db.observe(&mut **tx))
        .await?;
    let refs = markdown::references(&db::text(&row, "message")?)?;
    let mut recipients = BTreeSet::new();
    for uid in reply_recipients(db, tx, pid).await? {
        if uid != actor_uid && active_member(db, tx, uid).await? {
            recipients.insert(uid);
        }
    }
    let mut mentions = Vec::new();
    for uid in refs.members {
        if uid == actor_uid {
            continue;
        }
        if active_member(db, tx, uid).await? {
            mentions.push(uid);
        }
    }
    let mut quotes = Vec::new();
    for quote in refs.quotes {
        if quote == pid {
            continue;
        }
        let public: i64 = crate::db::query_scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid
             JOIN gonof_forum_forum f ON f.fid=t.fid
             JOIN gonof_common_member author ON author.uid=p.authorid
             WHERE p.pid=? AND author.freeze=0 AND author.account_state='active' AND {} AND {}",
            content::thread_visible_by_forum_sql("f", Some(actor_uid)),
            content::post_visible("p")
        ))
        .bind(quote)
        .fetch_one(db.observe(&mut **tx))
        .await?;
        if public == 1 {
            quotes.push(quote);
        }
    }
    let first = row.try_get::<i64, _>("first")? == 1;
    let (table, column, target, kind) = if first {
        (
            "gonof_home_forum_follow",
            "fid",
            row.try_get::<i64, _>("fid")?,
            Kind::Forum,
        )
    } else {
        (
            "gonof_home_thread_follow",
            "tid",
            row.try_get::<i64, _>("tid")?,
            Kind::Thread,
        )
    };
    // Publishing owns the same forum/thread locks taken by every subscription write.
    // Re-subscribing receives a new identity, outside this publication's upper bound.
    let mut subscription = subscription_page(db, tx, (table, column), kind, target).await?;
    // A new thread posted in a child forum: the parent forum's followers receive it too. Posting has
    // already taken shared locks on the forum path (parent forum and this forum); follow writes must lock the forum path
    // as well, and the upper bound is read under the lock.
    let mut then = None;
    if first {
        let parent: Option<i64> =
            crate::db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
                .bind(target)
                .fetch_one(db.observe(&mut **tx))
                .await?;
        if let Some(parent) = parent {
            then = subscription_page(db, tx, (table, column), kind, parent).await?;
            if subscription.is_none() {
                subscription = then.take();
            }
        }
    }
    if first && mentions.is_empty() && quotes.is_empty() && subscription.is_none() {
        return Ok(());
    }
    let payload = serde_json::to_string(&ContentEvent {
        pid,
        actor_uid,
        recipient_uids: recipients.into_iter().collect(),
        mention_uids: mentions,
        quote_pids: quotes,
        subscription,
        then,
        direct_done: false,
    })
    .context("could not encode the content notification event")?;
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,'notification.content','post',?,?,?,?)")
        .bind(format!("notification.content:{pid}")).bind(pid).bind(payload).bind(timestamp).bind(timestamp)
        .execute(db.observe(&mut **tx)).await?;
    Ok(())
}

/// The paging upper bound of one follow list: the largest follow ID this target had at the time of publication; `None`
/// when nobody follows it.
async fn subscription_page(
    db: &Database,
    tx: &mut Tx<'_>,
    (table, column): (&str, &str),
    kind: Kind,
    target: i64,
) -> Result<Option<Subscription>> {
    let highwater: Option<i64> =
        crate::db::query_scalar(&format!("SELECT MAX(id) FROM {table} WHERE {column}=?"))
            .bind(target)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    Ok(highwater.map(|highwater| Subscription {
        kind,
        target,
        highwater,
        cursor: 0,
    }))
}

pub(crate) struct Source {
    fid: i64,
    tid: i64,
    authorid: Option<i64>,
}

// Discovery is outside the transaction, before account locks. It confers no access.
pub(crate) async fn discover(
    db: &Database,
    pids: &BTreeSet<i64>,
) -> anyhow::Result<BTreeMap<i64, Source>> {
    let mut sources = BTreeMap::new();
    for pid in pids {
        if let Some(row) =
            crate::db::query("SELECT fid,tid,authorid FROM gonof_forum_post WHERE pid=?")
                .bind(pid)
                .fetch_optional(db.executor())
                .await?
        {
            sources.insert(
                *pid,
                Source {
                    fid: row.try_get("fid")?,
                    tid: row.try_get("tid")?,
                    authorid: row.try_get("authorid")?,
                },
            );
        }
    }
    Ok(sources)
}

/// All forums, then all threads, are locked before any post. This keeps cross-thread
/// quoting compatible with publishing, edits, moves and other notification workers.
pub(crate) async fn lock_sources(
    db: &Database,
    tx: &mut Tx<'_>,
    sources: &BTreeMap<i64, Source>,
    viewer_uid: i64,
) -> anyhow::Result<BTreeMap<i64, interaction::Target>> {
    let mut forums: BTreeSet<_> = sources.values().map(|s| s.fid).collect();
    let mut parents = BTreeMap::new();
    for fid in forums.clone() {
        let parent: Option<i64> =
            crate::db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
                .bind(fid)
                .fetch_one(db.observe(&mut **tx))
                .await?;
        forums.extend(parent);
        parents.insert(fid, parent);
    }
    let threads: BTreeMap<_, _> = sources.values().map(|s| (s.tid, s.fid)).collect();
    for fid in forums {
        crate::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
            .bind(fid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    }
    for (fid, expected_parent) in parents {
        let actual: Option<i64> = crate::db::query_scalar(
            "SELECT parent_fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE",
        )
        .bind(fid)
        .fetch_one(db.observe(&mut **tx))
        .await?;
        anyhow::ensure!(
            actual == expected_parent,
            "the forum hierarchy is changing; retrying later"
        );
    }
    for (tid, expected_fid) in threads {
        let fid: i64 =
            crate::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
                .bind(tid)
                .fetch_one(db.observe(&mut **tx))
                .await?;
        anyhow::ensure!(
            fid == expected_fid,
            "the notified content is being moved; retrying later"
        );
    }
    let mut result = BTreeMap::new();
    for (pid, source) in sources {
        match interaction::social::lock_target_for(db, tx, *pid, viewer_uid).await {
            Ok(target) => {
                anyhow::ensure!(
                    target.fid == source.fid && target.tid == source.tid,
                    "the notified content moved to another thread or forum; retrying later"
                );
                result.insert(*pid, target);
            }
            Err(ContentWriteError::NotFound) => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(result)
}

pub(super) async fn deliver(
    db: &Database,
    claim: &Claim,
    now: i64,
) -> anyhow::Result<Option<usize>> {
    let Ok(mut event) = serde_json::from_str::<ContentEvent>(&claim.payload) else {
        fail_claim(db, claim, now, "content notification event is malformed").await?;
        return Ok(None);
    };
    if event.pid != claim.pid
        || event.pid <= 0
        || event.actor_uid <= 0
        || event.recipient_uids.len() > 2
        || event.mention_uids.len() > 50
        || event.quote_pids.len() > 50
        || event
            .recipient_uids
            .iter()
            .chain(&event.mention_uids)
            .any(|uid| *uid <= 0 || *uid == event.actor_uid)
        || event
            .quote_pids
            .iter()
            .any(|pid| *pid <= 0 || *pid == event.pid)
        || event.subscription.iter().chain(&event.then).any(|s| {
            s.storage().is_none()
                || s.target <= 0
                || s.highwater <= 0
                || s.cursor < 0
                || s.cursor > s.highwater
        })
        || event
            .then
            .as_ref()
            .is_some_and(|s| s.kind != Kind::Forum || event.subscription.is_none())
        || claim.event_key != format!("{}:{}", claim.event_type, event.pid)
    {
        fail_claim(
            db,
            claim,
            now,
            "content notification event target is not valid",
        )
        .await?;
        return Ok(None);
    }
    let mut subscribers = Vec::new();
    if let Some(subscription) = &event.subscription {
        let (table, column) = subscription.storage().expect("validated subscription");
        subscribers = crate::db::query(&format!(
            "SELECT id,uid FROM {table} WHERE {column}=? AND id>? AND id<=? ORDER BY id LIMIT 50"
        ))
        .bind(subscription.target)
        .bind(subscription.cursor)
        .bind(subscription.highwater)
        .fetch_all(db.executor())
        .await?;
    }
    let mut ids: BTreeSet<_> = event.quote_pids.iter().copied().collect();
    ids.insert(event.pid);
    let sources = discover(db, &ids).await?;
    let mut accounts: BTreeSet<i64> = event
        .recipient_uids
        .iter()
        .chain(&event.mention_uids)
        .copied()
        .collect();
    accounts.extend(sources.values().filter_map(|s| s.authorid));
    accounts.insert(event.actor_uid);
    for row in &subscribers {
        accounts.insert(row.try_get("uid")?);
    }
    let mut tx = db.begin_write().await?;
    let mut eligible = BTreeSet::new();
    let mut email_hashes = BTreeMap::new();
    for uid in accounts {
        let row = crate::db::query("SELECT `freeze`,account_state,email,emailstatus FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(uid)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
        if let Some(row) = row
            && row.try_get::<i64, _>("freeze")? == 0
            && db::text(&row, "account_state")? == "active"
        {
            eligible.insert(uid);
            if row.try_get::<i64, _>("emailstatus")? == 1
                && let Some(email) = row
                    .try_get::<Option<String>, _>("email")?
                    .filter(|email| !email.is_empty())
            {
                email_hashes.insert(uid, identity::token_hash(&email));
            }
        }
    }
    let targets = lock_sources(db, &mut tx, &sources, event.actor_uid).await?;
    if !current_claim(db, &mut tx, claim, now).await? {
        tx.commit().await?;
        return Ok(None);
    }
    let target = targets.get(&event.pid);
    if !eligible.contains(&event.actor_uid)
        || target.is_none_or(|t| !t.visible || t.authorid != Some(event.actor_uid))
    {
        crate::db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND lease_token=?")
            .bind("the notification target is not accessible")
            .bind(claim.eventid).bind(&claim.lease).execute(db.observe(&mut *tx)).await?;
        tx.commit().await?;
        return Ok(None);
    }
    // Skipped when the recipient has blocked the content's forum (or its parent forum); neither in-site nor e-mail
    // notifications are sent.
    let event_fid = target.map_or(0, |t| t.fid);
    let event_parent_fid: Option<i64> =
        crate::db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
            .bind(event_fid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
    let body = crate::db::query("SELECT message FROM gonof_forum_post WHERE pid=? FOR UPDATE")
        .bind(event.pid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    let current = if event.mention_uids.is_empty() && event.quote_pids.is_empty() {
        markdown::References::default()
    } else {
        markdown::references(&db::text(&body, "message")?)?
    };
    let mut recipients: BTreeMap<i64, BTreeSet<Kind>> = BTreeMap::new();
    if !event.direct_done {
        for uid in &event.mention_uids {
            if current.members.contains(uid) {
                recipients.entry(*uid).or_default().insert(Kind::Mention);
            }
        }
        for pid in &event.quote_pids {
            if current.quotes.contains(pid)
                && let Some(source) = targets.get(pid)
                && source.visible
                && let Some(uid) = source.authorid
            {
                recipients.entry(uid).or_default().insert(Kind::Quote);
            }
        }
        let current_replies = reply_recipients(db, &mut tx, event.pid).await?;
        for uid in &event.recipient_uids {
            if current_replies.contains(uid) {
                recipients.entry(*uid).or_default().insert(Kind::Reply);
            }
        }
    }
    let mut follows = BTreeMap::new();
    if let Some(subscription) = &event.subscription {
        let (table, column) = subscription.storage().expect("validated subscription");
        // The follow is on the new thread's forum or its parent forum.
        if target.is_some_and(|t| match subscription.kind {
            Kind::Forum => {
                t.first
                    && (t.fid == subscription.target
                        || event_parent_fid == Some(subscription.target))
            }
            Kind::Thread => !t.first && t.tid == subscription.target,
            _ => false,
        }) {
            for row in &subscribers {
                let uid: i64 = row.try_get("uid")?;
                let id: i64 = row.try_get("id")?;
                let current: Option<i64> = crate::db::query_scalar(&format!(
                    "SELECT id FROM {table} WHERE uid=? AND {column}=? AND id=?"
                ))
                .bind(uid)
                .bind(subscription.target)
                .bind(id)
                .fetch_optional(db.observe(&mut *tx))
                .await?;
                if current.is_some() {
                    recipients.entry(uid).or_default().insert(subscription.kind);
                    follows.insert(
                        uid,
                        FollowCandidate {
                            kind: subscription.kind,
                            target: subscription.target,
                            id,
                        },
                    );
                }
            }
        }
    }
    recipients.remove(&event.actor_uid);
    let key = format!("notification.content:{}", event.pid);
    let mut inserted = 0;
    for (uid, kinds) in recipients {
        if !eligible.contains(&uid) {
            continue;
        }
        let blocked: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
        )
        .bind(uid)
        .bind(event.actor_uid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        if blocked > 0
            || crate::interaction::forum_filter::blocks_forum_locked(
                db,
                &mut tx,
                uid,
                event_fid,
                event_parent_fid,
            )
            .await?
        {
            continue;
        }
        let preferences = preferences::locked(db, &mut tx, uid).await?;
        if let Some(kind) = preferences.highest(&kinds, false) {
            let exists: i64 = crate::db::query_scalar(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE uid=? AND event_key=?",
            )
            .bind(uid)
            .bind(&key)
            .fetch_one(db.observe(&mut *tx))
            .await?;
            if exists == 0 {
                // The duplicate check and the write both happen under the recipients' member row locks (taken above in
                // uid order), so when the check finds 0 this is certainly a new row; should a duplicate occur anyway,
                // the unique key error rolls back the whole batch for a retry, and unread counts are not inflated.
                super::record::insert(
                    db,
                    &mut tx,
                    &super::record::NewNotice {
                        uid,
                        authorid: event.actor_uid,
                        kind: kind.into(),
                        target: super::record::Target::Post(event.pid),
                        event_key: &key,
                        reason: "",
                        dateline: claim.dateline,
                    },
                )
                .await?;
                inserted += 1;
            }
        }
        if preferences.highest(&kinds, true).is_some()
            && let Some(email_hash) = email_hashes.get(&uid)
        {
            let mail_key = format!("notification.email:{}:{uid}", event.pid);
            // An earlier page of this publication may have queued the member's mail already: a new thread in a child
            // forum reaches the child forum's followers first and then the parent's. That mail takes this page's kinds
            // and follow too, otherwise undoing the first follow before delivery would drop the mail the second one
            // still asks for. A plain read finds it (the earlier page committed it), and only that row is then locked.
            let queued: Option<i64> = crate::db::query_scalar(
                "SELECT eventid FROM gonof_common_outbox WHERE event_key=?",
            )
            .bind(&mail_key)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
            if let Some(eventid) = queued {
                merge_content_mail(db, &mut tx, eventid, &kinds, follows.get(&uid).cloned())
                    .await?;
            } else {
                let quote_pids = event
                    .quote_pids
                    .iter()
                    .copied()
                    .filter(|pid| targets.get(pid).is_some_and(|t| t.authorid == Some(uid)))
                    .collect();
                let payload = ContentMail {
                    pid: event.pid,
                    actor_uid: event.actor_uid,
                    uid,
                    candidates: kinds,
                    quote_pids,
                    follow: follows.get(&uid).cloned(),
                    more_follows: Vec::new(),
                    email_hash: email_hash.clone(),
                };
                crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,'notification.email','member',?,?,?,?) ON DUPLICATE KEY UPDATE eventid=eventid")
                    .bind(mail_key).bind(uid).bind(serde_json::to_string(&payload)?)
                    .bind(claim.dateline).bind(claim.dateline).execute(db.observe(&mut *tx)).await?;
            }
        }
    }
    event.direct_done = true;
    let mut more = if let Some(subscription) = &mut event.subscription {
        if let Some(last) = subscribers.last() {
            subscription.cursor = last.try_get("id")?;
        }
        subscribers.len() == 50 && subscription.cursor < subscription.highwater
    } else {
        false
    };
    // This list is done, but the parent forum's followers remain: switch to the next list and page from the start in
    // the next round.
    if !more && let Some(next) = event.then.take() {
        event.subscription = Some(next);
        more = true;
    }
    // Every page commits its delivery and checkpoint atomically. No transaction reads
    // or locks the full subscription collection, and later subscriptions are excluded.
    // Each written page counts as a success: continuing to the next page resets attempts to zero, so backoff counts only
    // consecutive failures. Otherwise, once a large forum had pushed past page 9, a single transient failure would back
    // off for 2560 seconds.
    let checkpoint = if more {
        "UPDATE gonof_common_outbox SET status='pending',payload=?,attempts=0,lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?"
    } else {
        "UPDATE gonof_common_outbox SET status='sent',payload=?,lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?"
    };
    crate::db::query(checkpoint)
        .bind(serde_json::to_string(&event)?)
        .bind(claim.eventid)
        .bind(&claim.lease)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    Ok(Some(inserted))
}
