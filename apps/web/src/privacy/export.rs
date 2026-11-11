use super::worker::{Claim, finish_task, locked};
use super::*;
use crate::db::Tx;
use crate::media::Media;
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn json_row(row: &DbRow) -> anyhow::Result<serde_json::Value> {
    json_row_without(row, None)
}

/// Same as `json_row`, but skips one column; the extra grouping key of batch queries does not go into the export file.
fn json_row_without(row: &DbRow, skip: Option<&str>) -> anyhow::Result<serde_json::Value> {
    let mut object = serde_json::Map::new();
    for column in row.columns() {
        let name = column.name();
        if Some(name) == skip {
            continue;
        }
        let value = if column.is_integer() {
            serde_json::to_value(row.try_get::<Option<i64>, _>(name)?)?
        } else {
            serde_json::to_value(row.try_get::<Option<String>, _>(name)?)?
        };
        object.insert(name.into(), value);
    }
    Ok(serde_json::Value::Object(object))
}
enum ArchiveCommand {
    Entry(String),
    Bytes(Vec<u8>),
    Finalize,
}

#[derive(Debug)]
struct ArchiveOutcome {
    bytes: i64,
    hash: String,
}

/// Byte limit per send; together with the channel capacity it determines the in-flight memory.
const ARCHIVE_FLUSH_BYTES: usize = 256 * 1024;

/// Assembles the export archive on a dedicated blocking thread.
///
/// Deflate compression, writing to disk and the final digest are all CPU and disk work that would occupy
/// a Tokio worker thread for the whole export — archiving a large account takes tens of seconds. Here the whole
/// archive is moved to a `spawn_blocking` thread; the async side only sends entry names and byte chunks, while the
/// database snapshot and S3 reads still yield normally on the worker threads.
///
/// The channel is bounded, so the sending side is naturally subject to backpressure. If the async side aborts
/// before reaching `finish`, the channel closes and the blocking thread exits directly without finish/fsync; the
/// half-finished directory is deleted by `Partial`.
struct Archive {
    sender: tokio::sync::mpsc::Sender<ArchiveCommand>,
    worker: tokio::task::JoinHandle<anyhow::Result<ArchiveOutcome>>,
    pending: Vec<u8>,
}

impl Archive {
    /// `path`, `workdir` and `dir` let the blocking thread re-read the finished file and fsync each level when
    /// wrapping up.
    fn create(file: std::fs::File, path: PathBuf, workdir: PathBuf, dir: PathBuf) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let worker = tokio::task::spawn_blocking(move || {
            let mut zip = ZipWriter::new(file);
            let mut finalized = false;
            while let Some(command) = receiver.blocking_recv() {
                match command {
                    ArchiveCommand::Entry(name) => zip.start_file(
                        name,
                        SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated)
                            .unix_permissions(0o600),
                    )?,
                    ArchiveCommand::Bytes(bytes) => zip.write_all(&bytes)?,
                    ArchiveCommand::Finalize => {
                        finalized = true;
                        break;
                    }
                }
            }
            // Not receiving the finish instruction means the async side has already errored and exited; no finished
            // file is produced, and Partial cleans up.
            ensure!(
                finalized,
                "{}",
                crate::i18n::current().governance_error_export_unfinished()
            );
            let file = zip.finish()?;
            file.sync_all()?;
            let bytes = file.metadata()?.len();
            drop(file);
            let mut file = std::fs::File::open(&path)?;
            let mut hash = Sha256::new();
            let mut buffer = [0_u8; 65536];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
            }
            let hash = format!("{:x}", hash.finalize());
            std::fs::File::open(&workdir)?.sync_all()?;
            std::fs::File::open(&dir)?.sync_all()?;
            Ok(ArchiveOutcome {
                bytes: bytes as i64,
                hash,
            })
        });
        Self {
            sender,
            worker,
            pending: Vec::new(),
        }
    }

    /// An error on the blocking thread closes the channel first; replace the send failure with the thread's real error
    /// before reporting it.
    async fn send(&mut self, command: ArchiveCommand) -> anyhow::Result<()> {
        if self.sender.send(command).await.is_ok() {
            return Ok(());
        }
        Err(match (&mut self.worker).await {
            Ok(Err(error)) => error,
            Ok(Ok(_)) => anyhow::anyhow!("export archive thread ended early"),
            Err(error) => {
                anyhow::Error::new(error).context("export archive thread exited abnormally")
            }
        })
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.pending);
        self.send(ArchiveCommand::Bytes(bytes)).await
    }

    async fn flush_when_full(&mut self) -> anyhow::Result<()> {
        if self.pending.len() >= ARCHIVE_FLUSH_BYTES {
            self.flush().await
        } else {
            Ok(())
        }
    }

    /// A new entry must come after the already buffered bytes, or its content would land in the previous entry.
    async fn entry(&mut self, name: &str) -> anyhow::Result<()> {
        self.flush().await?;
        self.send(ArchiveCommand::Entry(name.to_owned())).await
    }

    async fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.pending.extend_from_slice(bytes);
        self.flush_when_full().await
    }

    async fn write_json(&mut self, value: &serde_json::Value) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.pending, value)?;
        self.flush_when_full().await
    }

    async fn finish(mut self) -> anyhow::Result<ArchiveOutcome> {
        self.flush().await?;
        self.send(ArchiveCommand::Finalize).await?;
        drop(self.sender);
        self.worker
            .await
            .context("export archive thread exited abnormally")?
    }
}

async fn table(
    db: &Database,
    tx: &mut Tx<'_>,
    archive: &mut Archive,
    name: &str,
    sql: &str,
    key: &str,
    uid: i64,
) -> anyhow::Result<()> {
    archive.entry(&format!("{name}.json")).await?;
    archive.write(b"[\n").await?;
    let mut before = 0;
    let mut first = true;
    loop {
        let rows = db::query(sql)
            .bind(uid)
            .bind(before)
            .fetch_all(db.observe(&mut **tx))
            .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            if !first {
                archive.write(b",\n").await?;
            }
            first = false;
            archive.write_json(&json_row(row)?).await?;
        }
        before = rows.last().unwrap().try_get(key)?;
    }
    archive.write(b"\n]\n").await?;
    Ok(())
}

/// Batches of invite codes generated in bulk by administrators (`gonof_common_invite_batch`): count and time,
/// without the request identifier or digest. The primary key is (uid, request identifier), with no integer key, so
/// pages go by (time, request identifier).
async fn invite_batches(
    db: &Database,
    tx: &mut Tx<'_>,
    archive: &mut Archive,
    uid: i64,
) -> anyhow::Result<()> {
    archive.entry("invite_batches.json").await?;
    archive.write(b"[\n").await?;
    let mut before = (i64::MIN, String::new());
    let mut first = true;
    loop {
        let rows = db::query(
            "SELECT request_key,invites,dateline FROM gonof_common_invite_batch WHERE uid=?
               AND (dateline>? OR (dateline=? AND request_key>?)) ORDER BY dateline,request_key LIMIT 100",
        )
        .bind(uid)
        .bind(before.0)
        .bind(before.0)
        .bind(&before.1)
        .fetch_all(db.observe(&mut **tx))
        .await?;
        let Some(last) = rows.last() else { break };
        before = (last.try_get("dateline")?, db::text(last, "request_key")?);
        for row in &rows {
            if !first {
                archive.write(b",\n").await?;
            }
            first = false;
            let entry = serde_json::json!({
                "invites": row.try_get::<i64, _>("invites")?,
                "dateline": row.try_get::<i64, _>("dateline")?,
            });
            archive.write_json(&entry).await?;
        }
    }
    archive.write(b"\n]\n").await?;
    Ok(())
}

/// At most this many revision rows per page; posts beyond that fall back to per-post paging.
const REVISION_BATCH: usize = 1000;

/// Revision records for a page of posts.
///
/// A whole page of posts is fetched at once, in (pid, version) primary key order, at most `REVISION_BATCH` rows; one
/// query per post (N+1) inside the export snapshot transaction would stretch the snapshot out for accounts with many
/// posts. When full, the post of the last row may be incomplete: it and the posts after it are still paged per post,
/// so queries are always bounded.
async fn post_revisions(
    db: &Database,
    tx: &mut Tx<'_>,
    archive: &mut Archive,
    posts: &[DbRow],
) -> anyhow::Result<()> {
    let pids = posts
        .iter()
        .map(|p| p.try_get::<i64, _>("pid"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let sql = format!(
        "SELECT version,subject,message,dateline,pid FROM gonof_forum_post_revision WHERE pid IN ({}) ORDER BY pid,version LIMIT {}",
        vec!["?"; pids.len()].join(","),
        REVISION_BATCH + 1
    );
    let mut query = db::query(&sql);
    for pid in &pids {
        query = query.bind(*pid);
    }
    let rows = query.fetch_all(db.observe(&mut **tx)).await?;
    // When full, the post of row REVISION_BATCH+1 and the posts after it are incomplete.
    let complete_below = match rows.get(REVISION_BATCH) {
        Some(row) => row.try_get::<i64, _>("pid")?,
        None => i64::MAX,
    };
    let mut grouped: std::collections::BTreeMap<i64, Vec<&DbRow>> = Default::default();
    for row in rows.iter().take(REVISION_BATCH) {
        grouped.entry(row.try_get("pid")?).or_default().push(row);
    }
    for p in posts {
        let pid: i64 = p.try_get("pid")?;
        archive.entry(&format!("markdown/post-{pid}.md")).await?;
        archive.write(db::text(p, "message")?.as_bytes()).await?;
        let name = format!("revisions/post-{pid}");
        if pid >= complete_below {
            table(db,tx,archive,&name,"SELECT version,subject,message,dateline FROM gonof_forum_post_revision WHERE pid=? AND version>? ORDER BY version LIMIT 100","version",pid).await?;
            continue;
        }
        archive.entry(&format!("{name}.json")).await?;
        archive.write(b"[\n").await?;
        for (index, row) in grouped.get(&pid).into_iter().flatten().enumerate() {
            if index > 0 {
                archive.write(b",\n").await?;
            }
            archive
                .write_json(&json_row_without(row, Some("pid"))?)
                .await?;
        }
        archive.write(b"\n]\n").await?;
    }
    Ok(())
}
struct Partial {
    path: PathBuf,
    keep: bool,
}
impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
pub(super) async fn build(
    db: &Database,
    media: &Media,
    c: &Claim,
    dir: &Path,
    words: &super::worker::Words,
    now: i64,
) -> anyhow::Result<()> {
    let key = format!("request-{}-{}", c.request_id, c.lease);
    let workdir = dir.join(&key);
    std::fs::create_dir(&workdir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&workdir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = workdir.join("archive.zip");
    let mut cleanup = Partial {
        path: workdir.clone(),
        keep: false,
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut gate = db.begin_write().await?;
    let current = locked(db, &mut gate, c, identity::now().max(now)).await?;
    ensure!(
        current.status == "processing" && active_member(db, &mut gate, c.uid).await?,
        "export request invalidated"
    );
    // Last online is written back by the cleanup job (docs/queries.md, "Last online"): before exporting, first write
    // back the last activity time from the member's own online table entry.
    identity::Identity::flush_presence(db, &mut gate, c.uid).await?;
    let file = options.open(&path)?;
    let mut archive = Archive::create(file, path, workdir.clone(), dir.to_owned());
    gate.commit().await?;
    // One repeatable-read snapshot captures database rows. No S3 call or SQL write occurs in it.
    let mut tx = db.begin_snapshot().await?;
    let profile=db::query("SELECT uid,username,email,emailstatus,mobile,bio,regdate,space_visibility FROM gonof_common_member WHERE uid=? AND account_state='active'").bind(c.uid).fetch_optional(db.observe(&mut *tx)).await?.context("export account unavailable")?;
    archive.entry("profile.json").await?;
    archive.write_json(&json_row(&profile)?).await?;
    for (name, sql, key) in [
        (
            "thread_follows",
            "SELECT id,tid,dateline FROM gonof_home_thread_follow WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "forum_follows",
            "SELECT id,fid,dateline FROM gonof_home_forum_follow WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "blocked_members",
            "SELECT blocked_uid,dateline FROM gonof_home_member_block WHERE uid=? AND blocked_uid>? ORDER BY blocked_uid LIMIT 100",
            "blocked_uid",
        ),
        (
            // Export only the members this member follows, not the follower list (that is other people's behavior).
            "member_follows",
            "SELECT followed_uid,dateline FROM gonof_home_member_follow WHERE uid=? AND followed_uid>? ORDER BY followed_uid LIMIT 100",
            "followed_uid",
        ),
        (
            "forum_filters",
            "SELECT fid,filter_mode,dateline FROM gonof_home_forum_filter WHERE uid=? AND fid>? ORDER BY fid LIMIT 100",
            "fid",
        ),
        (
            // Export only whether it is enabled, the generation time and the switch, without the token digest or hint.
            "feed_token",
            "SELECT uid,include_members,dateline FROM gonof_home_feed_token WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            // Two-factor authentication exports only whether it is enabled and the time; remaining recovery codes are
            // one row each with only the generation time; no secret, digests or failure count.
            "two_factor",
            "SELECT uid,enabled_at,totp_enabled_at FROM gonof_common_member_two_factor WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            // Third-party bindings: provider, external ID, display name, bound and last
            // sign-in times. At most two rows per member, so the first page gets them all (the next page starts after
            // the member's own uid and has no rows).
            "external_accounts",
            "SELECT uid,provider,subject,display,dateline,last_login_at FROM gonof_common_member_external WHERE uid=? AND uid>? ORDER BY uid,provider LIMIT 100",
            "uid",
        ),
        (
            "recovery_codes",
            "SELECT uid,dateline FROM gonof_common_member_recovery_code WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "notification_preferences",
            "SELECT uid,site_reply,site_quote,site_mention,site_thread,site_forum,email_reply,email_quote,email_mention,email_thread,email_forum,email_message,email_login FROM gonof_home_notification_preference WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "reading_preferences",
            "SELECT uid,list_sort,thread_view,title_latest,title_new_tab,avatar_shape,custom_css,custom_css_version,updated_at FROM gonof_home_reading_preference WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "data_requests",
            "SELECT id,kind,content_mode,status,dateline,execute_after,finished_at FROM gonof_common_data_request WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "threads",
            "SELECT tid,fid,subject,dateline,category,visibility,deleted_at FROM gonof_forum_thread WHERE authorid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "posts",
            "SELECT pid,tid,fid,first,position,root_pid,reply_to_pid,message,dateline,deleted_at,erased_at FROM gonof_forum_post WHERE authorid=? AND pid>? ORDER BY pid LIMIT 100",
            "pid",
        ),
        (
            "sent_messages",
            "SELECT m.id,m.thread_id,m.sender_uid,p.counterpart_uid,m.body,m.dateline FROM gonof_home_message m JOIN gonof_home_message_member p ON p.thread_id=m.thread_id AND p.uid=m.sender_uid WHERE m.sender_uid=? AND m.id>? ORDER BY m.id LIMIT 100",
            "id",
        ),
        (
            "notifications",
            "SELECT id,authorid,pid,type,dateline,read_at,reason FROM gonof_home_notification WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "invites",
            "SELECT id,dateline,endtime,status,regdateline FROM gonof_common_invite WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            // Registration source: one row per member, so the first page gets it all.
            "registration_source",
            "SELECT uid,site,channel,inviter_uid,invite_prefix FROM gonof_common_member_register WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "reports",
            "SELECT id,target_type,target_id,reason,status,resolution,dateline,handled_at FROM gonof_common_report WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "sanctions",
            "SELECT id,kind,reason,starts_at,expires_at,revoked_at FROM gonof_common_member_sanction WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "appeals",
            "SELECT id,sanction_id,reason,status,resolution,dateline,handled_at FROM gonof_common_member_appeal WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "restorations",
            "SELECT id,pid,state,reason,status,resolution,dateline,handled_at FROM gonof_forum_restore_request WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "credits",
            "SELECT id,available_delta,frozen_delta,available_after,frozen_after,kind,tid,pid,reason,dateline FROM gonof_common_credit_ledger WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "presence",
            "SELECT uid,last_seen FROM gonof_common_member_presence WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "growth",
            "SELECT uid,growth FROM gonof_common_growth_account WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            "growth_ledger",
            "SELECT id,delta,growth_after,kind,pid,reason,dateline FROM gonof_common_growth_ledger WHERE uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            // Verification exports only what the member can see, without the granting administrator.
            "verification",
            "SELECT uid,category,title,site_url,granted_at,reviewed_at,expires_at FROM gonof_common_member_verification WHERE uid=? AND uid>? ORDER BY uid LIMIT 100",
            "uid",
        ),
        (
            // Honor groups: group name and validity period, without the granting administrator (same as verification).
            "honors",
            "SELECT g.honor_id,h.name,g.granted_at,g.expires_at FROM gonof_common_member_honor g JOIN gonof_common_honor h ON h.id=g.honor_id WHERE g.uid=? AND g.honor_id>? ORDER BY g.honor_id LIMIT 100",
            "honor_id",
        ),
        (
            // All link exchange applications and links the member submitted, without the handling administrator.
            "friend_links",
            "SELECT id,status,category,name,url,domain,description,backlink_url,note,home,resolution,dateline,handled_at FROM gonof_common_friend_link WHERE owner_uid=? AND id>? ORDER BY id LIMIT 100",
            "id",
        ),
        (
            "favorites",
            "SELECT tid,dateline FROM gonof_home_favorite WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "reactions",
            "SELECT pid,dateline FROM gonof_forum_post_reaction WHERE uid=? AND pid>? ORDER BY pid LIMIT 100",
            "pid",
        ),
        (
            "downvotes",
            "SELECT pid,dateline FROM gonof_forum_post_downvote WHERE uid=? AND pid>? ORDER BY pid LIMIT 100",
            "pid",
        ),
        (
            "attachments",
            "SELECT a.aid,d.filename,d.filesize,d.mime_type,d.description,d.alt_text,d.dateline,d.lifecycle,d.scan_status,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND a.aid>? ORDER BY a.aid LIMIT 100",
            "aid",
        ),
        (
            "activities",
            "SELECT tid,kind,budget,remaining,deadline,state,created_at,settled_at FROM gonof_forum_activity WHERE owner_uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "lottery_entries",
            "SELECT tid,source_pid,amount,dateline FROM gonof_forum_lottery_entry WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "lottery_signups",
            "SELECT tid,amount,dateline FROM gonof_forum_lottery_signup WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "lottery_exclusions",
            "SELECT tid,reason,dateline FROM gonof_forum_lottery_exclusion WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        // Redemption codes never go into any export; here there are only prize names and types.
        (
            "lottery_awards",
            "SELECT a.tid,a.prize_no,p.name,p.kind,a.assigned_at FROM gonof_forum_lottery_award a JOIN gonof_forum_lottery_prize p ON p.tid=a.tid AND p.prize_no=a.prize_no WHERE a.uid=? AND a.tid>? ORDER BY a.tid LIMIT 100",
            "tid",
        ),
        (
            "redpacket_claims",
            "SELECT tid,source_pid,amount,dateline FROM gonof_forum_redpacket_claim WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
        (
            "poll_votes",
            "SELECT tid,choices FROM gonof_forum_poll_vote WHERE uid=? AND tid>? ORDER BY tid LIMIT 100",
            "tid",
        ),
    ] {
        table(db, &mut tx, &mut archive, name, sql, key, c.uid).await?;
    }
    invite_batches(db, &mut tx, &mut archive, c.uid).await?;
    let mut before = 0;
    loop {
        let posts=db::query("SELECT pid,message FROM gonof_forum_post WHERE authorid=? AND pid>? AND erased_at=0 ORDER BY pid LIMIT 100").bind(c.uid).bind(before).fetch_all(db.observe(&mut *tx)).await?;
        if posts.is_empty() {
            break;
        }
        post_revisions(db, &mut tx, &mut archive, &posts).await?;
        before = posts.last().unwrap().try_get("pid")?;
    }
    // Spill the safe-attachment snapshot to disk; large accounts do not build an in-memory list.
    let manifest_path = workdir.join("attachments.manifest");
    let mut manifest_options = std::fs::OpenOptions::new();
    manifest_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        manifest_options.mode(0o600);
    }
    // The manifest has one attachment per row; a bare File would issue one write system call per row.
    let mut manifest = std::io::BufWriter::new(manifest_options.open(&manifest_path)?);
    before = 0;
    loop {
        let rows=db::query("SELECT a.aid,d.filesize,d.content_sha256 FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND a.aid>? AND d.lifecycle='ready' AND d.scan_status='clean' ORDER BY a.aid LIMIT 100").bind(c.uid).bind(before).fetch_all(db.observe(&mut *tx)).await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            writeln!(
                manifest,
                "{} {} {}",
                row.try_get::<i64, _>("aid")?,
                row.try_get::<i64, _>("filesize")?,
                db::text(row, "content_sha256")?
            )?;
        }
        before = rows.last().unwrap().try_get("aid")?;
    }
    let manifest = manifest
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    manifest.sync_all()?;
    drop(manifest);
    tx.commit().await?;
    use std::io::BufRead;
    let mut missing = Vec::new();
    for line in std::io::BufReader::new(std::fs::File::open(&manifest_path)?).lines() {
        let line = line?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        ensure!(fields.len() == 3, "invalid attachment snapshot");
        let aid: i64 = fields[0].parse()?;
        let expected: i64 = fields[1].parse()?;
        let download = match media.export_owned(aid, c.uid).await {
            Ok(download) => download,
            // After the snapshot the attachment was purged, or the object in storage is lost: retrying cannot get it
            // either, so skip it and list it in the archive; the whole export must not retry forever.
            Err(crate::media::MediaError::NotFound) => {
                tracing::warn!(
                    aid,
                    uid = c.uid,
                    "attachment no longer exportable or missing in storage, skipped"
                );
                missing.push(aid);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let mut body = download.body.into_async_read();
        let mut buffer = [0_u8; 65536];
        let mut size = 0_i64;
        let mut hash = Sha256::new();
        archive.entry(&format!("attachments/{aid}.bin")).await?;
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut body, &mut buffer).await?;
            if n == 0 {
                break;
            }
            size += n as i64;
            ensure!(size <= expected, "attachment exceeded recorded length");
            hash.update(&buffer[..n]);
            archive.write(&buffer[..n]).await?;
        }
        ensure!(
            size == expected && format!("{:x}", hash.finalize()) == fields[2],
            "attachment changed during export"
        );
    }
    std::fs::remove_file(&manifest_path)?;
    if !missing.is_empty() {
        archive.entry("attachments-missing.json").await?;
        archive.write_json(&serde_json::json!(missing)).await?;
        archive.write(b"\n").await?;
    }
    archive.entry("README.txt").await?;
    // The read-me follows the site language and names the site (a background task: no request).
    let readme = format!("{}\n", words.lang().governance_export_readme(words.site()));
    archive.write(readme.as_bytes()).await?;
    // finish, fsync and the final digest all happen on the archive thread; the manifest has already been deleted, so
    // the directory fsync reaches the final state.
    let ArchiveOutcome { bytes, hash } = archive.finish().await?;
    let mut tx = db.begin_write().await?;
    let r = locked(db, &mut tx, c, identity::now().max(now)).await?;
    ensure!(
        r.status == "processing" && active_member(db, &mut tx, c.uid).await?,
        "export request no longer available"
    );
    db::query("UPDATE gonof_common_data_request SET status='ready',stage='done',artifact_key=?,artifact_hash=?,artifact_bytes=?,artifact_expires_at=?,finished_at=?,last_error='',version=version+1 WHERE id=? AND uid=?").bind(key).bind(hash).bind(bytes).bind(now+EXPORT_SECONDS).bind(now).bind(c.request_id).bind(c.uid).execute(db.observe(&mut *tx)).await?;
    finish_task(db, &mut tx, c, true, now).await?;
    tx.commit().await?;
    cleanup.keep = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let path = std::env::temp_dir().join(format!("gonof-export-{}", identity::random_token()));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    /// The archive thread writes in send order, so entry boundaries must not be broken up by flush chunking; the
    /// finished byte count and digest directly determine artifact_bytes / artifact_hash and must match the file on disk.
    #[tokio::test]
    async fn archive_keeps_entry_boundaries_across_flushes_and_reports_the_file_digest() {
        let root = temp_dir();
        let workdir = root.0.join("work");
        std::fs::create_dir(&workdir).unwrap();
        let path = workdir.join("archive.zip");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut archive = Archive::create(file, path.clone(), workdir.clone(), root.0.clone());
        // The body crosses the flush threshold twice and is not a whole chunk; confirm the remainder does not leak into
        // the next entry.
        let large: Vec<u8> = (0..ARCHIVE_FLUSH_BYTES * 2 + 7).map(|i| i as u8).collect();
        archive.entry("first.bin").await.unwrap();
        archive.write(&large).await.unwrap();
        archive.entry("second.json").await.unwrap();
        archive
            .write_json(&serde_json::json!({"k": "value"}))
            .await
            .unwrap();
        archive.entry("third.txt").await.unwrap();
        archive.write(b"tail").await.unwrap();
        let outcome = archive.finish().await.unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(outcome.bytes, written.len() as i64);
        assert_eq!(outcome.hash, format!("{:x}", Sha256::digest(&written)));

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut entry = |name: &str| {
            let mut value = Vec::new();
            zip.by_name(name).unwrap().read_to_end(&mut value).unwrap();
            value
        };
        assert_eq!(entry("first.bin"), large);
        assert_eq!(entry("second.json"), r#"{"k":"value"}"#.as_bytes());
        assert_eq!(entry("third.txt"), b"tail");
    }

    /// When the async side aborts before reaching finish, the archive thread must not wrap up the half-finished
    /// archive as finished.
    #[tokio::test]
    async fn dropping_the_archive_without_finishing_reports_an_unfinished_build() {
        let root = temp_dir();
        let path = root.0.join("archive.zip");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut archive = Archive::create(file, path, root.0.clone(), root.0.clone());
        archive.entry("first.txt").await.unwrap();
        archive.write(b"partial").await.unwrap();
        let Archive { sender, worker, .. } = archive;
        drop(sender);
        let error = worker.await.unwrap().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("The export archive was not finished"),
            "{error}"
        );
    }
}
