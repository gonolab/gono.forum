use super::*;
use crate::db::Tx;
use std::{collections::BTreeSet, ffi::OsStr};

struct Claim {
    eventid: i64,
    aid: i64,
    kind: String,
    payload: String,
    lease: String,
    attempts: i64,
}

/// Maximum number of jobs claimed per batch.
const BATCH_SIZE: usize = 8;

/// When the inspection subprocess gives no verdict (a timeout, its own or the parent's, killed by a signal, abnormal
/// exit, unreadable result), the inspection at which the file is judged as failed. Earlier ones back off and retry as
/// environment failures: a process under heavy load or killed by OOM should not get a legitimate file rejected
/// permanently; but a file that hangs every time cannot be retried forever either.
///
/// Only inspections without a verdict count, in the job's payload (`ProcessEvent::no_verdict`): claims that failed
/// for other reasons (storage, the output disk) do not use up the file's allowance, and an administrator's retry
/// starts it over ([`retried_payload`]).
const INSPECTION_ATTEMPTS: i64 = 3;

/// Maximum number of keys in one `event_key IN (…)` during the quarantine sweep.
const SOURCE_KEY_BATCH: usize = 500;

/// How long (seconds) a staging file may sit untouched before it is cleaned up. Staging files are registered or
/// deleted right after the upload request finishes writing them; only a request broken off midway leaves one behind.
const STAGING_SECONDS: i64 = 3600;

/// How long after a purge completes to re-check the objects once more.
///
/// As long as a processing process did not overrun its own lease, its upload happens before the purge (the purge
/// waits for the processing lease to expire). This re-check covers a process that overran its lease, uploaded late
/// and then failed to delete the object itself; one re-check and it is done.
const RECONCILE_GRACE_SECONDS: i64 = 300;

#[derive(Default, Deserialize)]
#[serde(default)]
struct PurgeEvent {
    reconcile: bool,
}

/// A processing job that has been inspected and is waiting for upload.
struct Prepared {
    source: PathBuf,
    output: PathBuf,
    inspection: inspect::Inspection,
    attachment: String,
    storage_profile_id: String,
}

impl Prepared {
    async fn discard(&self) {
        discard_output(&self.output).await;
    }
}

async fn discard_output(output: &Path) {
    let _ = tokio::fs::remove_file(output).await;
    let _ = tokio::fs::remove_file(output.with_extension("json")).await;
    let _ = tokio::fs::remove_file(inspect::thumbnail_path(output)).await;
}

/// Outcome of the inspection subprocess.
#[derive(Debug)]
enum Inspected {
    /// Finished normally with a verdict: passed.
    Passed(inspect::Inspection),
    /// Finished normally with a verdict: failed.
    Failed,
    /// Finished normally, but writing the output failed: the disk's fault, retried like a storage failure.
    Unavailable,
    /// No verdict was given (a timeout on either side, a crash, no result or a broken one): retried up to
    /// `INSPECTION_ATTEMPTS`.
    Abnormal(&'static str),
}

/// What the inspection subprocess writes as its result (see [`read_inspection`]): the inspection, `{"error":…}` for
/// a verdict against the file, `{"unavailable":…}` when the output could not be written, or `{"timeout":…}` when the
/// subprocess ran out of time itself ([`inspect::TimedOut`]). The parent waits as long, so this is usually how a
/// timeout shows, and it gives no more of a verdict than the parent's own.
fn verdict(result: anyhow::Result<inspect::Inspection>) -> anyhow::Result<serde_json::Value> {
    Ok(match result {
        Ok(result) => serde_json::to_value(result)?,
        Err(error) if error.downcast_ref::<inspect::OutputError>().is_some() => {
            serde_json::json!({"unavailable":"the processed output could not be written"})
        }
        Err(error) if error.downcast_ref::<inspect::TimedOut>().is_some() => {
            serde_json::json!({"timeout":"file processing timed out"})
        }
        Err(_) => {
            serde_json::json!({"error":"the file failed the format, structure or resource check"})
        }
    })
}

/// Determines the outcome from the subprocess exit status and the result file. When inspection fails the subprocess
/// still exits with 0 and writes `{"error":…}`; a non-zero exit, being killed by a signal or an incomplete result
/// file are not verdicts on the file, and neither are `{"unavailable":…}` and `{"timeout":…}`. The subprocess's own
/// timeout reads as the parent's.
fn read_inspection(exited_cleanly: bool, result: Option<&[u8]>) -> Inspected {
    if !exited_cleanly {
        return Inspected::Abnormal(crate::i18n::current().media_check_crashed());
    }
    let Some(bytes) = result else {
        return Inspected::Abnormal(crate::i18n::current().media_check_no_result());
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Inspected::Abnormal(crate::i18n::current().media_check_incomplete());
    };
    if value.get("unavailable").is_some() {
        return Inspected::Unavailable;
    }
    if value.get("timeout").is_some() {
        return Inspected::Abnormal(crate::i18n::current().media_check_timeout());
    }
    if value.get("error").is_some() {
        return Inspected::Failed;
    }
    match serde_json::from_value(value) {
        Ok(inspection) => Inspected::Passed(inspection),
        Err(_) => Inspected::Abnormal(crate::i18n::current().media_check_incomplete()),
    }
}

/// An inspection without a verdict is retried as a temporary failure the first few times and rejected only at the
/// `INSPECTION_ATTEMPTS`th such inspection (`count` includes this one).
fn inconclusive(count: i64, message: &str) -> MediaError {
    if count >= INSPECTION_ATTEMPTS {
        MediaError::Validation(message.into())
    } else {
        MediaError::Unavailable(format!("{message}; will retry automatically"))
    }
}

/// Early main() hook for resource-bounded inspection. No application/database initialization occurs here.
pub fn worker_entry() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).map(OsStr::new) != Some(OsStr::new("--gonof-media-inspect")) {
        return Ok(false);
    }
    anyhow::ensure!(args.len() == 6, "invalid media inspection arguments");
    let config: MediaConfig = serde_json::from_str(
        args[5]
            .to_str()
            .context("invalid inspection configuration")?,
    )?;
    let result = inspect::inspect(
        Path::new(&args[2]),
        Path::new(&args[3]),
        args[4].to_str().context("invalid file name")?,
        &config,
    );
    let path = Path::new(&args[3]).with_extension("json");
    std::fs::write(path, serde_json::to_vec(&verdict(result)?)?)?;
    Ok(true)
}

/// Pacing of media processing jobs: when no job can be claimed for consecutive rounds,
/// slow down step by step, 2 seconds, 4 seconds, at most one check every 5 seconds; when this process registers a new
/// upload or schedules a purge ([`Pacer::wake`]) the next round processes normally right away. With multiple
/// processes, an upload received by another process starts processing at most 5 seconds late.
#[derive(Debug, Default)]
pub(super) struct Pacer {
    woken: std::sync::atomic::AtomicBool,
    /// Number of consecutive idle rounds and the time of the next check.
    idle: std::sync::Mutex<(u32, Option<std::time::Instant>)>,
}

/// Longest interval between checks while idle.
const IDLE_MAX: Duration = Duration::from_secs(5);

impl Pacer {
    pub(super) fn wake(&self) {
        self.woken.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u32, Option<std::time::Instant>)> {
        self.idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this round should check.
    fn due(&self) -> bool {
        if self.woken.swap(false, std::sync::atomic::Ordering::SeqCst) {
            *self.lock() = (0, None);
            return true;
        }
        self.lock()
            .1
            .is_none_or(|next| std::time::Instant::now() >= next)
    }

    /// Check done: if there was work, keep checking every round; if idle, push the next check back.
    fn finished(&self, busy: bool) {
        let mut idle = self.lock();
        if busy {
            *idle = (0, None);
            return;
        }
        idle.0 = idle.0.saturating_add(1);
        let delay = Duration::from_secs(1_u64 << idle.0.min(3)).min(IDLE_MAX);
        idle.1 = Some(std::time::Instant::now() + delay);
    }
}

impl Media {
    /// Called once per second by the background "media processing" task: decides by the [`Pacer`] whether this round
    /// checks, sending less SQL while idle.
    pub async fn process_when_due(&self, now: i64) -> Result<()> {
        if !self.pacer.due() {
            return Ok(());
        }
        let outcome = self.process_pending(now).await;
        let busy = outcome.as_ref().map_or(true, |report| {
            report.processed + report.failed + report.purged > 0
        });
        self.pacer.finished(busy);
        outcome.map(drop)
    }

    /// Claims at most `BATCH_SIZE` jobs per batch, rotating among uploaders (see `claim`).
    pub async fn process_pending(&self, now: i64) -> Result<MediaReport> {
        let mut report = MediaReport::default();
        let mut served = BTreeSet::new();
        // Background work: what it stores for members to read (why a file did not pass the check) is in the site
        // language (docs/architecture.md, "Languages"), read once a batch has a job.
        let mut site: Option<crate::site::Site> = None;
        for _ in 0..BATCH_SIZE {
            let Some(claim) = self.claim(now, &mut served).await? else {
                break;
            };
            let scope = match &site {
                Some(site) => site.clone(),
                None => {
                    let language = crate::runtime::read(&self.db)
                        .await
                        .map_err(|error| MediaError::Internal(error.into()))?
                        .site
                        .lang();
                    site.insert(crate::site::Site {
                        language,
                        ..crate::site::Site::default()
                    })
                    .clone()
                }
            };
            // AWS operation futures are large; keep nested worker calls off the executor stack.
            Box::pin(crate::site::scope(
                scope,
                self.run_claim(&claim, now, &mut report),
            ))
            .await?;
        }
        Ok(report)
    }

    /// Runs one claimed job to its settlement.
    async fn run_claim(&self, claim: &Claim, now: i64, report: &mut MediaReport) -> Result<()> {
        if claim.kind == "media.purge" {
            match Box::pin(self.purge_claim(claim, now)).await {
                // The first purge rescheduled its re-check in the transaction that marked it purged.
                Ok(true) => report.purged += 1,
                outcome => self.settle(claim, outcome.map(drop), now, report).await?,
            }
            return Ok(());
        }
        match Box::pin(self.prepare_claim(claim, now)).await {
            Ok(Some(prepared)) => {
                let outcome = Box::pin(self.store_claim(claim, &prepared, now)).await;
                prepared.discard().await;
                self.settle(claim, outcome, now, report).await
            }
            Ok(None) => self.settle(claim, Ok(()), now, report).await,
            Err(error) => self.settle(claim, Err(error), now, report).await,
        }
    }

    /// Settles a job by its outcome: completed, failed inspection (rejected) or retry later.
    async fn settle(
        &self,
        claim: &Claim,
        outcome: Result<()>,
        now: i64,
        report: &mut MediaReport,
    ) -> Result<()> {
        match outcome {
            Ok(()) => {
                self.finish_claim(claim, None, now).await?;
                if claim.kind == "media.purge" {
                    report.purged += 1
                } else {
                    report.processed += 1
                }
            }
            Err(MediaError::Validation(message)) => {
                self.reject_claim(claim, &message, now).await?;
                self.finish_claim(claim, None, now).await?;
                report.failed += 1;
            }
            Err(error) => {
                tracing::warn!(
                    eventid = claim.eventid,
                    aid = claim.aid,
                    kind = %claim.kind,
                    attempts = claim.attempts,
                    error = ?error,
                    "attachment job not completed, will retry"
                );
                self.finish_claim(
                    claim,
                    Some("the attachment service is temporarily unavailable"),
                    now,
                )
                .await?;
                report.failed += 1;
            }
        }
        Ok(())
    }

    async fn reject_claim(&self, claim: &Claim, message: &str, now: i64) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        if self.claim_is_current(&mut tx, claim).await? {
            crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='rejected',scan_status='rejected',scan_detail=?,checked_at=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle IN ('pending','processing')")
                .bind(crate::media::check_code(message)).bind(now).bind(now).bind(claim.aid).execute(self.db.observe(&mut *tx)).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    fn lease_seconds(&self) -> i64 {
        self.config
            .processing_timeout_seconds
            .saturating_add(180)
            .min(i64::MAX as u64) as i64
    }

    /// Claims the next job.
    ///
    /// Within one batch, claims rotate among uploaders: first one job for each uploader still in the queue (the
    /// earliest of each), and only after everyone has had a turn does the next round start. With plain first-in
    /// first-out by eventid, one member's large batch of uploads would block everyone.
    ///
    /// The candidate is read outside the lock, then that row is locked by primary key and its status and times are
    /// re-checked in code: another process may already have claimed it. Claims and leases use real time: a batch can
    /// run for several minutes, and reusing the `now` from the start of the batch would make the leases of later jobs
    /// close to expiry the moment they are claimed, so other processes would claim them again.
    async fn claim(&self, now: i64, served: &mut BTreeSet<i64>) -> Result<Option<Claim>> {
        for _ in 0..4 {
            let now = identity::now().max(now);
            let Some((eventid, uploader)) = self.next_candidate(now, served).await? else {
                if served.is_empty() {
                    return Ok(None);
                }
                served.clear();
                continue;
            };
            let mut tx = self.db.begin_write().await?;
            let row = crate::db::query("SELECT eventid,aggregate_id,event_type,payload,attempts,status,available_at,lease_until FROM gonof_common_outbox WHERE eventid=? FOR UPDATE")
                .bind(eventid).fetch_optional(self.db.observe(&mut *tx)).await?;
            let Some(row) = row else {
                tx.commit().await?;
                continue;
            };
            let claimable = matches!(db::text(&row, "status")?.as_str(), "pending" | "processing")
                && row.try_get::<i64, _>("available_at")? <= now
                && row.try_get::<i64, _>("lease_until")? <= now;
            if !claimable {
                tx.commit().await?;
                continue;
            }
            let claim = Claim {
                eventid,
                aid: row.try_get("aggregate_id")?,
                kind: db::text(&row, "event_type")?,
                payload: db::text(&row, "payload")?,
                lease: identity::random_token(),
                attempts: row.try_get::<i64, _>("attempts")? + 1,
            };
            crate::db::query("UPDATE gonof_common_outbox SET status='processing',attempts=attempts+1,lease_token=?,lease_until=? WHERE eventid=?")
                .bind(&claim.lease).bind(now.saturating_add(self.lease_seconds())).bind(claim.eventid).execute(self.db.observe(&mut *tx)).await?;
            tx.commit().await?;
            served.insert(uploader);
            return Ok(Some(claim));
        }
        Ok(None)
    }

    /// Among uploaders who have not had a turn this round, the job due longest and its uploader
    /// (`outbox::next_due_sql`: in each range of `gonof_outbox_claim`, the first job whose uploader is still waiting).
    async fn next_candidate(&self, now: i64, served: &BTreeSet<i64>) -> Result<Option<(i64, i64)>> {
        const TYPES: [&str; 2] = ["media.process", "media.purge"];
        let filter = if served.is_empty() {
            String::new()
        } else {
            format!(" AND a.uid NOT IN ({})", vec!["?"; served.len()].join(","))
        };
        let sql = crate::outbox::next_due_sql(
            TYPES.len(),
            "o.eventid,o.available_at,a.uid",
            " JOIN gonof_forum_attachment a ON a.aid=o.aggregate_id",
            &filter,
        );
        let query = crate::outbox::bind_next_due(crate::db::query(&sql), &TYPES, now, |query| {
            served.iter().fold(query, |query, uid| query.bind(*uid))
        });
        let row = query.fetch_optional(self.db.executor()).await?;
        Ok(match row {
            Some(row) => Some((row.try_get("eventid")?, row.try_get("uid")?)),
            None => None,
        })
    }

    /// Settles one job: an empty `error` means completed; otherwise back off and retry as a temporary failure, and
    /// after `outbox::MAX_ATTEMPTS` consecutive failures stop retrying automatically, set it to `failed` and wait for
    /// an administrator to schedule a retry in the admin area.
    async fn finish_claim(&self, claim: &Claim, error: Option<&str>, now: i64) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        let updated = match error {
            None => crate::db::query("UPDATE gonof_common_outbox SET status='sent',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND lease_token=? AND status='processing'")
                .bind(now).bind(claim.eventid).bind(&claim.lease).execute(self.db.observe(&mut *tx)).await?,
            Some(error) if claim.attempts >= crate::outbox::MAX_ATTEMPTS => {
                tracing::warn!(
                    eventid = claim.eventid,
                    aid = claim.aid,
                    kind = %claim.kind,
                    attempts = claim.attempts,
                    "attachment job retries exhausted, automatic retry stopped"
                );
                crate::db::query("UPDATE gonof_common_outbox SET status='failed',lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND lease_token=? AND status='processing'")
                    .bind(crate::outbox::exhausted_error(claim.attempts, error)).bind(claim.eventid).bind(&claim.lease).execute(self.db.observe(&mut *tx)).await?
            }
            Some(error) => {
                let delay = 10_i64
                    .saturating_mul(1_i64 << claim.attempts.min(8))
                    .min(3600);
                // Back-off counts from real time; the now from the start of the batch may be several minutes old.
                let retry_at = identity::now().max(now).saturating_add(delay);
                crate::db::query("UPDATE gonof_common_outbox SET status='pending',available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=? WHERE eventid=? AND lease_token=? AND status='processing'")
                    .bind(retry_at).bind(error).bind(claim.eventid).bind(&claim.lease).execute(self.db.observe(&mut *tx)).await?
            }
        };
        if error.is_some() && updated.rows_affected() == 1 && claim.kind == "media.process" {
            // Stable codes, translated when read (`media::check_code`).
            let detail = if claim.attempts >= crate::outbox::MAX_ATTEMPTS {
                "paused"
            } else {
                "retrying"
            };
            crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='pending',scan_status='error',scan_detail=?,updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle='processing'")
                .bind(detail).bind(now).bind(claim.aid).execute(self.db.observe(&mut *tx)).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// After a purge completes, reschedules the same purge event once to re-check, when due, that the objects are
    /// really deleted (see `purge_claim`). Runs in the transaction that marks the attachment purged: a process that
    /// stopped in between would leave a purged attachment whose event a retry settles without the re-check.
    async fn schedule_reconciliation(
        &self,
        tx: &mut Tx<'_>,
        claim: &Claim,
        now: i64,
    ) -> Result<()> {
        let at = identity::now()
            .max(now)
            .saturating_add(self.lease_seconds())
            .saturating_add(RECONCILE_GRACE_SECONDS);
        crate::db::query("UPDATE gonof_common_outbox SET status='pending',payload='{\"reconcile\":true}',attempts=0,available_at=GREATEST(dateline,?),lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?")
            .bind(at)
            .bind(claim.eventid)
            .bind(&claim.lease)
            .execute(self.db.observe(&mut **tx))
            .await?;
        Ok(())
    }

    /// First half of a processing job: mark it as processing, verify the quarantined file, inspect it in a
    /// subprocess. When the attachment is already terminal, returns `None` and deletes any leftover object on the way.
    async fn prepare_claim(&self, claim: &Claim, now: i64) -> Result<Option<Prepared>> {
        let mut tx = self.db.begin_write().await?;
        if !self.claim_is_current(&mut tx, claim).await? {
            tx.commit().await?;
            return Err(MediaError::Unavailable(
                "the media job lease has expired".into(),
            ));
        }
        let row=crate::db::query(&format!("SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE"))
            .bind(claim.aid).fetch_optional(self.db.observe(&mut *tx)).await?.ok_or(MediaError::NotFound)?;
        let state = db::text(&row, "lifecycle")?;
        if !matches!(state.as_str(), "pending" | "processing") {
            // The event is claimed again only because the previous claimer may have uploaded the object and crashed
            // before settling. When the attachment is rejected or being purged and has no references, delete the
            // object once; deleting an object that does not exist also counts as success.
            let delete_late = matches!(state.as_str(), "rejected" | "purging" | "purged")
                && self.attachment_reference_count(&mut tx, claim.aid).await? == 0;
            tx.commit().await?;
            if delete_late {
                self.delete_object(&row).await?;
            }
            return Ok(None);
        }
        // A queued non-image attachment (for example one queued by an older version) is not inspected but rejected
        // directly: only images are accepted.
        if !inspect::allowed_extension(&inspect::extension(&db::text(&row, "filename")?)) {
            tx.commit().await?;
            return Err(MediaError::Validation(
                crate::i18n::current().media_check_unsupported().into(),
            ));
        }
        crate::db::query(
            "UPDATE gonof_forum_attachment_0 SET lifecycle='processing',updated_at=GREATEST(dateline,?) WHERE aid=?",
        )
        .bind(now)
        .bind(claim.aid)
        .execute(self.db.observe(&mut *tx))
        .await?;
        tx.commit().await?;
        let payload: ProcessEvent =
            serde_json::from_str(&claim.payload).context("invalid media job payload")?;
        if payload.source.contains(['/', '\\']) || payload.source.contains("..") {
            return Err(MediaError::Validation(
                crate::i18n::current().media_check_source().into(),
            ));
        }
        let source = self.directory.join("sources").join(&payload.source);
        if hash_file(&source).await? != db::text(&row, "content_sha256")? {
            return Err(MediaError::Validation(
                crate::i18n::current().media_check_mismatch().into(),
            ));
        }
        let output_directory = self.directory.join("processed");
        private_directory(&output_directory).await?;
        let output = output_directory.join(format!("{}-{}", claim.aid, claim.lease));
        let inspected = self
            .inspect_process(&source, &output, &db::text(&row, "filename")?)
            .await;
        let inspection = match inspected {
            Ok(Inspected::Passed(inspection)) => inspection,
            Ok(Inspected::Failed) => {
                discard_output(&output).await;
                return Err(MediaError::Validation(
                    crate::i18n::current().media_check_failed().into(),
                ));
            }
            Ok(Inspected::Unavailable) => {
                discard_output(&output).await;
                return Err(MediaError::Unavailable(
                    "writing the processed image failed; will retry automatically".into(),
                ));
            }
            Ok(Inspected::Abnormal(message)) => {
                discard_output(&output).await;
                let count = payload.no_verdict.saturating_add(1);
                if count < INSPECTION_ATTEMPTS {
                    self.record_no_verdict(claim, count).await?;
                }
                return Err(inconclusive(count, message));
            }
            Err(error) => {
                discard_output(&output).await;
                return Err(error);
            }
        };
        Ok(Some(Prepared {
            source,
            output,
            inspection,
            attachment: db::text(&row, "attachment")?,
            storage_profile_id: db::text(&row, "storage_profile_id")?,
        }))
    }

    /// Writes how many inspections of this job have given no verdict into its payload, under the lease fence; the
    /// other payload fields (the source file, the original size the daily quota reads) stay as they are.
    async fn record_no_verdict(&self, claim: &Claim, count: i64) -> Result<()> {
        let mut payload: serde_json::Value =
            serde_json::from_str(&claim.payload).context("invalid media job payload")?;
        let fields = payload
            .as_object_mut()
            .context("invalid media job payload")?;
        fields.insert("no_verdict".into(), count.into());
        crate::db::query("UPDATE gonof_common_outbox SET payload=? WHERE eventid=? AND lease_token=? AND status='processing'")
            .bind(payload.to_string())
            .bind(claim.eventid)
            .bind(&claim.lease)
            .execute(self.db.executor())
            .await?;
        Ok(())
    }

    /// Second half of a processing job: upload the processed result and mark the attachment ready under the lease
    /// fence.
    async fn store_claim(&self, claim: &Claim, prepared: &Prepared, now: i64) -> Result<()> {
        let (client, profile) = self.object_client(&prepared.storage_profile_id).await?;
        let inspection = &prepared.inspection;
        let body = aws_sdk_s3::primitives::ByteStream::from_path(&prepared.output)
            .await
            .map_err(|_| {
                MediaError::Unavailable("the processed attachment cannot be read".into())
            })?;
        client
            .put_object()
            .bucket(&profile.bucket)
            .key(&prepared.attachment)
            .content_type(&inspection.mime_type)
            .body(body)
            .send()
            .await
            .map_err(|_| MediaError::Unavailable("uploading to object storage failed".into()))?;
        // The list thumbnail is stored after the original; failing to store it does not keep the
        // attachment from being ready, the first request will resize it on the fly and store it then.
        if let Some(thumb_mime) = &inspection.thumbnail {
            let stored = match aws_sdk_s3::primitives::ByteStream::from_path(
                inspect::thumbnail_path(&prepared.output),
            )
            .await
            {
                Ok(body) => client
                    .put_object()
                    .bucket(&profile.bucket)
                    .key(super::thumbnail::object_key(&prepared.attachment))
                    .content_type(thumb_mime)
                    .body(body)
                    .send()
                    .await
                    .map(drop)
                    .map_err(|error| format!("{error:?}")),
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = stored {
                tracing::warn!(aid = claim.aid, error = %error, "thumbnail not stored, will be resized on first request");
            }
        }
        let mut tx = self.db.begin_write().await?;
        // Use the outbox lease as a fence and the detail row as the common binding/cleanup lock.
        let current = self.claim_is_current(&mut tx, claim).await?;
        let state: String = crate::db::query_scalar(
            "SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE",
        )
        .bind(claim.aid)
        .fetch_one(self.db.observe(&mut *tx))
        .await?;
        if !current || !matches!(state.as_str(), "pending" | "processing") {
            // Only terminal states forbid a new binding after this lock is released.
            // A ready image stays available until the orphan cleaner fences it as purging.
            // Delete it even when the lease has been taken over: the new holder may already have seen the attachment
            // become terminal and settled early, so nobody else would delete this late upload. If the attachment is
            // still processing, the new holder is redoing it and will overwrite the same key.
            let delete_late = matches!(state.as_str(), "rejected" | "purging" | "purged")
                && self.attachment_reference_count(&mut tx, claim.aid).await? == 0;
            tx.commit().await?;
            if delete_late {
                for key in [
                    prepared.attachment.clone(),
                    super::thumbnail::object_key(&prepared.attachment),
                ] {
                    client
                        .delete_object()
                        .bucket(&profile.bucket)
                        .key(key)
                        .send()
                        .await
                        .map_err(|_| {
                            MediaError::Unavailable(
                                "removing a late attachment object failed".into(),
                            )
                        })?;
                }
            }
            return if current {
                Ok(())
            } else {
                Err(MediaError::Unavailable(
                    "the media job lease has expired".into(),
                ))
            };
        }
        // The inspection result comes back across processes, so re-check its values before writing (the value rules
        // live in code, see `check_inspection`).
        let filesize =
            i64::try_from(inspection.filesize).map_err(|e| MediaError::Internal(e.into()))?;
        super::check_inspection(
            &inspection.mime_type,
            &inspection.content_sha256,
            filesize,
            inspection.width,
            inspection.height,
        )
        .map_err(MediaError::Internal)?;
        crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='ready',scan_status='clean',scan_detail=?,mime_type=?,content_sha256=?,filesize=?,width=?,height=?,checked_at=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=?")
            .bind("passed").bind(&inspection.mime_type).bind(&inspection.content_sha256).bind(filesize).bind(inspection.width).bind(inspection.height).bind(now).bind(now).bind(claim.aid)
            .execute(self.db.observe(&mut *tx)).await?;
        tx.commit().await?;
        let _ = tokio::fs::remove_file(&prepared.source).await;
        Ok(())
    }

    /// Runs both halves of a single job, for tests to drive directly.
    #[cfg(test)]
    async fn process_claim(&self, claim: &Claim, now: i64) -> Result<()> {
        let Some(prepared) = self.prepare_claim(claim, now).await? else {
            return Ok(());
        };
        let result = self.store_claim(claim, &prepared, now).await;
        prepared.discard().await;
        result
    }

    /// Deletes the attachment's object and its thumbnail object; deleting an object that does not exist
    /// also counts as success.
    async fn delete_object(&self, row: &DbRow) -> Result<()> {
        let (client, profile) = self
            .object_client(&db::text(row, "storage_profile_id")?)
            .await?;
        let attachment = db::text(row, "attachment")?;
        for key in [
            super::thumbnail::object_key(&attachment),
            attachment.clone(),
        ] {
            client
                .delete_object()
                .bucket(&profile.bucket)
                .key(key)
                .send()
                .await
                .map_err(|_| {
                    MediaError::Unavailable("removing the attachment object failed".into())
                })?;
        }
        Ok(())
    }

    async fn claim_is_current(&self, tx: &mut Tx<'_>, claim: &Claim) -> Result<bool> {
        let current: Option<String> = crate::db::query_scalar("SELECT lease_token FROM gonof_common_outbox WHERE eventid=? AND status='processing' FOR UPDATE")
        .bind(claim.eventid)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?
        .flatten();
        Ok(current.as_deref() == Some(&claim.lease))
    }

    #[cfg(not(test))]
    async fn inspect_process(
        &self,
        source: &Path,
        output: &Path,
        filename: &str,
    ) -> Result<Inspected> {
        let mut executable = std::env::current_exe()?;
        if executable.parent().is_some_and(|p| p.ends_with("deps")) {
            // Integration tests run from target/<profile>/deps; the web binary sits one level up.
            executable = executable
                .parent()
                .and_then(Path::parent)
                .context("cannot locate the media inspection program")?
                .join("gono-forum-web");
        } else if cfg!(target_os = "linux") {
            // The running image itself, even after a one-click upgrade has renamed a new binary over the file
            // (current_exe would then name the replacement, or a path ending in " (deleted)").
            executable = std::path::PathBuf::from("/proc/self/exe");
        }
        let configuration =
            serde_json::to_string(&self.config).context("invalid inspection configuration")?;
        let mut command = tokio::process::Command::new(executable);
        command
            .arg("--gonof-media-inspect")
            .arg(source)
            .arg(output)
            .arg(filename)
            .arg(configuration)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let status = match tokio::time::timeout(
            Duration::from_secs(self.config.processing_timeout_seconds),
            command.status(),
        )
        .await
        {
            Err(_) => {
                return Ok(Inspected::Abnormal(
                    crate::i18n::current().media_check_timeout(),
                ));
            }
            Ok(status) => status.map_err(|_| {
                MediaError::Unavailable("the inspection program cannot start".into())
            })?,
        };
        let result = tokio::fs::read(output.with_extension("json")).await.ok();
        Ok(read_inspection(status.success(), result.as_deref()))
    }

    /// In tests the inspection runs in this process, through the same result the subprocess would write, so its own
    /// deadline (`processing_timeout_seconds`) acts as the subprocess's would. [`tests::NO_VERDICT`] makes the next
    /// inspections time out in the parent instead.
    #[cfg(test)]
    async fn inspect_process(
        &self,
        source: &Path,
        output: &Path,
        filename: &str,
    ) -> Result<Inspected> {
        if tests::NO_VERDICT.with(|left| left.replace(left.get().saturating_sub(1))) > 0 {
            return Ok(Inspected::Abnormal(
                crate::i18n::current().media_check_timeout(),
            ));
        }
        let result = verdict(inspect::inspect(source, output, filename, &self.config))?;
        Ok(read_inspection(
            true,
            Some(&serde_json::to_vec(&result).context("invalid inspection result")?),
        ))
    }

    #[cfg(test)]
    pub async fn cancel(&self, uid: i64, aid: i64) -> Result<()> {
        self.cancel_actor(self.test_actor(uid).await?, aid).await
    }

    pub async fn cancel_actor(&self, actor: identity::Actor, aid: i64) -> Result<()> {
        // Cancelling one's own attachment that is not referenced yet only affects oneself, so it works as usual in
        // read-only mode.
        let mut tx = self.db.begin_write().await?;
        self.member_actor_locked(&mut tx, actor, false).await?;
        self.purge_own(tx, actor.uid, aid, false).await
    }

    /// Sends the member's own unreferenced attachment to purge, in `tx`, which has rechecked the member. With
    /// `library` (deleting from "My attachments"), only an attachment in the trash or one that failed the check
    /// qualifies, judged under the attachment row lock: a page opened before the attachment was restored cannot
    /// delete it any more. One already being purged counts as deleted.
    pub(super) async fn purge_own(
        &self,
        mut tx: Tx<'_>,
        uid: i64,
        aid: i64,
        library: bool,
    ) -> Result<()> {
        let row=crate::db::query("SELECT a.uid,d.lifecycle,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(aid).fetch_optional(self.db.observe(&mut *tx)).await?.ok_or(MediaError::NotFound)?;
        if row.try_get::<i64, _>("uid")? != uid {
            return Err(MediaError::NotFound);
        }
        if library {
            let lifecycle = db::text(&row, "lifecycle")?;
            if matches!(lifecycle.as_str(), "purging" | "purged") {
                tx.commit().await?;
                return Ok(());
            }
            let trashed = row.try_get::<Option<i64>, _>("trashed_at")?.is_some();
            if !(lifecycle == "rejected" || (lifecycle == "ready" && trashed)) {
                return Err(MediaError::Conflict(
                    crate::i18n::current().media_error_delete_kept().into(),
                ));
            }
        }
        let references = self.attachment_reference_count(&mut tx, aid).await?;
        if references > 0 {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_cancel_in_use().into(),
            ));
        }
        let now = identity::now();
        crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purging',purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle<>'purged'")
            .bind(now).bind(now).bind(aid).execute(self.db.observe(&mut *tx)).await?;
        self.enqueue(
            &mut tx,
            aid,
            "media.purge",
            &format!("media.purge:{aid}"),
            serde_json::json!({}),
            now,
        )
        .await?;
        self.audit(
            &mut tx,
            uid,
            "media.cancel",
            serde_json::json!({"aid":aid}),
            now,
        )
        .await?;
        tx.commit().await?;
        self.pacer.wake();
        Ok(())
    }

    pub async fn cleanup(&self, now: i64) -> Result<MediaReport> {
        // States are listed value by value instead of NOT IN ('purging','purged'): the index `(lifecycle,
        // purge_after)` can only go on to a range on the expiry time when the state is an equality.
        // The set of values follows `media::LIFECYCLES`; when adding a state, change this as well.
        let rows=crate::db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE lifecycle IN ('pending','processing','ready','rejected') AND purge_after<=? ORDER BY aid LIMIT 100")
            .bind(now).fetch_all(self.db.executor()).await?;
        for row in rows {
            let aid: i64 = row.try_get("aid")?;
            let mut tx = self.db.begin_write().await?;
            let row = crate::db::query(
                "SELECT purge_after,lifecycle FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE",
            )
            .bind(aid)
            .fetch_one(self.db.observe(&mut *tx))
            .await?;
            // Candidates were listed outside the lock: while waiting for the lock the attachment may have been sent
            // to purge, or even purged, by another flow; its media.purge event may already be sent, and setting it
            // back to purging would leave nobody to process it.
            if matches!(db::text(&row, "lifecycle")?.as_str(), "purging" | "purged") {
                tx.commit().await?;
                continue;
            }
            let expiry: Option<i64> = row.try_get("purge_after")?;
            let count = self.attachment_reference_count(&mut tx, aid).await?;
            if count > 0 {
                crate::db::query("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=? AND lifecycle NOT IN ('purging','purged')").bind(aid).execute(self.db.observe(&mut *tx)).await?;
            } else if expiry.is_some_and(|v| v <= now) {
                crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purging',updated_at=GREATEST(dateline,?) WHERE aid=?")
                    .bind(now).bind(aid).execute(self.db.observe(&mut *tx)).await?;
                self.enqueue(
                    &mut tx,
                    aid,
                    "media.purge",
                    &format!("media.purge:{aid}"),
                    serde_json::json!({}),
                    now,
                )
                .await?;
            }
            tx.commit().await?;
        }
        self.cleanup_files(now).await?;
        // Purged attachments do not resend DELETE periodically: each purge event is rescheduled once after it
        // completes, re-checks the objects once when due (`schedule_reconciliation`), and then ends.
        self.process_pending(now).await
    }

    /// Purges the attachment objects. Returns true when the first purge completes, having rescheduled the event for
    /// one re-check in the same transaction; returns false for a re-check or when there is nothing to do.
    async fn purge_claim(&self, claim: &Claim, now: i64) -> Result<bool> {
        let mut tx = self.db.begin_write().await?;
        let row=crate::db::query("SELECT attachment,storage_profile_id,lifecycle FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
            .bind(claim.aid).fetch_one(self.db.observe(&mut *tx)).await?;
        let references = self.attachment_reference_count(&mut tx, claim.aid).await?;
        tx.commit().await?;
        let reconcile = serde_json::from_str::<PurgeEvent>(&claim.payload)
            .map(|event| event.reconcile)
            .unwrap_or(false);
        match db::text(&row, "lifecycle")?.as_str() {
            "purging" => {}
            // Re-check: when a processing process overran its lease, uploaded late after the purge and then crashed,
            // the object gets written back. A purged attachment never gets new bindings, so deleting once is enough.
            "purged" if reconcile && references == 0 => {
                self.delete_object(&row).await?;
                return Ok(false);
            }
            _ => return Ok(false),
        }
        // New bindings reject purging, so this guard remains true until deletion finishes.
        if references != 0 {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_purge_in_use().into(),
            ));
        }
        // Wait for earlier process workers before deleting; late workers also delete their just-written object.
        // The processing event key is always media.process:{aid}, which uses the unique index; status and lease are
        // checked in code.
        let process_key = format!("media.process:{}", claim.aid);
        let process = crate::db::query(
            "SELECT status,lease_until FROM gonof_common_outbox WHERE event_key=?",
        )
        .bind(&process_key)
        .fetch_optional(self.db.executor())
        .await?;
        let active = match &process {
            Some(row) => {
                db::text(row, "status")? == "processing"
                    && row.try_get::<i64, _>("lease_until")? > identity::now()
            }
            None => false,
        };
        if active {
            return Err(MediaError::Unavailable(
                "the attachment is still being processed; purge later".into(),
            ));
        }
        self.delete_object(&row).await?;
        let payload = crate::db::query("SELECT payload FROM gonof_common_outbox WHERE event_key=?")
            .bind(&process_key)
            .fetch_optional(self.db.executor())
            .await?;
        if let Some(payload) = payload {
            let source: ProcessEvent = serde_json::from_str(&db::text(&payload, "payload")?)
                .context("invalid media job")?;
            if !source.source.contains(['/', '\\']) {
                let _ = tokio::fs::remove_file(self.directory.join("sources").join(source.source))
                    .await;
            }
        }
        let mut tx = self.db.begin_write().await?;
        let mut purged = false;
        if self.claim_is_current(&mut tx, claim).await? {
            purged = crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purged',updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle='purging'")
                .bind(identity::now()).bind(claim.aid).execute(self.db.observe(&mut *tx)).await?.rows_affected() == 1;
            if purged {
                self.schedule_reconciliation(&mut tx, claim, now).await?;
            }
        }
        tx.commit().await?;
        Ok(purged)
    }

    /// File names in `sources/` still in use by processing. Orphans are identified from the database
    /// rather than by deriving the aid from the file name (the file is written to disk before the database row, and
    /// its name has nothing to do with the aid): for attachments being processed (`pending`, `processing`, using the
    /// `(lifecycle, purge_after)` index), build the keys `media.process:{aid}` and fetch their processing events with
    /// one `event_key IN (…)` per [`SOURCE_KEY_BATCH`] keys; while an event is unfinished, the file name in its
    /// payload is in use. Once an attachment leaves these two states, the processing job does not read the source
    /// file any more (`prepare_claim`).
    async fn sources_in_use(&self) -> Result<std::collections::HashSet<String>> {
        let aids: Vec<i64> = crate::db::query_scalar(
            "SELECT aid FROM gonof_forum_attachment_0 WHERE lifecycle IN ('pending','processing')",
        )
        .fetch_all(self.db.executor())
        .await?;
        let mut names = std::collections::HashSet::new();
        for chunk in aids.chunks(SOURCE_KEY_BATCH) {
            let sql = format!(
                "SELECT status,payload FROM gonof_common_outbox WHERE event_key IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut query = crate::db::query(&sql);
            for aid in chunk {
                query = query.bind(format!("media.process:{aid}"));
            }
            for row in query.fetch_all(self.db.executor()).await? {
                if matches!(db::text(&row, "status")?.as_str(), "pending" | "processing")
                    && let Ok(event) =
                        serde_json::from_str::<ProcessEvent>(&db::text(&row, "payload")?)
                {
                    names.insert(event.source);
                }
            }
        }
        Ok(names)
    }

    async fn cleanup_files(&self, now: i64) -> Result<()> {
        let mut in_use = None;
        for name in ["staging", "sources", "processed", "secrets"] {
            let directory = self.directory.join(name);
            let mut files = match tokio::fs::read_dir(&directory).await {
                Ok(files) => files,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(file) = files.next_entry().await? {
                let metadata = file.metadata().await?;
                if !metadata.is_file() {
                    continue;
                }
                let modified = metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                // The staging area only holds request bodies still being received; files left by a request broken
                // off midway are cleaned after one hour (the other directories wait for `orphan_seconds`).
                let keep = if name == "staging" {
                    STAGING_SECONDS
                } else {
                    self.config.orphan_seconds
                };
                if modified.saturating_add(keep) > now {
                    continue;
                }
                if name == "sources" {
                    // Query the database only once there is an expired file; other files in the same round share
                    // this list.
                    if in_use.is_none() {
                        in_use = Some(self.sources_in_use().await?);
                    }
                    if in_use.as_ref().is_some_and(|names| {
                        names.contains(file.file_name().to_string_lossy().as_ref())
                    }) {
                        continue;
                    }
                }
                if name == "secrets" {
                    let filename = file.file_name().to_string_lossy().into_owned();
                    if self
                        .stored()
                        .await?
                        .profiles
                        .values()
                        .any(|profile| profile.secret_ref == filename)
                    {
                        continue;
                    }
                }
                let _ = tokio::fs::remove_file(file.path()).await;
            }
        }
        Ok(())
    }
}

/// The payload an administrator's retry of a media job writes back, or `None` to keep it: a processing job's count
/// of inspections without a verdict starts over (`INSPECTION_ATTEMPTS`), so the retried file gets its full allowance.
pub(crate) fn retried_payload(event_type: &str, payload: &str) -> Option<String> {
    if event_type != "media.process" {
        return None;
    }
    let mut payload: serde_json::Value = serde_json::from_str(payload).ok()?;
    payload.as_object_mut()?.remove("no_verdict")?;
    Some(payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::{FakeS3, Fixture, png, png_sized};

    thread_local! {
        /// How many of the next inspections on this thread give no verdict (see `inspect_process`).
        pub(super) static NO_VERDICT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn idle_rounds_back_off_until_woken() {
        let pacer = Pacer::default();
        assert!(pacer.due(), "the first round checks as usual");
        pacer.finished(false);
        assert!(!pacer.due(), "an idle round postpones the next");
        pacer.wake();
        assert!(pacer.due(), "new work in this process checks at once");
        pacer.finished(true);
        assert!(pacer.due(), "a busy round checks again next round");
        for _ in 0..10 {
            pacer.finished(false);
        }
        let next = pacer.lock().1.unwrap();
        assert!(
            next <= std::time::Instant::now() + IDLE_MAX,
            "at most 5 seconds between checks"
        );
    }

    /// Finding the next job reads only the due media jobs, in `gonof_outbox_claim` order, also when uploaders who had
    /// their turn are left out; not the jobs already done or the backlog of other queues.
    #[tokio::test]
    async fn media_claims_read_the_claim_index() {
        use crate::debug::trace::{self, RequestProfile};
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let upload = fixture
            .upload(&identity::random_token(), "queued.png", &png())
            .await
            .unwrap();
        let now = identity::now();
        crate::test_support::outbox_backlog(&fixture.db, &["media.process", "media.purge"], now)
            .await
            .unwrap();
        let profile = std::sync::Arc::new(RequestProfile::new());
        let served = BTreeSet::from([fixture.uid + 1]);
        let candidate = trace::scope(profile.clone(), fixture.media.next_candidate(now, &served))
            .await
            .unwrap();
        assert_eq!(candidate.map(|(_, uid)| uid), Some(fixture.uid));
        let claim = trace::scope(
            profile.clone(),
            fixture.media.claim(now, &mut Default::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claim.aid, upload.aid);
        let checked =
            crate::test_support::assert_outbox_claim_plans(&fixture.db, &profile.snapshot())
                .await
                .unwrap();
        assert_eq!(checked, 2);
        fixture._services.cleanup_ref().await.unwrap();
    }

    #[tokio::test]
    async fn stale_leases_cannot_start_reject_or_retry_an_upload() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let upload = fixture
            .upload(&identity::random_token(), "invalid.png", b"invalid image")
            .await
            .unwrap();
        let now = identity::now();
        let claim = fixture
            .media
            .claim(now, &mut Default::default())
            .await
            .unwrap()
            .unwrap();
        crate::db::query("UPDATE gonof_common_outbox SET lease_token=? WHERE eventid=?")
            .bind(identity::random_token())
            .bind(claim.eventid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        assert!(fixture.media.process_claim(&claim, now).await.is_err());
        fixture
            .media
            .reject_claim(&claim, "late inspection result", now)
            .await
            .unwrap();
        fixture
            .media
            .finish_claim(&claim, Some("late retry"), now)
            .await
            .unwrap();
        let pending = fixture.media.upload(fixture.uid, upload.aid).await.unwrap();
        assert_eq!(pending.lifecycle, "pending");
        assert_eq!(pending.scan_status, "pending");
        let current = fixture
            .media
            .claim(now + 3600, &mut Default::default())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fixture.media.process_claim(&current, now + 3600).await,
            Err(MediaError::Validation(_))
        ));
        fixture
            .media
            .reject_claim(&current, "invalid image", now + 3600)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .media
                .upload(fixture.uid, upload.aid)
                .await
                .unwrap()
                .lifecycle,
            "rejected"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_reconciles_late_objects_and_unregistered_staging() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut image, image::ImageFormat::Png)
            .unwrap();
        let upload = fixture
            .upload(&identity::random_token(), "photo.png", image.get_ref())
            .await
            .unwrap();
        let (aborted, file) = fixture.media.staging_file().await.unwrap();
        drop(file);
        let source_count = || {
            std::fs::read_dir(fixture.media.directory.join("sources"))
                .unwrap()
                .count()
        };
        // The staging area is cleaned after one hour; the quarantine waits for the orphan period.
        fixture
            .media
            .cleanup_files(identity::now() + STAGING_SECONDS + 60)
            .await
            .unwrap();
        assert!(
            !aborted.exists(),
            "staged files left by an aborted upload go after an hour"
        );
        assert_eq!(source_count(), 1);
        fixture
            .media
            .cleanup_files(identity::now() + 8 * 86400)
            .await
            .unwrap();
        assert_eq!(
            source_count(),
            1,
            "an unprocessed upload still needs its source file"
        );
        assert!(
            std::fs::read_dir(fixture.media.directory.join("secrets"))
                .unwrap()
                .next()
                .is_some()
        );
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        let object = server
            .state
            .objects
            .lock()
            .unwrap()
            .iter()
            .next()
            .map(|(k, v)| (k.clone(), v.clone()))
            .unwrap();
        fixture.media.cancel(fixture.uid, upload.aid).await.unwrap();
        fixture
            .media
            .process_pending(identity::now() + 3600)
            .await
            .unwrap();
        assert!(server.state.objects.lock().unwrap().is_empty());
        server
            .state
            .objects
            .lock()
            .unwrap()
            .insert(object.0, object.1);
        fixture.media.cleanup(identity::now() + 7200).await.unwrap();
        assert!(server.state.objects.lock().unwrap().is_empty());
        fixture._services.cleanup_ref().await.unwrap();
    }

    // After the cleanup job lists its candidates and before it gets the row lock, the attachment may already have been
    // sent to purge and purged by the account deletion flow. It must not be set back to purging then: the media.purge
    // event is already sent and will not be queued again.
    #[tokio::test]
    async fn cleanup_leaves_an_attachment_purged_while_it_waited_for_the_row_lock() {
        // Only verifies MySQL row lock waits (SQLite write transactions queue as a whole and start only after getting
        // the lock, so they read the latest commit).
        crate::mysql_only!();
        use crate::debug::trace::{self, QuerySource, RequestProfile};
        use std::{sync::Arc, time::Duration};
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let aid = fixture
            .upload(&identity::random_token(), "orphan.png", b"orphan")
            .await
            .unwrap()
            .aid;
        let now = identity::now();
        crate::db::query("UPDATE gonof_forum_attachment_0 SET purge_after=? WHERE aid=?")
            .bind(now)
            .bind(aid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        let mut purger = fixture.db.begin_write().await.unwrap();
        crate::db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
            .bind(aid)
            .fetch_one(fixture.db.observe(&mut *purger))
            .await
            .unwrap();
        let profile = Arc::new(RequestProfile::new());
        let cleanup = trace::scope(profile.clone(), Box::pin(fixture.media.cleanup(now + 60)));
        let purge = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !profile.snapshot().iter().any(|r| {
                    r.source == QuerySource::Application
                        && r.sql
                            .ends_with("FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
                        && r.rows_returned == 0
                        && r.elapsed.is_zero()
                }) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the cleanup lists candidates before waiting for the attachment row lock");
            crate::db::query(
                "UPDATE gonof_forum_attachment_0 SET lifecycle='purged',updated_at=? WHERE aid=?",
            )
            .bind(now)
            .bind(aid)
            .execute(fixture.db.observe(&mut *purger))
            .await
            .unwrap();
            crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,status,dateline,available_at) VALUES(?,'media.purge','attachment',?,'{}','sent',?,?)")
                .bind(format!("media.purge:{aid}")).bind(aid).bind(now).bind(now)
                .execute(fixture.db.observe(&mut *purger)).await.unwrap();
            purger.commit().await.unwrap();
        };
        let (cleaned, ()) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(cleanup, purge)
        })
        .await
        .expect("the cleanup finishes once the attachment row is released");
        cleaned.unwrap();
        let lifecycle: String =
            crate::db::query_scalar("SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(aid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(
            lifecycle, "purged",
            "a purged attachment must not go back to purging"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }

    #[tokio::test]
    async fn upload_markdown_cannot_inject_an_additional_link() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let mut upload = fixture
            .upload(&identity::random_token(), "name.png", &png())
            .await
            .unwrap();
        upload.mime_type = "image/png".into();
        upload.alt_text = "](https://example.test/evil)\n![x\\] <tag> &amp;".into();
        let markdown = upload.markdown();
        assert!(!markdown.contains('\n'));
        let mut images = 0;
        let mut links = 0;
        for event in pulldown_cmark::Parser::new(&markdown) {
            match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) => {
                    images += 1;
                    assert_eq!(dest_url.as_ref(), format!("attachment:{}", upload.aid));
                }
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { .. }) => links += 1,
                _ => {}
            }
        }
        assert_eq!((images, links), (1, 0));
        fixture._services.cleanup_ref().await.unwrap();
    }

    async fn lifecycle(fixture: &Fixture, aid: i64) -> String {
        crate::db::query_scalar("SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=?")
            .bind(aid)
            .fetch_one(fixture.db.executor())
            .await
            .unwrap()
    }

    // A media job that fails consecutively up to the limit stops retrying automatically, and the
    // attachment says it is waiting for an administrator.
    #[tokio::test]
    async fn media_retries_stop_at_the_shared_limit() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        // Failing to write to object storage is a temporary failure, retried with back-off.
        server
            .state
            .fail_put
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        crate::db::query("UPDATE gonof_common_outbox SET attempts=? WHERE event_key=?")
            .bind(crate::outbox::MAX_ATTEMPTS - 1)
            .bind(format!("media.process:{aid}"))
            .execute(fixture.db.executor())
            .await
            .unwrap();
        let report = fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        assert_eq!(report.failed, 1);
        let row =
            crate::db::query("SELECT status,last_error FROM gonof_common_outbox WHERE event_key=?")
                .bind(format!("media.process:{aid}"))
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(db::text(&row, "status").unwrap(), "failed");
        assert!(crate::outbox::is_exhausted(
            row.try_get::<Option<String>, _>("last_error")
                .unwrap()
                .as_deref()
        ));
        let detail: String =
            crate::db::query_scalar("SELECT scan_detail FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(aid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(detail, "paused");
        assert_eq!(lifecycle(&fixture, aid).await, "pending");
        fixture._services.cleanup_ref().await.unwrap();
    }

    // Only images are allowed: a queued non-image attachment is not inspected; it is rejected as unsupported, and
    // nothing is left in object storage.
    #[tokio::test]
    async fn queued_non_image_uploads_are_rejected_as_unsupported() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        assert!(matches!(
            fixture
                .upload(&identity::random_token(), "notes.txt", b"plain text")
                .await,
            Err(MediaError::Validation(message)) if message == crate::media::unsupported_format()
        ));
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        // Pretend to be a file registered before uploads were limited to images and still queued.
        crate::db::query("UPDATE gonof_forum_attachment_0 SET filename='notes.txt' WHERE aid=?")
            .bind(aid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        let report = fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        assert_eq!(report.failed, 1);
        let row = crate::db::query(
            "SELECT lifecycle,scan_status,scan_detail FROM gonof_forum_attachment_0 WHERE aid=?",
        )
        .bind(aid)
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
        assert_eq!(db::text(&row, "lifecycle").unwrap(), "rejected");
        assert_eq!(db::text(&row, "scan_status").unwrap(), "rejected");
        assert_eq!(db::text(&row, "scan_detail").unwrap(), "unsupported");
        assert!(server.state.objects.lock().unwrap().is_empty());
        fixture._services.cleanup_ref().await.unwrap();
    }

    // One batch rotates among uploaders, so a large batch of uploads cannot block members who come later.
    #[tokio::test]
    async fn claims_rotate_between_uploaders_instead_of_strict_fifo() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        crate::db::query("INSERT INTO gonof_common_member(username,password,adminid,regdate) VALUES('mediaother','unusable-media-contract-password',1,1)").execute(fixture.db.executor()).await.unwrap();
        let other: i64 = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member WHERE username='mediaother'",
        )
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
        let mut heavy = Vec::new();
        for index in 0..3 {
            let upload = fixture
                .upload(
                    &identity::random_token(),
                    &format!("bulk-{index}.png"),
                    b"bulk",
                )
                .await
                .unwrap();
            heavy.push(upload.aid);
        }
        let actor = fixture.media.test_actor(other).await.unwrap();
        let late = fixture
            .upload_as(actor, &identity::random_token(), "late.png", b"late")
            .await
            .unwrap()
            .aid;
        let mut served = BTreeSet::new();
        let now = identity::now();
        let mut order = Vec::new();
        for _ in 0..4 {
            order.push(
                fixture
                    .media
                    .claim(now, &mut served)
                    .await
                    .unwrap()
                    .unwrap()
                    .aid,
            );
        }
        assert_eq!(order, vec![heavy[0], late, heavy[1], heavy[2]]);
        assert!(
            fixture
                .media
                .claim(now, &mut served)
                .await
                .unwrap()
                .is_none()
        );
        fixture._services.cleanup_ref().await.unwrap();
    }

    // Later jobs in a batch get leases from real time, so they do not expire on claim and get claimed again
    // by someone else.
    #[tokio::test]
    async fn claims_lease_from_the_real_clock_even_with_a_stale_batch_time() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        fixture
            .upload(&identity::random_token(), "late.png", b"late")
            .await
            .unwrap();
        let stale = identity::now() - 1000;
        let claim = fixture
            .media
            .claim(stale, &mut BTreeSet::new())
            .await
            .unwrap()
            .unwrap();
        let lease_until: i64 =
            crate::db::query_scalar("SELECT lease_until FROM gonof_common_outbox WHERE eventid=?")
                .bind(claim.eventid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert!(lease_until >= identity::now() + fixture.media.lease_seconds() - 5);
        assert!(
            fixture
                .media
                .claim(identity::now(), &mut BTreeSet::new())
                .await
                .unwrap()
                .is_none(),
            "a task with an unexpired lease cannot be claimed again"
        );
        fixture
            .media
            .finish_claim(&claim, Some("test backoff"), stale)
            .await
            .unwrap();
        let available_at: i64 =
            crate::db::query_scalar("SELECT available_at FROM gonof_common_outbox WHERE eventid=?")
                .bind(claim.eventid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert!(
            available_at > identity::now(),
            "the backoff counts from the real time"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }

    // When the subprocess gives no verdict, retry first and reject only after reaching the limit; verdicts
    // given normally apply as before.
    #[test]
    fn inconclusive_inspections_retry_before_they_reject() {
        assert!(matches!(
            read_inspection(true, Some(br#"{"error":"bad"}"#)),
            Inspected::Failed
        ));
        assert!(matches!(
            read_inspection(true, Some(br#"{"unavailable":"disk"}"#)),
            Inspected::Unavailable
        ));
        let passed =
            br#"{"mime_type":"image/png","width":2,"height":2,"filesize":4,"content_sha256":"00"}"#;
        assert!(matches!(
            read_inspection(true, Some(passed)),
            Inspected::Passed(_)
        ));
        for abnormal in [
            read_inspection(false, Some(passed)),
            read_inspection(true, None),
            read_inspection(true, Some(b"{\"mime_ty")),
            read_inspection(true, Some(b"{}")),
        ] {
            assert!(matches!(abnormal, Inspected::Abnormal(_)), "{abnormal:?}");
        }
        for attempts in 1..INSPECTION_ATTEMPTS {
            assert!(matches!(
                inconclusive(attempts, "processing timed out"),
                MediaError::Unavailable(_)
            ));
        }
        assert!(matches!(
            inconclusive(INSPECTION_ATTEMPTS, "processing timed out"),
            MediaError::Validation(_)
        ));
    }

    // The subprocess's own timeout is no verdict, like the parent's: written as `{"timeout":…}`, also under context
    // added on the way up, and read back as the timeout message. Other failures stay verdicts against the file.
    #[test]
    fn a_timeout_in_the_subprocess_is_no_verdict() {
        let written =
            |error: anyhow::Error| serde_json::to_vec(&verdict(Err(error)).unwrap()).unwrap();
        for error in [
            anyhow::Error::from(inspect::TimedOut),
            anyhow::Error::from(inspect::TimedOut)
                .context("encoding the animation")
                .context("inspecting the upload"),
        ] {
            let bytes = written(error);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(value.get("timeout").is_some(), "{value}");
            match read_inspection(true, Some(&bytes)) {
                Inspected::Abnormal(message) => {
                    assert_eq!(message, crate::i18n::current().media_check_timeout());
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            read_inspection(
                true,
                Some(&written(anyhow::anyhow!("the image has too many pixels")))
            ),
            Inspected::Failed
        ));
    }

    // The inspection running out of its own time is retried like the parent's timeout: the first two are retried and
    // counted in the payload, and only the third rejects the file, with the timeout as the reason.
    #[tokio::test]
    async fn the_inspections_own_timeout_is_retried_before_it_rejects() {
        // No time at all: the inspection's deadline trips at its first check, as the subprocess's would on a slow
        // machine.
        let fixture = Fixture::new(MediaConfig {
            processing_timeout_seconds: 0,
            ..MediaConfig::default()
        })
        .await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        let payload = || async {
            crate::db::query_scalar::<String>(
                "SELECT payload FROM gonof_common_outbox WHERE event_key=?",
            )
            .bind(format!("media.process:{aid}"))
            .fetch_one(fixture.db.executor())
            .await
            .unwrap()
        };
        let mut now = identity::now();
        for count in 1..INSPECTION_ATTEMPTS {
            now += 7200;
            fixture.media.process_pending(now).await.unwrap();
            assert_eq!(lifecycle(&fixture, aid).await, "pending");
            let payload = payload().await;
            assert!(
                payload.contains(&format!("\"no_verdict\":{count}")),
                "{payload}"
            );
        }
        now += 7200;
        fixture.media.process_pending(now).await.unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "rejected");
        let detail: String =
            crate::db::query_scalar("SELECT scan_detail FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(aid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(detail, "timeout");
        fixture._services.cleanup_ref().await.unwrap();
    }

    // A failure writing the processed output is the disk's, not the file's: the job retries it as a temporary
    // failure, and the image passes once the disk works again.
    #[tokio::test]
    async fn output_write_failures_retry_instead_of_rejecting() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        let now = identity::now();
        let claim = fixture
            .media
            .claim(now, &mut Default::default())
            .await
            .unwrap()
            .unwrap();
        // The output path is taken by a directory, so creating the output file fails.
        let blocked = fixture
            .media
            .directory
            .join("processed")
            .join(format!("{aid}-{}", claim.lease));
        std::fs::create_dir_all(&blocked).unwrap();
        let mut report = MediaReport::default();
        fixture
            .media
            .run_claim(&claim, now, &mut report)
            .await
            .unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "pending");
        let row =
            crate::db::query("SELECT status,payload FROM gonof_common_outbox WHERE event_key=?")
                .bind(format!("media.process:{aid}"))
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(db::text(&row, "status").unwrap(), "pending");
        assert!(
            !db::text(&row, "payload").unwrap().contains("no_verdict"),
            "a disk failure is not an inspection without a verdict"
        );
        std::fs::remove_dir_all(&blocked).unwrap();
        fixture.media.process_pending(now + 3600).await.unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "ready");
        fixture._services.cleanup_ref().await.unwrap();
    }

    // Claims that decoded the image and then failed to store it do not count toward the inspections allowed to give
    // no verdict: only the third timeout rejects the file, and an administrator's retry starts the count over.
    #[tokio::test]
    async fn storage_failures_do_not_use_up_the_inspection_allowance() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        let key = format!("media.process:{aid}");
        let event = |fixture: &Fixture| {
            let key = key.clone();
            let db = fixture.db.clone();
            async move {
                crate::db::query("SELECT eventid,status,attempts,available_at,lease_until,payload FROM gonof_common_outbox WHERE event_key=?")
                    .bind(key)
                    .fetch_one(db.executor())
                    .await
                    .unwrap()
            }
        };
        let mut now = identity::now();
        let mut round = |set: u32| {
            NO_VERDICT.with(|left| left.set(set));
            now += 7200;
            now
        };
        // Two claims decode the image and fail at the storage.
        server
            .state
            .fail_put
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..2 {
            let at = round(0);
            fixture.media.process_pending(at).await.unwrap();
        }
        server
            .state
            .fail_put
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            event(&fixture).await.try_get::<i64, _>("attempts").unwrap(),
            2
        );
        // The first timeout is retried.
        let at = round(1);
        fixture.media.process_pending(at).await.unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "pending");
        assert_eq!(
            db::text(&event(&fixture).await, "status").unwrap(),
            "pending"
        );
        // So is the second; the job then runs out of attempts and waits for an administrator.
        let at = round(1);
        fixture.media.process_pending(at).await.unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "pending");
        crate::db::query("UPDATE gonof_common_outbox SET attempts=? WHERE event_key=?")
            .bind(crate::outbox::MAX_ATTEMPTS - 1)
            .bind(&key)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        server
            .state
            .fail_put
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let at = round(0);
        fixture.media.process_pending(at).await.unwrap();
        server
            .state
            .fail_put
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let row = event(&fixture).await;
        assert_eq!(db::text(&row, "status").unwrap(), "failed");
        assert!(
            db::text(&row, "payload")
                .unwrap()
                .contains("\"no_verdict\":2")
        );
        let actor = fixture.media.test_actor(fixture.uid).await.unwrap();
        crate::administration::retry_task(
            &fixture.db,
            actor,
            row.try_get("eventid").unwrap(),
            row.try_get("attempts").unwrap(),
            row.try_get("available_at").unwrap(),
            row.try_get("lease_until").unwrap(),
            "failed",
            "storage restored",
        )
        .await
        .unwrap();
        let payload = db::text(&event(&fixture).await, "payload").unwrap();
        assert!(!payload.contains("no_verdict"), "{payload}");
        assert!(payload.contains("original_size"), "{payload}");
        // After the retry two more timeouts are retried, and the third rejects the file.
        for _ in 0..2 {
            let at = round(1);
            fixture.media.process_pending(at).await.unwrap();
            assert_eq!(lifecycle(&fixture, aid).await, "pending");
        }
        let at = round(1);
        fixture.media.process_pending(at).await.unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "rejected");
        fixture._services.cleanup_ref().await.unwrap();
    }

    // A process that stops right after the purge commits still leaves the re-check scheduled: marking the attachment
    // purged and rescheduling its event are one transaction, so an object a late upload writes back is still removed.
    #[tokio::test]
    async fn a_purge_interrupted_after_it_commits_still_reconciles() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap()
            .aid;
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        let objects = server.state.objects.lock().unwrap().clone();
        assert!(!objects.is_empty());
        fixture.media.cancel(fixture.uid, aid).await.unwrap();
        let now = identity::now();
        let claim = fixture
            .media
            .claim(now, &mut Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.kind, "media.purge");
        assert!(fixture.media.purge_claim(&claim, now).await.unwrap());
        assert_eq!(lifecycle(&fixture, aid).await, "purged");
        // The process stops here, and a late upload writes the objects back.
        server.state.objects.lock().unwrap().extend(objects);
        fixture.media.cleanup(now + 7200).await.unwrap();
        assert!(
            server.state.objects.lock().unwrap().is_empty(),
            "the re-check removes the late objects"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }

    // After a purge completes, the objects are re-checked only once; later cleanup rounds do not resend DELETE.
    #[tokio::test]
    async fn purged_attachments_are_reconciled_once_and_then_left_alone() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut image, image::ImageFormat::Png)
            .unwrap();
        let aid = fixture
            .upload(&identity::random_token(), "photo.png", image.get_ref())
            .await
            .unwrap()
            .aid;
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        fixture.media.cancel(fixture.uid, aid).await.unwrap();
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        assert_eq!(lifecycle(&fixture, aid).await, "purged");
        let deletes = || {
            server
                .state
                .deletes
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        // The original and the thumbnail are each deleted once.
        assert_eq!(deletes(), 2);
        let row =
            crate::db::query("SELECT status,payload FROM gonof_common_outbox WHERE event_key=?")
                .bind(format!("media.purge:{aid}"))
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(
            db::text(&row, "status").unwrap(),
            "pending",
            "a finished purge schedules one re-check"
        );
        let now = identity::now();
        fixture.media.cleanup(now + 3600).await.unwrap();
        assert_eq!(deletes(), 4, "the due re-check runs once (two objects)");
        let status: String =
            crate::db::query_scalar("SELECT status FROM gonof_common_outbox WHERE event_key=?")
                .bind(format!("media.purge:{aid}"))
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(status, "sent");
        for later in [now + 7200, now + 86400, now + 2 * 86400] {
            fixture.media.cleanup(later).await.unwrap();
        }
        assert_eq!(deletes(), 4, "no DELETE is resent after the re-check");
        fixture._services.cleanup_ref().await.unwrap();
    }

    // Sweeping quarantined files looks up processing events by aid via the unique key instead of reading all
    // in-progress events. The aids come from attachments being processed, not from file names, and the built keys
    // are fetched in `IN` batches, still looking up by key only.
    #[tokio::test]
    async fn source_sweep_looks_up_each_file_by_its_event_key() {
        use crate::debug::trace::{self, QuerySource, RequestProfile};
        use std::sync::Arc;
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        fixture
            .upload(&identity::random_token(), "kept.png", b"kept")
            .await
            .unwrap();
        let profile = Arc::new(RequestProfile::new());
        trace::scope(
            profile.clone(),
            fixture.media.cleanup_files(identity::now() + 8 * 86400),
        )
        .await
        .unwrap();
        let sources = std::fs::read_dir(fixture.media.directory.join("sources"))
            .unwrap()
            .count();
        assert_eq!(sources, 1, "the source file of a pending upload is kept");
        let outbox: Vec<_> = profile
            .snapshot()
            .into_iter()
            .filter(|r| {
                r.source == QuerySource::Application && r.sql.contains("gonof_common_outbox")
            })
            .collect();
        assert!(!outbox.is_empty());
        assert!(
            outbox
                .iter()
                .all(|r| r.sql.contains("event_key=?") || r.sql.contains("event_key IN (")),
            "the sweep must not list every processing event: {:?}",
            outbox.iter().map(|r| &r.sql).collect::<Vec<_>>()
        );
        fixture._services.cleanup_ref().await.unwrap();
    }
    // The file is written to disk before the database row, and the source file name has nothing to do with the aid.
    // Neither a replay nor a failed database write leaves the pre-written file behind, and the sweep identifies old
    // unreferenced files from the database.
    #[tokio::test]
    async fn sources_are_written_before_the_transaction_and_swept_by_database_references() {
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        let directory = fixture.media.directory.join("sources");
        let sources = || {
            let mut names: Vec<String> = std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let key = identity::random_token();
        let upload = fixture.upload(&key, "first.png", &png()).await.unwrap();
        let names = sources();
        assert_eq!(names.len(), 1);
        assert!(
            !names[0].starts_with(&format!("{}-", upload.aid)),
            "source file names do not contain the aid: {names:?}"
        );
        assert_eq!(
            fixture.upload(&key, "first.png", &png()).await.unwrap().aid,
            upload.aid,
            "replaying a request returns the original attachment"
        );
        assert_eq!(sources(), names, "a replay removes the file written ahead");
        assert!(
            fixture
                .upload(&key, "first.png", &png_sized(3, 3))
                .await
                .is_err(),
            "the same request ID with other content is rejected"
        );
        assert_eq!(
            sources(),
            names,
            "a failed database write removes the file written ahead"
        );
        // When the commit response fails and the commit did not actually succeed, an unreferenced file is left behind.
        let stray = directory.join(identity::random_token());
        std::fs::write(&stray, b"stray").unwrap();
        fixture
            .media
            .cleanup_files(identity::now() + 8 * 86400)
            .await
            .unwrap();
        assert!(
            !stray.exists(),
            "an old unreferenced file is removed once expired"
        );
        assert_eq!(
            sources(),
            names,
            "the source file of an attachment in processing is kept"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }
}
