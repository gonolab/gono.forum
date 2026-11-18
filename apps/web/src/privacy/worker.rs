use super::*;
use crate::db::Tx;
use crate::identity::rules::AccountState;
use crate::media::Media;
use anyhow::{Context, ensure};

/// Placeholder values after permanent erasure. The attachment file name marks the progress of the batched
/// account-deletion cleanup (`filename<>?` finds those not yet scrubbed), and placeholders also appear in report
/// details and later data exports. System text written to the database follows the site language at the time
/// (docs/architecture.md, "Languages"), so the words come from [`Words`]; after the site language changes, the scrub
/// step only rewrites the already scrubbed names once more.
const ERASED_URL: &str = "about:blank";

/// System text this task writes, in the site language (a background task has no request).
pub(super) struct Words {
    lang: crate::i18n::Lang,
    credit: String,
    site: String,
}

impl Words {
    pub(super) async fn load(
        global: &crate::cache::GlobalCache,
        db: &Database,
    ) -> anyhow::Result<Self> {
        let block = global
            .settings(db)
            .await
            .map_err(|error| anyhow::anyhow!("cannot read the site settings: {error}"))?;
        let site = &block.settings.site;
        let lang = site.lang();
        Ok(Self {
            lang,
            credit: site.credit_word(lang),
            site: site.display_name().to_owned(),
        })
    }
    pub(super) fn lang(&self) -> crate::i18n::Lang {
        self.lang
    }
    pub(super) fn site(&self) -> &str {
        &self.site
    }
    fn erased_file(&self) -> &'static str {
        self.lang.governance_erased_attachment()
    }
    fn erased_text(&self) -> &'static str {
        self.lang.governance_erased_text()
    }
}
/// Task lease. An export may run far longer than this, during which [`Heartbeat`] renews it.
const LEASE_SECONDS: i64 = 900;
/// Why a task that failed for now goes back to the queue (the task's `last_error`; no page shows it).
const RETRY_REASON: &str = "Data processing failed for now and will be retried";
/// The renewal interval is much shorter than the lease: if one or two renewals occasionally fail, there is still time
/// to catch up before the lease expires.
const RENEW_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
pub(super) struct Claim {
    pub eventid: i64,
    pub request_id: i64,
    pub uid: i64,
    pub lease: String,
}
/// The caller checks Redis before invoking a bounded batch, like the other workers.
/// `username_retention_days` is how many days the original username stays reserved after account deletion, shared with
/// the old-username protection period after renaming, `settings.username.history_days`; the worker takes it once
/// per round from the process cache.
pub async fn process_pending(
    db: &Database,
    global: &crate::cache::GlobalCache,
    media: &Media,
    root: &Path,
    local: &Path,
    username_retention_days: i64,
    now: i64,
) -> anyhow::Result<usize> {
    let dir = directory(root, local);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    cleanup_exports(db, &dir, now).await?;
    let mut count = 0;
    for _ in 0..20 {
        // Tasks in one batch run one after another, possibly far apart in time. Leases and failure backoff are computed
        // from the actual claim and failure times; otherwise later tasks would hold an already expired lease the moment
        // they claim it, and backoff would land in the past. Business times such as export validity still use this
        // batch's now.
        let Some(claim) = claim(db, identity::now().max(now)).await? else {
            break;
        };
        let words = Words::load(global, db).await?;
        let result = run(
            db,
            global,
            media,
            &words,
            &claim,
            (
                &dir,
                &root.join(local).join("mail"),
                &crate::sms::local_folder(&root.join(local)),
            ),
            username_retention_days,
            now,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(request_id=claim.request_id,%error,"data request processing did not finish");
            let mut tx = db.begin_write().await?;
            // Lock order remains member -> request -> task even on failure.
            db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
                .bind(claim.uid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
            let r = db::query(
                "SELECT status FROM gonof_common_data_request WHERE id=? AND uid=? FOR UPDATE",
            )
            .bind(claim.request_id)
            .bind(claim.uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
            let changed=db::query("UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND lease_token=? AND status='processing'").bind(identity::now().max(now)+60).bind(RETRY_REASON).bind(claim.eventid).bind(&claim.lease).execute(db.observe(&mut *tx)).await?;
            if changed.rows_affected() == 1
                && !matches!(
                    db::text(&r, "status")?.as_str(),
                    "cancelled" | "completed" | "expired" | "ready"
                )
            {
                // Pages show any non-empty error of a request as this sentence in their own language; the row keeps
                // it in the site language.
                db::query("UPDATE gonof_common_data_request SET status='failed',last_error=?,version=version+1 WHERE id=?").bind(words.lang().governance_data_failed()).bind(claim.request_id).execute(db.observe(&mut *tx)).await?;
            }
            tx.commit().await?;
        } else {
            count += 1;
        }
    }
    Ok(count)
}
async fn claim(db: &Database, now: i64) -> anyhow::Result<Option<Claim>> {
    let mut tx = db.begin_write().await?;
    let row=db::query("SELECT eventid,aggregate_id,payload FROM gonof_common_outbox WHERE event_type IN ('privacy.export','privacy.erase') AND status IN ('pending','processing') AND available_at<=? AND lease_until<=? ORDER BY eventid LIMIT 1 FOR UPDATE").bind(now).bind(now).fetch_optional(db.observe(&mut *tx)).await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let eventid = row.try_get("eventid")?;
    let request_id = row.try_get("aggregate_id")?;
    let payload: serde_json::Value = serde_json::from_str(&db::text(&row, "payload")?)?;
    let uid = payload
        .get("uid")
        .and_then(|v| v.as_i64())
        .context("data request owner missing")?;
    let lease = identity::random_token();
    db::query("UPDATE gonof_common_outbox SET status='processing',attempts=attempts+1,lease_token=?,lease_until=? WHERE eventid=?").bind(&lease).bind(now+LEASE_SECONDS).bind(eventid).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(Some(Claim {
        eventid,
        request_id,
        uid,
        lease,
    }))
}
pub(super) async fn locked(
    db: &Database,
    tx: &mut Tx<'_>,
    c: &Claim,
    now: i64,
) -> anyhow::Result<DataRequest> {
    db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(c.uid)
        .fetch_one(db.observe(&mut **tx))
        .await?;
    let row = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_data_request WHERE id=? AND uid=? FOR UPDATE"
    ))
    .bind(c.request_id)
    .bind(c.uid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    let task=db::query("SELECT aggregate_id,event_key,event_type,status,lease_token,lease_until FROM gonof_common_outbox WHERE eventid=? FOR UPDATE").bind(c.eventid).fetch_one(db.observe(&mut **tx)).await?;
    let r = decode(&row)?;
    ensure!(
        task.try_get::<i64, _>("aggregate_id")? == r.id
            && db::text(&task, "event_key")? == format!("privacy.request:{}", r.id)
            && db::text(&task, "event_type")?
                == if r.kind == "export" {
                    "privacy.export"
                } else {
                    "privacy.erase"
                },
        "privacy task identity mismatch"
    );
    ensure!(
        db::text(&task, "status")? == "processing"
            && task.try_get::<Option<String>, _>("lease_token")?.as_deref()
                == Some(c.lease.as_str())
            && task.try_get::<i64, _>("lease_until")? > now,
        "privacy task lease expired"
    );
    Ok(r)
}
/// Renews the lease in the background during an export. Stops once the lease is taken back (claimed by another process
/// after a timeout, task finished or revoked), and the [`locked`] check at the end of the export voids this output; a
/// temporary database error is only logged and retried next round.
struct Heartbeat {
    cancel: tokio_util::sync::CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Heartbeat {
    fn start(db: &Database, c: &Claim, floor: i64, every: std::time::Duration) -> Self {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (db, c, cancelled) = (db.clone(), c.clone(), cancel.clone());
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = cancelled.cancelled() => return,
                    () = tokio::time::sleep(every) => {}
                }
                match renew(&db, &c, identity::now().max(floor)).await {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(error) => {
                        tracing::warn!(request_id=c.request_id,%error,"data request lease renewal failed; retrying later")
                    }
                }
            }
        });
        Self {
            cancel,
            task: Some(task),
        }
    }
    /// Waits for the renewal in progress to finish writing before returning; after that no renewal reaches the
    /// database.
    async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Heartbeat {
    // Must also stop when the outer future is dropped; otherwise a task nobody is working on would keep being renewed.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
async fn renew(db: &Database, c: &Claim, now: i64) -> anyhow::Result<bool> {
    let updated=db::query("UPDATE gonof_common_outbox SET lease_until=? WHERE eventid=? AND lease_token=? AND status='processing'").bind(now+LEASE_SECONDS).bind(c.eventid).bind(&c.lease).execute(db.executor()).await?;
    Ok(updated.rows_affected() == 1)
}
pub(super) async fn finish_task(
    db: &Database,
    tx: &mut Tx<'_>,
    c: &Claim,
    complete: bool,
    now: i64,
) -> anyhow::Result<()> {
    let updated=db::query("UPDATE gonof_common_outbox SET status=?,available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND lease_token=? AND status='processing'").bind(if complete {"sent"} else {"pending"}).bind(now).bind(c.eventid).bind(&c.lease).execute(db.observe(&mut **tx)).await?;
    ensure!(updated.rows_affected() == 1, "privacy lease lost");
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn run(
    db: &Database,
    global: &crate::cache::GlobalCache,
    media: &Media,
    words: &Words,
    c: &Claim,
    (dir, mail_dir, sms_dir): (&Path, &Path, &Path),
    username_retention_days: i64,
    now: i64,
) -> anyhow::Result<()> {
    let mut tx = db.begin_content_write().await?;
    // Closing eventually renames an account. Acquire the same namespace lock as registration first.
    db::query(
        "SELECT skey FROM gonof_common_settings WHERE skey='identity.username_namespace' FOR UPDATE",
    )
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let mut r = locked(db, &mut tx, c, identity::now().max(now)).await?;
    if matches!(
        r.status.as_str(),
        "cancelled" | "completed" | "expired" | "ready"
    ) {
        finish_task(db, &mut tx, c, true, now).await?;
        tx.commit().await?;
        return Ok(());
    }
    // Woken before the cooling period ends (a generic retry, a clock step): the request is not failing, so it only
    // waits again until it is due, and the member keeps seeing it as cooling.
    if r.execute_after > now {
        finish_task(db, &mut tx, c, false, r.execute_after).await?;
        tx.commit().await?;
        return Ok(());
    }
    if r.kind == "export" {
        ensure!(
            active_member(db, &mut tx, c.uid).await?,
            "account no longer active"
        );
        db::query("UPDATE gonof_common_data_request SET status='processing',stage='export',last_error='',version=version+1 WHERE id=?").bind(r.id).execute(db.observe(&mut *tx)).await?;
        tx.commit().await?;
        let heartbeat = Heartbeat::start(db, c, now, RENEW_EVERY);
        let built = export::build(db, media, c, dir, words, now).await;
        heartbeat.stop().await;
        return built;
    }
    if r.stage == "start" {
        settled(db, &mut tx, c.uid).await?;
        if r.kind == "close" {
            let admin: i64 =
                db::query_scalar("SELECT adminid FROM gonof_common_member WHERE uid=?")
                    .bind(c.uid)
                    .fetch_one(db.observe(&mut *tx))
                    .await?;
            ensure!(admin != 1, "administrator cannot close their account");
            // Account state only moves forward (guaranteed here, not by the schema): it only goes from active to
            // closing, with the value bound via `AccountState`; when nothing matches (already closing or closed) it
            // rolls back and the failure path records it.
            let entered = db::query("UPDATE gonof_common_member SET account_state=?,credentials_version=credentials_version+1 WHERE uid=? AND account_state=?")
                .bind(AccountState::Deleting.value())
                .bind(c.uid)
                .bind(AccountState::Active.value())
                .execute(db.observe(&mut *tx))
                .await?
                .rows_affected();
            ensure!(
                entered == 1,
                "account is not active and cannot enter closing"
            );
            db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND consumed_at IS NULL AND revoked_at IS NULL").bind(now).bind(c.uid).execute(db.observe(&mut *tx)).await?;
            let latest = db::query_scalar::<Option<i64>>(
                "SELECT MAX(pid) FROM gonof_forum_post WHERE authorid=?",
            )
            .bind(c.uid)
            .fetch_one(db.observe(&mut *tx))
            .await?
            .unwrap_or(0);
            r.upper_pid = r.upper_pid.max(latest);
        }
        db::query("UPDATE gonof_common_data_request SET status='expired',finished_at=?,version=version+1 WHERE uid=? AND kind='export' AND status<>'expired'").bind(now).bind(c.uid).execute(db.observe(&mut *tx)).await?;
        // The upper bound only grows (guaranteed here, not by the schema): the MAX(pid) re-read for account deletion
        // would never be below the value at submission (posts are not physically deleted and authors do not change);
        // taking the larger one stops this from depending on invariants elsewhere, and the in-memory value matches
        // what is written.
        db::query("UPDATE gonof_common_data_request SET status='processing',stage='posts',upper_pid=GREATEST(upper_pid,?),last_error='',version=version+1 WHERE id=?").bind(r.upper_pid).bind(r.id).execute(db.observe(&mut *tx)).await?;
        if r.kind == "close" {
            // Once account deletion starts, the anonymous home page must drop this member immediately. Commit right
            // after locking the site-wide version; post cleanup continues from the next claim.
            finish_task(db, &mut tx, c, false, now).await?;
            crate::content::copies::bump_site(db, &mut tx).await?;
            tx.commit().await?;
            return Ok(());
        }
        r.stage = "posts".into();
    }
    if r.stage == "posts" && (r.kind == "erase" || r.content_mode == "erase") {
        let row=db::query("SELECT pid,tid,fid,root_pid,first,erased_at FROM gonof_forum_post WHERE authorid=? AND pid>? AND pid<=? ORDER BY pid LIMIT 1").bind(c.uid).bind(r.cursor_pid).bind(r.upper_pid).fetch_optional(db.observe(&mut *tx)).await?;
        if let Some(p) = row {
            let pid: i64 = p.try_get("pid")?;
            let tid: i64 = p.try_get("tid")?;
            db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
                .bind(p.try_get::<i64, _>("fid")?)
                .fetch_one(db.observe(&mut *tx))
                .await?;
            db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
                .bind(tid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
            if let Some(parent) = p.try_get::<Option<i64>, _>("root_pid")? {
                db::query("SELECT pid FROM gonof_forum_post WHERE pid=? FOR UPDATE")
                    .bind(parent)
                    .fetch_one(db.observe(&mut *tx))
                    .await?;
            }
            let locked = db::query(
                "SELECT invisible,deleted_at,erased_at FROM gonof_forum_post WHERE pid=? AND authorid=? FOR UPDATE",
            )
            .bind(pid)
            .bind(c.uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
            let first = p.try_get::<i64, _>("first")? == 1;
            // Erasure works like withdrawal: posts still counted under the member are subtracted from the member
            // counts.
            if locked.try_get::<i64, _>("erased_at")? == 0
                && crate::content::copies::counted_post(
                    locked.try_get("invisible")?,
                    locked.try_get("deleted_at")?,
                )
            {
                let digest = crate::content::copies::ListedThread::load(db, &mut tx, tid)
                    .await?
                    .digest;
                crate::content::copies::post_counted_changed(
                    db,
                    &mut tx,
                    (Some(c.uid), first, digest),
                    -1,
                )
                .await?;
            }
            let aids: Vec<i64> = db::query_scalar(
                "SELECT aid FROM gonof_forum_attachment_reference WHERE pid=? ORDER BY aid",
            )
            .bind(pid)
            .fetch_all(db.observe(&mut *tx))
            .await?;
            for aid in &aids {
                db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
                    .bind(*aid)
                    .fetch_one(db.observe(&mut *tx))
                    .await?;
            }
            db::query("DELETE FROM gonof_forum_attachment_reference WHERE pid=?")
                .bind(pid)
                .execute(db.observe(&mut *tx))
                .await?;
            db::query("DELETE FROM gonof_forum_post_revision WHERE pid=?")
                .bind(pid)
                .execute(db.observe(&mut *tx))
                .await?;
            db::query("UPDATE gonof_forum_post SET message='',deleted_at=?,erased_at=?,version=version+1 WHERE pid=? AND authorid=? AND erased_at=0").bind(now).bind(now).bind(pid).bind(c.uid).execute(db.observe(&mut *tx)).await?;
            // Erasure works like withdrawal: the growth points this post brought are taken back.
            crate::growth::sync_posts(db, &mut tx, &[pid], crate::growth::Cause::Revoke, now)
                .await?;
            if first {
                db::query("UPDATE gonof_forum_thread SET subject=?,deleted_at=?,version=version+1,cache_revision=cache_revision+1 WHERE tid=? AND authorid=?").bind(words.lang().governance_erased_subject()).bind(now).bind(tid).bind(c.uid).execute(db.observe(&mut *tx)).await?;
                // The first post's attachment references were just released and will be cleaned up next, so the list
                // thumbnails are deleted too.
                db::query("DELETE FROM gonof_forum_thread_image WHERE tid=?")
                    .bind(tid)
                    .execute(db.observe(&mut *tx))
                    .await?;
                // Poll options and news sources are part of the first post's body, and the value rules require them to
                // be non-empty (guaranteed in code), so they are replaced with placeholder text.
                // The placeholder text is built in code by option number and written back option by option (there are
                // at most a few dozen options).
                let options: Vec<i64> = db::query_scalar(
                    "SELECT option_no FROM gonof_forum_poll_option WHERE tid=? ORDER BY option_no",
                )
                .bind(tid)
                .fetch_all(db.observe(&mut *tx))
                .await?;
                for option in options {
                    db::query(
                        "UPDATE gonof_forum_poll_option SET label=? WHERE tid=? AND option_no=?",
                    )
                    .bind(words.lang().governance_erased_option(option.to_string()))
                    .bind(tid)
                    .bind(option)
                    .execute(db.observe(&mut *tx))
                    .await?;
                }
                db::query("UPDATE gonof_forum_news SET source_url=?,source_name=?,source_author=? WHERE tid=?")
                    .bind(ERASED_URL)
                    .bind(words.erased_text())
                    .bind(words.erased_text())
                    .bind(tid)
                    .execute(db.observe(&mut *tx))
                    .await?;
                // A lottery's prizes, entry conditions and removal reasons are the author's text too (the export has
                // them, `export.rs`). Prize names must be non-empty (guaranteed in code), so each becomes a numbered
                // placeholder, written prize by prize (at most `MAX_PRIZES`); notes and conditions are optional and
                // are cleared. Winners keep their awards and see the placeholder in their prize list.
                let prizes: Vec<i64> = db::query_scalar(
                    "SELECT prize_no FROM gonof_forum_lottery_prize WHERE tid=? ORDER BY prize_no",
                )
                .bind(tid)
                .fetch_all(db.observe(&mut *tx))
                .await?;
                for prize in prizes {
                    db::query(
                        "UPDATE gonof_forum_lottery_prize SET name=?,note='' WHERE tid=? AND prize_no=?",
                    )
                    .bind(words.lang().governance_erased_prize(prize.to_string()))
                    .bind(tid)
                    .bind(prize)
                    .execute(db.observe(&mut *tx))
                    .await?;
                }
                db::query("UPDATE gonof_forum_lottery SET requirements=NULL WHERE tid=?")
                    .bind(tid)
                    .execute(db.observe(&mut *tx))
                    .await?;
                // A removal reason must be non-empty, and the removed entrant's card shows it as the sign of the
                // removal, so it becomes the placeholder text rather than empty.
                db::query("UPDATE gonof_forum_lottery_exclusion SET reason=? WHERE tid=?")
                    .bind(words.erased_text())
                    .bind(tid)
                    .execute(db.observe(&mut *tx))
                    .await?;
            }
            for aid in aids {
                purge_if_unused(db, &mut tx, aid, words, now).await?;
            }
            crate::content::refresh_effective_replies(db, &mut tx, tid).await?;
            db::query("UPDATE gonof_common_data_request SET cursor_pid=GREATEST(cursor_pid,?),version=version+1,last_error='',status='processing' WHERE id=?").bind(pid).bind(r.id).execute(db.observe(&mut *tx)).await?;
            finish_task(db, &mut tx, c, false, now).await?;
            // Erasure changed the title, effective replies or last reply, so the anonymous cache is invalidated
            // immediately at commit.
            crate::content::copies::bump_site(db, &mut tx).await?;
            tx.commit().await?;
            return Ok(());
        }
    }
    if r.stage == "posts" {
        db::query(
            "UPDATE gonof_common_data_request SET stage='cleanup',version=version+1 WHERE id=?",
        )
        .bind(r.id)
        .execute(db.observe(&mut *tx))
        .await?;
        r.stage = "cleanup".into();
    }
    if r.kind == "close" && r.stage == "cleanup" {
        // Only non-content ownership is cleared; governance and immutable balances keep stable UID references.
        // Cleanup commits in batches and every batch reaches this point; the avatar is only cleared while it is still
        // there. This step commits on its own, and later batches clean up attachments. Thread rows carry no avatar
        // copy (list avatars are addressed by uid), so threads need no rewrite.
        let avatar = db::query("SELECT avatar,avatar_aid FROM gonof_common_member WHERE uid=?")
            .bind(c.uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
        if avatar.try_get::<i64, _>("avatar")? != 0
            || avatar.try_get::<Option<i64>, _>("avatar_aid")?.is_some()
        {
            db::query("UPDATE gonof_common_member SET avatar_aid=NULL,avatar=0 WHERE uid=?")
                .bind(c.uid)
                .execute(db.observe(&mut *tx))
                .await?;
            finish_task(db, &mut tx, c, false, now).await?;
            tx.commit().await?;
            return Ok(());
        }
        let aids:Vec<i64>=db::query_scalar("SELECT a.aid FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND d.lifecycle NOT IN ('purging','purged') AND NOT EXISTS(SELECT 1 FROM gonof_forum_attachment_reference r WHERE r.aid=a.aid) AND NOT EXISTS(SELECT 1 FROM gonof_forum_forum f WHERE f.icon_aid=a.aid) AND NOT EXISTS(SELECT 1 FROM gonof_common_site_asset s WHERE s.aid=a.aid) AND NOT EXISTS(SELECT 1 FROM gonof_common_site_domain sd WHERE sd.logo_aid=a.aid OR sd.favicon_aid=a.aid) ORDER BY a.aid LIMIT 20").bind(c.uid).fetch_all(db.observe(&mut *tx)).await?;
        for aid in &aids {
            db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
                .bind(*aid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
            purge_if_unused(db, &mut tx, *aid, words, now).await?;
        }
        if !aids.is_empty() {
            finish_task(db, &mut tx, c, false, now).await?;
            tx.commit().await?;
            return Ok(());
        }
        // The member's own attachments that entered cleanup earlier do not go through the loop above; their metadata is
        // scrubbed here in batches.
        let aids:Vec<i64>=db::query_scalar("SELECT a.aid FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND d.lifecycle IN ('purging','purged') AND d.filename<>? ORDER BY a.aid LIMIT 20").bind(c.uid).bind(words.erased_file()).fetch_all(db.observe(&mut *tx)).await?;
        for aid in &aids {
            db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
                .bind(*aid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
            scrub_attachment(db, &mut tx, *aid, words).await?;
        }
        if !aids.is_empty() {
            finish_task(db, &mut tx, c, false, now).await?;
            tx.commit().await?;
            return Ok(());
        }
    }
    // Keep the task lease while doing external deletion; no database locks span object I/O.
    tx.commit().await?;
    cleanup_exports(db, dir, now).await?;
    let pending:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND d.lifecycle='purging'").bind(c.uid).fetch_one(db.executor()).await?;
    let exports:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_common_data_request WHERE uid=? AND kind='export' AND artifact_key IS NOT NULL").bind(c.uid).fetch_one(db.executor()).await?;
    if r.kind == "close" {
        cleanup_mail(db, c.uid, mail_dir).await?;
        cleanup_mail(db, c.uid, &mail_dir.join("prepared")).await?;
        cleanup_sms(db, c.uid, sms_dir).await?;
        // MEMORY activity is not transactional; deleting is safe and replayable once
        // the persistent account state prevents any new activity UPSERT.
        db::query("DELETE FROM gonof_common_session WHERE uid=?")
            .bind(c.uid)
            .execute(db.executor())
            .await?;
    }
    // Logins are already revoked, so the account can no longer load a new member page frame. This external cleanup is
    // done first, and only after it succeeds is the task marked complete; when Redis is temporarily unavailable the
    // existing lease and retry flow takes over, without holding MySQL locks while waiting on the network.
    if r.kind == "close" && pending == 0 && exports == 0 {
        crate::cache::member::clear(global.redis(), c.uid)
            .await
            .context(
                "cannot clear the member display preference cache before closing the account",
            )?;
    }
    let mut tx = db.begin_content_write().await?;
    db::query(
        "SELECT skey FROM gonof_common_settings WHERE skey='identity.username_namespace' FOR UPDATE",
    )
    .fetch_one(db.observe(&mut *tx))
    .await?;
    let current = locked(db, &mut tx, c, identity::now().max(now)).await?;
    ensure!(current.stage == "cleanup", "privacy cleanup stage changed");
    if pending > 0 || exports > 0 {
        finish_task(db, &mut tx, c, false, now + 2).await?;
        tx.commit().await?;
        return Ok(());
    }
    if r.kind == "close" {
        close_account(
            db,
            &mut tx,
            (c.uid, r.id),
            username_retention_days,
            words,
            now,
        )
        .await?;
    }
    db::query("UPDATE gonof_common_data_request SET status='completed',stage='done',finished_at=?,last_error='',version=version+1 WHERE id=? AND uid=?").bind(now).bind(r.id).bind(c.uid).execute(db.observe(&mut *tx)).await?;
    audit(
        db,
        &mut tx,
        c.uid,
        "privacy.complete",
        serde_json::json!({"request_id":r.id,"kind":r.kind,"content_mode":r.content_mode}),
        now,
    )
    .await?;
    finish_task(db, &mut tx, c, true, now).await?;
    tx.commit().await?;
    if r.kind == "close" {
        // Account deletion removed moderator rows and changed the username: after commit, clear the moderator roster
        // and broadcast instead of waiting for the lifetime to pass.
        // A broadcast failure is only logged: the task is already complete and there is no request to answer, so other
        // processes see it after at most one lifetime.
        if let Err(error) = global.announce(&[crate::cache::Block::Moderators]).await {
            tracing::warn!(uid = c.uid, %error, "account deletion finished, but the moderator roster invalidation broadcast was not sent");
        }
    }
    Ok(())
}
async fn close_account(
    db: &Database,
    tx: &mut Tx<'_>,
    (uid, id): (i64, i64),
    username_retention_days: i64,
    words: &Words,
    now: i64,
) -> anyhow::Result<()> {
    settled(db, tx, uid).await?;
    let member = db::query(
        "SELECT username,account_state,adminid,mobile FROM gonof_common_member WHERE uid=?",
    )
    .bind(uid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    ensure!(
        db::text(&member, "account_state")? == "deleting"
            && member.try_get::<i64, _>("adminid")? != 1,
        "account cannot be closed"
    );
    let username = db::text(&member, "username")?;
    // The mobile number is cleared on account deletion (the final UPDATE), and verification code rows sent to this
    // number that have not been cleaned up yet are deleted too; their local files went before this transaction
    // (`cleanup_sms`).
    let mobile: Option<String> = member.try_get("mobile")?;
    if let Some(mobile) = &mobile {
        db::query("DELETE FROM gonof_common_sms_code WHERE mobile=?")
            .bind(mobile)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    // Third-party links are deleted on account deletion: signing in later with the same
    // Google or GitHub account counts as a new registration.
    db::query("DELETE FROM gonof_common_member_external WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    let reserved_until = now + username_retention_days.saturating_mul(86_400);
    crate::identity::rules::check_username_history(&username, now, reserved_until)?;
    db::query("INSERT INTO gonof_common_member_username_history(username,uid,dateline,expires_at) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE expires_at=GREATEST(expires_at,VALUES(expires_at))").bind(username).bind(uid).bind(now).bind(reserved_until).execute(db.observe(&mut **tx)).await?;
    db::query(
        "INSERT INTO gonof_common_credit_account(uid) VALUES(?) ON DUPLICATE KEY UPDATE uid=uid",
    )
    .bind(uid)
    .execute(db.observe(&mut **tx))
    .await?;
    let available: i64 = db::query_scalar(
        "SELECT available FROM gonof_common_credit_account WHERE uid=? FOR UPDATE",
    )
    .bind(uid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    db::query("INSERT INTO gonof_common_credit_ledger(event_key,uid,available_delta,frozen_delta,available_after,frozen_after,kind,reason,dateline) VALUES(?,?,?,0,0,0,'privacy_close',?,?)").bind(format!("privacy.close:{id}")).bind(uid).bind(-available).bind(words.lang().governance_close_ledger(&words.credit)).bind(now).execute(db.observe(&mut **tx)).await?;
    // The ledger entry above records the balance as 0/0, so the account must really be zeroed under the same condition
    // (the frozen amount was prechecked as 0 by `settled`); the schema does not check the two against each other, so a
    // mismatch rolls back rather than leaving the ledger and account inconsistent.
    let cleared = db::query("UPDATE gonof_common_credit_account SET available=0,version=version+1 WHERE uid=? AND frozen=0").bind(uid).execute(db.observe(&mut **tx)).await?;
    ensure!(
        cleared.rows_affected() == 1,
        "frozen points were not 0 when zeroing the balance on account closure"
    );
    // Likes, favorites and downvotes the member gave are kept, and counts and growth points stay untouched: their
    // number is unbounded, and each would need to lock the thread, decrement counts and recompute growth points; doing
    // that in this transaction would queue all site writes on SQLite. Removing them would need a separate batched
    // cleanup.
    // The member's growth account, per-post growth rows and ledger are kept (historical books are not deleted); they
    // just no longer appear on leaderboards.
    crate::ranks::forget_member(db, tx, uid).await?;
    // Verification is deleted, pending friend-link applications are changed to withdrawn, and displayed links are kept.
    crate::directory::forget_member(db, tx, uid, now).await?;
    // Honor group grants are deleted on account deletion (docs/roles.md, "Honor groups").
    crate::honors::forget_member(db, tx, uid).await?;
    for sql in [
        "DELETE FROM gonof_forum_moderator WHERE uid=?",
        "DELETE FROM gonof_common_member_token WHERE uid=?",
        "DELETE FROM gonof_home_thread_follow WHERE uid=?",
        "DELETE FROM gonof_home_forum_follow WHERE uid=?",
        "DELETE FROM gonof_home_member_block WHERE uid=?",
        // Member follows are deleted in both directions: the people this member follows, and others following this
        // member.
        "DELETE FROM gonof_home_member_follow WHERE uid=?",
        "DELETE FROM gonof_home_member_follow WHERE followed_uid=?",
        "DELETE FROM gonof_home_forum_filter WHERE uid=?",
        "DELETE FROM gonof_home_feed_token WHERE uid=?",
        "DELETE FROM gonof_common_member_recovery_code WHERE uid=?",
        "DELETE FROM gonof_common_member_two_factor WHERE uid=?",
        "DELETE FROM gonof_home_interaction_request WHERE uid=?",
        "DELETE FROM gonof_home_notification_preference WHERE uid=?",
        "DELETE FROM gonof_home_reading_preference WHERE uid=?",
        "DELETE FROM gonof_common_member_presence WHERE uid=?",
    ] {
        db::query(sql)
            .bind(uid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    // Notifications are deleted in one batch and the unread count drops to zero with them; only through this one entry
    // point can counts and details never disagree.
    crate::notifications::record::delete_all(db, tx, uid).await?;
    db::query("UPDATE gonof_common_invite SET status='revoked' WHERE uid=? AND status IN ('available','frozen')").bind(uid).execute(db.observe(&mut **tx)).await?;
    // API token rows keep their history but are revoked with the account, and the last-used IP is cleared.
    crate::identity::rules::revoke_api_keys(db, tx, uid, Some(uid), now).await?;
    db::query("UPDATE gonof_common_member_api_key SET last_used_ip='' WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    db::query("UPDATE gonof_common_outbox SET payload='{}',status=IF(status='sent','sent','failed'),lease_token=NULL,lease_until=0,last_error=NULL WHERE aggregate_type='member' AND aggregate_id=?").bind(uid).execute(db.observe(&mut **tx)).await?;
    db::query("DELETE FROM gonof_common_log WHERE uid=? AND type LIKE 'identity.%' AND type<>'identity.role.assign'").bind(uid).execute(db.observe(&mut **tx)).await?;
    // Registration sources are deleted together with the registration log; the admin registration-source
    // statistics keep the same rules.
    db::query("DELETE FROM gonof_common_member_register WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    // Generated pseudonyms cannot take an existing username; the namespace mutex serializes registration.
    let anonymous = loop {
        let candidate = format!("deleted-{uid}-{}", &identity::random_token()[..12]);
        let used: i64 =
            db::query_scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username=?")
                .bind(&candidate)
                .fetch_one(db.observe(&mut **tx))
                .await?;
        let protected: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_username_history WHERE username=? AND expires_at>?")
            .bind(&candidate).bind(now).fetch_one(db.observe(&mut **tx)).await?;
        if used == 0 && protected == 0 {
            break candidate;
        }
    };
    // The only statement that enters closed (irreversible): the same statement clears email, password, avatar, bio and
    // management identity, entering only from closing.
    let closed = db::query("UPDATE gonof_common_member SET username=?,email=NULL,mobile=NULL,password=NULL,bio='',avatar=0,avatar_aid=NULL,emailstatus=0,adminid=0,space_visibility=?,account_state=?,credentials_version=credentials_version+1 WHERE uid=? AND account_state=?")
        .bind(&anonymous)
        .bind(crate::identity::SpaceVisibility::Private.value())
        .bind(AccountState::Deleted.value())
        .bind(uid)
        .bind(AccountState::Deleting.value())
        .execute(db.observe(&mut **tx))
        .await?
        .rows_affected();
    ensure!(
        closed == 1,
        "account is not closing and cannot be marked deleted"
    );
    // The author copies on threads are replaced with the pseudonym as they are (lists show "Deleted user" by account
    // state); for threads ending with this member's reply, the last replier is written directly as "Deleted user",
    // and the site-wide version goes up by one.
    crate::content::copies::member_changed(db, tx, uid).await?;
    Ok(())
}

async fn purge_if_unused(
    db: &Database,
    tx: &mut Tx<'_>,
    aid: i64,
    words: &Words,
    now: i64,
) -> anyhow::Result<()> {
    let refs:i64=db::query_scalar("SELECT (SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=?) + (SELECT COUNT(*) FROM gonof_common_member WHERE avatar_aid=?) + (SELECT COUNT(*) FROM gonof_forum_forum WHERE icon_aid=?) + (SELECT COUNT(*) FROM gonof_common_site_asset WHERE aid=?) + (SELECT COUNT(*) FROM gonof_common_site_domain WHERE logo_aid=? OR favicon_aid=?)").bind(aid).bind(aid).bind(aid).bind(aid).bind(aid).bind(aid).fetch_one(db.observe(&mut **tx)).await?;
    if refs == 0 {
        db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purging',purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle<>'purged'").bind(now).bind(now).bind(aid).execute(db.observe(&mut **tx)).await?;
        db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,'media.purge','attachment',?,'{}',?,?) ON DUPLICATE KEY UPDATE status=IF(status='sent','pending',status),available_at=GREATEST(dateline,VALUES(available_at))").bind(format!("media.purge:{aid}")).bind(aid).bind(now).bind(now).execute(db.observe(&mut **tx)).await?;
        scrub_attachment(db, tx, aid, words).await?;
    }
    Ok(())
}
/// Permanent erasure keeps no attachment file name, description or alt text; the file name is replaced with placeholder
/// text ([`Words`]). Storage location, size and digest are kept for cleanup tasks and reconciliation.
async fn scrub_attachment(
    db: &Database,
    tx: &mut Tx<'_>,
    aid: i64,
    words: &Words,
) -> anyhow::Result<()> {
    db::query(
        "UPDATE gonof_forum_attachment_0 SET filename=?,description='',alt_text='' WHERE aid=?",
    )
    .bind(words.erased_file())
    .bind(aid)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}
async fn cleanup_mail(db: &Database, uid: i64, mail_dir: &Path) -> anyhow::Result<()> {
    let mut before = 0;
    loop {
        let ids:Vec<i64>=db::query_scalar("SELECT eventid FROM gonof_common_outbox WHERE aggregate_type='member' AND aggregate_id=? AND eventid>? ORDER BY eventid LIMIT 100").bind(uid).bind(before).fetch_all(db.executor()).await?;
        if ids.is_empty() {
            break;
        }
        for eventid in &ids {
            let path = mail_dir.join(format!("{eventid}.eml"));
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        before = *ids.last().unwrap();
    }
    // Crash remnants can include an unpublished temporary message file.
    let mut files = match tokio::fs::read_dir(mail_dir).await {
        Ok(files) => files,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = files.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(parts) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) else {
            continue;
        };
        let Some((id, lease)) = parts.split_once('.') else {
            continue;
        };
        let Ok(id) = id.parse::<i64>() else {
            continue;
        };
        if !valid_key(lease) {
            continue;
        }
        let owned: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE eventid=? AND aggregate_type='member' AND aggregate_id=?")
            .bind(id).bind(uid).fetch_one(db.executor()).await?;
        if owned == 1 {
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

/// Removes the local SMS files (`sms/{id}.txt`, [`crate::sms::local_file`]) of the codes sent to the member's number:
/// they hold the number and the code in plain text, and `close_account` then deletes the code rows that lead to
/// them. A code sent to the number after this step leaves a file that the retention cleanup sweeps up once it is past
/// the code retention period ([`crate::maintenance`]).
async fn cleanup_sms(db: &Database, uid: i64, sms_dir: &Path) -> anyhow::Result<()> {
    let mobile: Option<String> =
        db::query_scalar("SELECT mobile FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await?;
    let Some(mobile) = mobile else {
        return Ok(());
    };
    let mut after = 0;
    loop {
        let ids: Vec<i64> = db::query_scalar(
            "SELECT id FROM gonof_common_sms_code WHERE mobile=? AND id>? ORDER BY id LIMIT 100",
        )
        .bind(&mobile)
        .bind(after)
        .fetch_all(db.executor())
        .await?;
        let Some(&last) = ids.last() else {
            break;
        };
        for id in ids {
            match tokio::fs::remove_file(crate::sms::local_file(sms_dir, id)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        after = last;
    }
    Ok(())
}

async fn cleanup_exports(db: &Database, dir: &Path, now: i64) -> anyhow::Result<()> {
    let rows=db::query("SELECT id,uid,artifact_key FROM gonof_common_data_request WHERE artifact_key IS NOT NULL AND (status='expired' OR artifact_expires_at<=?) ORDER BY id LIMIT 100").bind(now).fetch_all(db.executor()).await?;
    for r in rows {
        let key = db::text(&r, "artifact_key")?;
        ensure!(safe_file(&key), "invalid private artifact key");
        match tokio::fs::remove_dir_all(dir.join(&key)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        db::query("UPDATE gonof_common_data_request SET status='expired',artifact_key=NULL,artifact_hash=NULL,artifact_bytes=NULL,artifact_expires_at=NULL,finished_at=?,version=version+1 WHERE id=? AND artifact_key=?").bind(now).bind(r.try_get::<i64,_>("id")?).bind(key).execute(db.executor()).await?;
    }
    // An expired lease or crash must not leave an unregistered archive containing erased data.
    let mut directories = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = directories.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(parts) = name.strip_prefix("request-") else {
            continue;
        };
        let Some((id, lease)) = parts.split_once('-') else {
            continue;
        };
        let Ok(id) = id.parse::<i64>() else {
            continue;
        };
        if !valid_key(lease) {
            continue;
        }
        // The event key is built in code before binding (not with CONCAT in SQL), so both backends run the same
        // statement.
        let row=db::query("SELECT r.status,r.artifact_key,o.lease_token,o.lease_until FROM gonof_common_data_request r LEFT JOIN gonof_common_outbox o ON o.event_key=? WHERE r.id=?")
            .bind(format!("privacy.request:{id}"))
            .bind(id).fetch_optional(db.executor()).await?;
        let keep = if let Some(r) = row {
            let state = db::text(&r, "status")?;
            (state == "ready"
                && r.try_get::<Option<String>, _>("artifact_key")?.as_deref()
                    == Some(name.as_str()))
                || (state == "processing"
                    && r.try_get::<Option<String>, _>("lease_token")?.as_deref() == Some(lease)
                    && r.try_get::<Option<i64>, _>("lease_until")?
                        .is_some_and(|t| t > now))
        } else {
            false
        };
        if !keep {
            match tokio::fs::remove_dir_all(entry.path()).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn lease_until(db: &Database, eventid: i64) -> i64 {
        db::query_scalar("SELECT lease_until FROM gonof_common_outbox WHERE eventid=?")
            .bind(eventid)
            .fetch_one(db.executor())
            .await
            .unwrap()
    }

    // An export may outlast one lease period: the heartbeat must push the lease forward and stop as soon as another
    // worker takes over the task, without overwriting the other worker's lease.
    #[tokio::test]
    async fn heartbeat_extends_the_lease_and_stops_once_it_is_taken_over() {
        let fixture = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = fixture.db.clone();
        let now = identity::now();
        let lease = identity::random_token();
        let eventid = db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,status,lease_token,lease_until,dateline,available_at) VALUES('privacy.request:1','privacy.export','data_request',1,'{\"uid\":1}','processing',?,?,?,?)")
            .bind(&lease).bind(now + 2).bind(now).bind(now)
            .execute(db.executor()).await.unwrap().last_insert_id() as i64;
        let claim = Claim {
            eventid,
            request_id: 1,
            uid: 1,
            lease,
        };
        let heartbeat = Heartbeat::start(&db, &claim, now, Duration::from_millis(200));
        tokio::time::timeout(Duration::from_secs(5), async {
            while lease_until(&db, eventid).await < now + LEASE_SECONDS {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the heartbeat extends the lease to one period past now");

        db::query("UPDATE gonof_common_outbox SET lease_token=?,lease_until=? WHERE eventid=?")
            .bind(identity::random_token())
            .bind(now + 5)
            .bind(eventid)
            .execute(db.executor())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !heartbeat.task.as_ref().unwrap().is_finished() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the heartbeat stops once the lease is taken over");
        assert_eq!(lease_until(&db, eventid).await, now + 5);
        heartbeat.stop().await;
        fixture.cleanup().await.unwrap();
    }
}
