//! Private uploads, durable processing and S3 delivery. Public routes never expose object URLs.
//!
//! Only image uploads are allowed, and no virus scanner is called: images are decoded and re-encoded in the
//! inspection subprocess and are usable once they pass.
use crate::db::DbRow;
use crate::db::Tx;
mod avatar;
mod branding;
mod display;
mod forum;
mod privacy;
pub use branding::{BRAND_SLOTS, BrandAsset};
mod inspect;
mod library;
mod non_image;
pub use library::{LIBRARY_PAGE_SIZE, LibraryItem, LibraryPage, LibraryView, Usage, UsageBucket};
pub use non_image::{NonImageAttachment, NonImageReport};
mod storage;
pub use storage::AuthorizedImage;

/// What `scan_detail` stores: a stable code per inspection outcome, translated when the row is read
/// (docs/architecture.md, "Languages"), so a site that changes its language shows the outcomes of earlier checks in
/// the new language.
type CheckText = fn(crate::i18n::Lang) -> &'static str;

const CHECK_DETAILS: &[(&str, CheckText)] = &[
    ("passed", crate::i18n::Lang::editor_upload_ready),
    ("crashed", crate::i18n::Lang::media_check_crashed),
    ("no-result", crate::i18n::Lang::media_check_no_result),
    ("incomplete", crate::i18n::Lang::media_check_incomplete),
    ("timeout", crate::i18n::Lang::media_check_timeout),
    ("paused", crate::i18n::Lang::media_check_paused),
    ("retrying", crate::i18n::Lang::media_check_retrying),
    ("unsupported", crate::i18n::Lang::media_check_unsupported),
    ("source", crate::i18n::Lang::media_check_source),
    ("mismatch", crate::i18n::Lang::media_check_mismatch),
    ("failed", crate::i18n::Lang::media_check_failed),
];

/// The code to store for an inspection message in this request's language; a message that is not one of the known
/// outcomes is stored as it is.
pub(crate) fn check_code(message: &str) -> String {
    let t = crate::i18n::current();
    CHECK_DETAILS
        .iter()
        .find(|(_, text)| text(t) == message)
        .map_or_else(|| message.to_owned(), |(code, _)| (*code).to_owned())
}

/// The stored `scan_detail` in this request's language; text that is not a known code is shown as it is.
fn check_text(stored: &str) -> String {
    CHECK_DETAILS
        .iter()
        .find(|(code, _)| *code == stored)
        .map_or_else(
            || stored.to_owned(),
            |(_, text)| text(crate::i18n::current()).to_owned(),
        )
}

#[cfg(test)]
mod tests;
mod thumbnail;
mod worker;
pub use worker::worker_entry;

use crate::{
    db::{self, Database},
    identity,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type Result<T> = std::result::Result<T, MediaError>;
const SETTING: &str = "media.storage";
/// How many of the member's attachments one page holds when the daily quota pages through them by aid descending
/// (`daily_bytes`).
const DAILY_PAGE: i64 = 50;
/// Stop reading further back once an attachment's upload time is this many seconds before midnight of the day.
const DAILY_SLACK_SECONDS: i64 = 300;

/// The hint when uploading a non-image (only images are allowed).
pub fn unsupported_format() -> &'static str {
    crate::i18n::current().media_error_unsupported()
}

/// Cache headers of image responses (post images, avatars, forum icons, list thumbnails): the browser does not request
/// again for 5 minutes, then revalidates with the `ETag`, and the server rechecks current permissions and bindings and
/// answers 304. The cost is that after an avatar change, withdrawal, hide or account deletion, browsers that already
/// saw the image may keep showing the old one for up to 5 minutes.
pub const IMAGE_CACHE_CONTROL: &str = "private, max-age=300";

/// Whether `If-None-Match` contains this entity tag (possibly several comma-separated ones, weak tags, or `*`).
pub fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    if_none_match.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// The visitor already verified by the session layer for this request: credentials version, freeze and account
/// status all come from the member row read fresh in this request (`identity::Identity::session`), so reading an
/// attachment does not read it again. Write paths still recheck under a lock in the transaction.
#[derive(Clone, Copy, Debug)]
pub struct MediaViewer {
    pub actor: identity::Actor,
    pub adminid: i64,
    /// Meets the participation conditions in effect for this request (`identity::Member::can_contribute`).
    pub contributes: bool,
}

/// Every value of the attachment detail `lifecycle` (`gonof_forum_attachment_0`; this list is the source of truth).
/// `db::source_guards` checks that every literal written to this column in SQL is listed here; a new value also needs
/// changes to the cleanup candidates (`cleanup` in `media/worker.rs`), the my-attachments list (`LISTED` in
/// `media/library.rs`) and the check scripts.
pub const LIFECYCLES: [&str; 6] = [
    "pending",
    "processing",
    "ready",
    "rejected",
    "purging",
    "purged",
];

/// Every value of the attachment detail `scan_status`, same rules as [`LIFECYCLES`].
pub const SCAN_STATUSES: [&str; 4] = ["pending", "clean", "rejected", "error"];

/// Inspection results: type of 1–127 characters, a 64-character digest, a non-negative size, and width and height
/// either both empty or both positive. Inspection runs in a subprocess and returns JSON; this rechecks before writing
/// "passed inspection" and errors if invalid, so the write is rejected.
pub(crate) fn check_inspection(
    mime_type: &str,
    content_sha256: &str,
    filesize: i64,
    width: Option<i64>,
    height: Option<i64>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=127).contains(&mime_type.chars().count())
            && content_sha256.chars().count() == 64
            && filesize >= 0,
        "invalid type, digest or size in the attachment inspection result"
    );
    anyhow::ensure!(
        match (width, height) {
            (None, None) => true,
            (Some(width), Some(height)) => width > 0 && height > 0,
            _ => false,
        },
        "invalid width or height in the attachment inspection result: {width:?} × {height:?}"
    );
    Ok(())
}

/// The detail row of a newly uploaded attachment (value rules checked before the write): file name of
/// 1–255 characters, storage location of 1–1024 characters, storage configuration of 1–64 characters, and a cleanup
/// time no earlier than the upload time; type, digest, size and dimensions as in [`check_inspection`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_new_upload(
    filename: &str,
    filesize: i64,
    mime_type: &str,
    content_sha256: &str,
    attachment: &str,
    storage_profile_id: &str,
    dateline: i64,
    purge_after: i64,
) -> anyhow::Result<()> {
    check_inspection(mime_type, content_sha256, filesize, None, None)?;
    anyhow::ensure!(
        (1..=255).contains(&filename.chars().count())
            && (1..=1024).contains(&attachment.chars().count())
            && (1..=64).contains(&storage_profile_id.chars().count()),
        "invalid file name, storage location or storage profile of the attachment"
    );
    anyhow::ensure!(
        dateline >= 0 && purge_after >= dateline,
        "invalid upload or purge time of the attachment"
    );
    Ok(())
}
const COLUMNS: &str = "a.aid, a.uid, a.request_hash, d.filename, d.filesize, d.mime_type, d.content_sha256, d.attachment, d.storage_profile_id, d.width, d.height, d.description, d.alt_text, d.dateline, d.updated_at, d.checked_at, d.purge_after, d.lifecycle, d.scan_status, d.scan_detail, d.trashed_at";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    pub local_directory: PathBuf,
    pub image_max_bytes: u64,
    /// Maximum bytes of a GIF converted from an animation (GIF, animated WebP, APNG). Non-image files are not
    /// accepted, so this is its only use.
    pub file_max_bytes: u64,
    pub daily_max_bytes: u64,
    pub image_max_pixels: u64,
    pub animation_max_frames: u32,
    pub animation_max_pixels: u64,
    pub processing_timeout_seconds: u64,
    pub orphan_seconds: i64,
    /// How long (seconds) attachments a member deleted into the trash are kept; once due, the cleanup task purges
    /// them by `purge_after`.
    pub trash_seconds: i64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            local_directory: ".local/media".into(),
            image_max_bytes: 10 * 1024 * 1024,
            file_max_bytes: 25 * 1024 * 1024,
            daily_max_bytes: 200 * 1024 * 1024,
            image_max_pixels: 40_000_000,
            animation_max_frames: 100,
            animation_max_pixels: 200_000_000,
            processing_timeout_seconds: 30,
            orphan_seconds: 7 * 86400,
            trash_seconds: 30 * 86400,
        }
    }
}

impl MediaConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.local_directory.as_os_str().is_empty(),
            "the media directory must not be empty"
        );
        anyhow::ensure!(
            self.image_max_bytes > 0
                && self.file_max_bytes > 0
                && self.daily_max_bytes > 0
                && self.image_max_bytes <= i64::MAX as u64
                && self.file_max_bytes <= i64::MAX as u64
                && self.daily_max_bytes <= i64::MAX as u64,
            "the media byte limits must be positive integers"
        );
        anyhow::ensure!(
            self.image_max_pixels > 0
                && self.animation_max_frames > 0
                && self.animation_max_pixels > 0
                && self.processing_timeout_seconds > 0
                && self.orphan_seconds > 0
                && self.trash_seconds > 0,
            "the media processing and cleanup limits must be greater than zero"
        );
        Ok(())
    }
}

#[derive(Debug)]
pub enum MediaError {
    Validation(String),
    Forbidden,
    NotFound,
    Conflict(String),
    Unavailable(String),
    Internal(anyhow::Error),
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(s) | Self::Conflict(s) | Self::Unavailable(s) => f.write_str(s),
            Self::Forbidden => f.write_str(crate::i18n::current().media_error_forbidden()),
            Self::NotFound => f.write_str(crate::i18n::current().media_error_not_found()),
            Self::Internal(_) => f.write_str(crate::i18n::current().media_error_internal()),
        }
    }
}
impl std::error::Error for MediaError {}
impl From<anyhow::Error> for MediaError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}
impl From<sqlx::Error> for MediaError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}
impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.into())
    }
}

#[derive(Clone, Debug)]
pub struct Media {
    db: Database,
    config: MediaConfig,
    directory: PathBuf,
    brand_cache: Arc<crate::cache::ImageCache>,
    display_cache: Arc<crate::cache::ImageCache>,
    /// List thumbnails (registry `thumbnails`).
    thumb_cache: Arc<crate::cache::ImageCache>,
    /// The process-wide global cache: the moderator check for downloading hidden attachments reads the moderator
    /// roster from it. Without it (constructed directly in tests) the roster is read fresh each time.
    global: Option<crate::cache::GlobalCache>,
    /// The pace of media processing tasks (slows down when idle, woken up by new tasks).
    pacer: Arc<worker::Pacer>,
    /// S3 clients cached by storage configuration id (registry `storage-clients`). Every save gets a new
    /// configuration id; new keys for a bucket also replace the credentials of its earlier ids and remove the old
    /// credentials file, which makes every process build those clients again. Replacing a credentials file on disk by
    /// hand requires a process restart.
    clients: Arc<storage::ClientCache>,
}

pub struct NewUpload {
    pub request_key: String,
    pub filename: String,
    pub description: String,
    pub alt_text: String,
    pub path: PathBuf,
}

/// The result of writing an upload to the database: a newly created attachment, or the existing one when the same
/// request is replayed.
enum Registered {
    Created(i64),
    Existing(Box<Upload>),
}

#[derive(Clone, Debug, Serialize)]
pub struct Upload {
    pub aid: i64,
    pub uid: i64,
    pub filename: String,
    pub filesize: i64,
    pub mime_type: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub description: String,
    pub alt_text: String,
    pub lifecycle: String,
    pub scan_status: String,
    /// The inspection outcome in this request's language: the column stores a stable code ([`check_code`]).
    pub scan_detail: String,
    #[serde(skip)]
    pub bound_pid: Option<i64>,
    /// Upload time, UTC seconds.
    #[serde(skip)]
    pub dateline: i64,
    /// Due cleanup time: empty while referenced; the end of the unused period while unused; the end of the trash
    /// retention period while in the trash.
    #[serde(skip)]
    pub purge_after: Option<i64>,
    /// When it was deleted into the trash; empty when not in the trash.
    #[serde(skip)]
    pub trashed_at: Option<i64>,
}

impl Upload {
    pub fn is_ready(&self) -> bool {
        self.lifecycle == "ready" && self.scan_status == "clean"
    }

    /// Avatars, forum icons and brand images only show images that finished processing and passed the scan.
    fn is_ready_image(&self) -> bool {
        self.is_ready() && self.mime_type.starts_with("image/")
    }

    pub fn markdown(&self) -> String {
        let is_image = self.mime_type.starts_with("image/");
        let label = if is_image && !self.alt_text.trim().is_empty() {
            &self.alt_text
        } else {
            &self.filename
        };
        let mut escaped = String::new();
        for character in label.chars() {
            if character.is_control() {
                escaped.push(' ');
                continue;
            }
            if matches!(
                character,
                '\\' | '['
                    | ']'
                    | '('
                    | ')'
                    | '*'
                    | '_'
                    | '!'
                    | '#'
                    | '~'
                    | '<'
                    | '>'
                    | '&'
                    | '|'
                    | '`'
            ) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        format!(
            "{}[{}](attachment:{})",
            if is_image { "!" } else { "" },
            escaped,
            self.aid
        )
    }
}

pub use display::DisplayImage;

pub struct MediaDownload {
    pub filename: String,
    pub mime_type: String,
    pub filesize: i64,
    pub is_image: bool,
    pub body: aws_sdk_s3::primitives::ByteStream,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StorageSettings {
    pub revision: i64,
    pub configured: bool,
    pub profile_id: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageInput {
    pub revision: i64,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Profile {
    id: String,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    path_style: bool,
    secret_ref: String,
}
#[derive(Default, Serialize, Deserialize)]
struct StoredSettings {
    revision: i64,
    active: String,
    profiles: BTreeMap<String, Profile>,
}
#[derive(Serialize, Deserialize)]
struct Credentials {
    access_key: String,
    secret_key: String,
}
#[derive(Default, Debug)]
pub struct MediaReport {
    pub processed: usize,
    pub failed: usize,
    pub purged: usize,
}
#[derive(Serialize, Deserialize)]
struct ProcessEvent {
    source: String,
}

impl Media {
    pub fn new(db: Database, config: MediaConfig, root: PathBuf) -> Self {
        Self {
            directory: root.join(&config.local_directory),
            db,
            config,
            brand_cache: Arc::new(branding::image_cache()),
            display_cache: Arc::new(display::cache()),
            thumb_cache: Arc::new(thumbnail::cache()),
            global: None,
            clients: Arc::default(),
            pacer: Arc::default(),
        }
    }

    /// Attach the process-wide global cache (the one shared by the router and the background workers).
    pub fn with_global(mut self, global: crate::cache::GlobalCache) -> Self {
        self.global = Some(global);
        self
    }

    pub fn config(&self) -> &MediaConfig {
        &self.config
    }

    pub async fn staging_file(&self) -> Result<(PathBuf, tokio::fs::File)> {
        let directory = self.directory.join("staging");
        private_directory(&directory).await?;
        let path = directory.join(identity::random_token());
        let file = private_file(&path).await?;
        Ok((path, file))
    }

    #[cfg(test)]
    pub async fn register_upload(&self, uid: i64, input: NewUpload) -> Result<Upload> {
        self.register_upload_actor(self.test_actor(uid).await?, input)
            .await
    }

    pub async fn register_upload_actor(
        &self,
        actor: identity::Actor,
        input: NewUpload,
    ) -> Result<Upload> {
        let path = input.path.clone();
        let staging = tokio::fs::canonicalize(self.directory.join("staging")).await?;
        if input.path.parent().is_none()
            || tokio::fs::canonicalize(input.path.parent().unwrap()).await? != staging
        {
            return Err(MediaError::Validation(
                crate::i18n::current().media_error_staging_file().into(),
            ));
        }
        let result = self.register(actor, input, false).await;
        let _ = tokio::fs::remove_file(path).await;
        result
    }

    async fn register(
        &self,
        actor: identity::Actor,
        mut input: NewUpload,
        management: bool,
    ) -> Result<Upload> {
        let uid = actor.uid;
        if !identity::valid_request_key(&input.request_key) {
            return Err(MediaError::Validation(
                crate::i18n::current().media_error_request_key().into(),
            ));
        }
        input.filename = input.filename.trim().to_owned();
        input.description = input.description.trim().to_owned();
        input.alt_text = input.alt_text.trim().to_owned();
        if input.filename.is_empty()
            || input.filename.chars().count() > 255
            || input
                .filename
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
            || input.description.chars().count() > 2000
            || input.alt_text.chars().count() > 1000
        {
            return Err(MediaError::Validation(
                crate::i18n::current().media_error_filename().into(),
            ));
        }
        let extension = inspect::extension(&input.filename);
        if !inspect::allowed_extension(&extension) {
            return Err(MediaError::Validation(unsupported_format().into()));
        }
        let metadata = tokio::fs::symlink_metadata(&input.path).await?;
        let staging = tokio::fs::canonicalize(self.directory.join("staging")).await?;
        if !metadata.is_file()
            || input.path.parent().is_none()
            || tokio::fs::canonicalize(input.path.parent().unwrap()).await? != staging
        {
            return Err(MediaError::Validation(
                crate::i18n::current().media_error_staging_file().into(),
            ));
        }
        if metadata.len() == 0 || metadata.len() > self.config.image_max_bytes {
            return Err(MediaError::Validation(
                crate::i18n::current().media_error_size().into(),
            ));
        }
        let source_hash = hash_file(&input.path).await?;
        let request_hash = identity::token_hash(
            &serde_json::json!([
                source_hash,
                input.filename,
                input.description,
                input.alt_text
            ])
            .to_string(),
        );
        let timestamp = identity::now();
        // The file first lands in its final location under a name unrelated to the aid, then the
        // database is written (on SQLite the whole site has a single writer, so a write transaction does not write to
        // disk). The hard link and the staged file are the same data, and the transaction
        // starts only after both file and directory are fsynced, so the source file is on disk before the processing
        // event becomes visible. Source files no processing event references are recognized from the database and
        // deleted after expiry by the cleanup task (`sources_in_use`).
        let source_directory = self.directory.join("sources");
        private_directory(&source_directory).await?;
        let source = identity::random_token();
        let source_path = source_directory.join(&source);
        tokio::fs::hard_link(&input.path, &source_path).await?;
        let durable = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&source_path)
                .await?;
            file.flush().await?;
            file.sync_all().await?;
            sync_directory(&source_directory).await
        }
        .await;
        let registered = match durable {
            Ok(()) => {
                self.register_rows(
                    actor,
                    &input,
                    management,
                    &source,
                    &metadata,
                    &source_hash,
                    &request_hash,
                    timestamp,
                )
                .await
            }
            Err(e) => Err(e),
        };
        let (tx, registered) = match registered {
            Ok(registered) => registered,
            Err(e) => {
                let _ = tokio::fs::remove_file(&source_path).await;
                return Err(e);
            }
        };
        // When the commit's reply fails, the commit may still have succeeded: keep the source file and let the cleanup
        // task check it against the database.
        tx.commit().await?;
        match registered {
            Registered::Created(aid) => {
                self.pacer.wake();
                self.upload(uid, aid).await
            }
            Registered::Existing(upload) => {
                // Same request replayed: the existing attachment uses its own source file, and the one written early
                // this time is referenced by nobody.
                let _ = tokio::fs::remove_file(&source_path).await;
                Ok(*upload)
            }
        }
    }

    /// The database part of an upload: quota, duplicate check, attachment row and processing event; the source file
    /// was written to disk before the transaction started (`register`). The transaction is handed back to the caller
    /// to commit.
    #[allow(clippy::too_many_arguments)]
    async fn register_rows(
        &self,
        actor: identity::Actor,
        input: &NewUpload,
        management: bool,
        source: &str,
        metadata: &std::fs::Metadata,
        source_hash: &str,
        request_hash: &str,
        timestamp: i64,
    ) -> Result<(Tx<'static>, Registered)> {
        let uid = actor.uid;
        let mut tx = self.db.begin_write().await?;
        // The daily upload quota counts by calendar day in the site time zone, with the time zone read by the gate.
        let offset_minutes = crate::runtime::guard_write(&self.db, &mut tx, management)
            .await
            .map_err(|e| MediaError::Validation(e.to_string()))?
            .time
            .offset_minutes;
        if management {
            crate::runtime::admin_locked(&self.db, &mut tx, actor)
                .await
                .map_err(|_| MediaError::Forbidden)?;
        }
        let mute_until = self.member_actor_locked(&mut tx, actor, false).await?;
        // No uploads while muted (docs/roles.md, "Mutes"): the upload could not be posted anyway and would only use
        // quota and run inspection. Administrators replacing brand images in the admin area are not affected
        // (administrators cannot be muted).
        if !management && crate::moderation::muted(mute_until, timestamp) {
            return Err(MediaError::Validation(crate::moderation::muted_message(
                mute_until,
            )));
        }
        let existing = crate::db::query(&format!("SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND a.request_key=?"))
            .bind(uid).bind(&input.request_key).fetch_optional(self.db.observe(&mut *tx)).await?;
        if let Some(row) = existing {
            if db::text(&row, "request_hash")? != request_hash {
                return Err(MediaError::Conflict(
                    crate::i18n::current().media_error_request_reused().into(),
                ));
            }
            return Ok((tx, Registered::Existing(Box::new(decode_upload(&row)?))));
        }
        let bytes =
            Self::daily_bytes(&self.db, Some(&mut tx), uid, timestamp, offset_minutes).await?;
        if bytes.saturating_add(metadata.len()) > self.config.daily_max_bytes {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_daily().into(),
            ));
        }
        let stored = self.stored_in(&mut tx).await?;
        let profile = stored.profiles.get(&stored.active).ok_or_else(|| {
            MediaError::Unavailable(crate::i18n::current().media_error_no_storage().into())
        })?;
        crate::db::query(
            "INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(?,?,?)",
        )
        .bind(uid)
        .bind(&input.request_key)
        .bind(request_hash)
        .execute(self.db.observe(&mut *tx))
        .await?;
        let aid: i64 = crate::db::query_scalar(
            "SELECT aid FROM gonof_forum_attachment WHERE uid=? AND request_key=?",
        )
        .bind(uid)
        .bind(&input.request_key)
        .fetch_one(self.db.observe(&mut *tx))
        .await?;
        let object = format!(
            "{}objects/{aid}-{}",
            profile.prefix,
            identity::random_token()
        );
        let filesize = i64::try_from(metadata.len()).map_err(|e| MediaError::Internal(e.into()))?;
        let purge_after = timestamp.saturating_add(self.config.orphan_seconds);
        check_new_upload(
            &input.filename,
            filesize,
            "application/octet-stream",
            source_hash,
            &object,
            &profile.id,
            timestamp,
            purge_after,
        )
        .map_err(MediaError::Internal)?;
        let insertion=async {
            crate::db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,width,height,description,alt_text,dateline,updated_at,checked_at,purge_after,lifecycle,scan_status,scan_detail) VALUES(?,?,?,?,?,?,?,NULL,NULL,?,?,?, ?,NULL,?,'pending','pending','')")
                .bind(aid).bind(&input.filename).bind(filesize).bind("application/octet-stream").bind(source_hash).bind(&object).bind(&profile.id)
                .bind(&input.description).bind(&input.alt_text).bind(timestamp).bind(timestamp).bind(purge_after)
                .execute(self.db.observe(&mut *tx)).await?;
            self.enqueue(&mut tx,aid,"media.process",&format!("media.process:{aid}"),serde_json::json!({"source":source,"original_size":metadata.len()}),timestamp).await?;
            self.audit(&mut tx,uid,"media.upload",serde_json::json!({"aid":aid}),timestamp).await?;
            Ok::<_,MediaError>(())
        }.await;
        insertion?;
        Ok((tx, Registered::Created(aid)))
    }

    /// Upload bytes the member has used today (calendar day in the site time zone, `offset_minutes`).
    ///
    /// The daily quota counts each attachment at the larger of its original and processed size. The original size is
    /// stored in the processing event's payload; `filesize` is the original size at upload and is changed to the
    /// processed size in the same transaction when processing finishes, so attachments still processing or rejected
    /// count at their original size, and any growth is added automatically once processing finishes. Rejected uploads
    /// still use quota; deleting into the trash, restoring and purging neither refund nor charge extra.
    /// The processing event's key is always media.process:{aid}: first take the day's attachments, then fetch the
    /// events through the unique index with keys built in code (rather than joining on CONCAT); the Outbox
    /// aggregate_id has no index. Attachments without a processing event do not count.
    /// When `tx` is given, read in that transaction, otherwise through the pool; both statements are hooked into the
    /// debug records.
    async fn daily_bytes(
        db: &Database,
        mut tx: Option<&mut Tx<'_>>,
        uid: i64,
        now: i64,
        offset_minutes: i64,
    ) -> Result<u64> {
        let day = crate::clock::day_start(now, offset_minutes);
        // Page through the member's attachments by aid descending using only the `(uid, aid)` index, stopping once an
        // upload time is [`DAILY_SLACK_SECONDS`] before midnight of the day: a `d.dateline>=?` filter could only apply
        // after the join, so every upload would scan all of this member's attachments ever.
        // aid and upload time increase together; the upload time is taken before the transaction starts, so
        // concurrent uploads may be a few seconds out of order, which the slack covers.
        let sql = "SELECT a.aid,d.filesize,d.dateline FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND a.aid<? ORDER BY a.aid DESC LIMIT ?";
        let mut files = Vec::new();
        let mut before = i64::MAX;
        loop {
            let page = match tx.as_deref_mut() {
                Some(tx) => {
                    crate::db::query(sql)
                        .bind(uid)
                        .bind(before)
                        .bind(DAILY_PAGE)
                        .fetch_all(db.observe(&mut **tx))
                        .await?
                }
                None => {
                    crate::db::query(sql)
                        .bind(uid)
                        .bind(before)
                        .bind(DAILY_PAGE)
                        .fetch_all(db.executor())
                        .await?
                }
            };
            let full = i64::try_from(page.len()).unwrap_or(0) == DAILY_PAGE;
            let mut reached_older = false;
            for row in page {
                let dateline: i64 = row.try_get("dateline")?;
                before = row.try_get("aid")?;
                if dateline >= day {
                    files.push(row);
                } else if dateline < day.saturating_sub(DAILY_SLACK_SECONDS) {
                    reached_older = true;
                }
            }
            if !full || reached_older {
                break;
            }
        }
        if files.is_empty() {
            return Ok(0);
        }
        let mut stored = std::collections::HashMap::new();
        for row in &files {
            let key = format!("media.process:{}", row.try_get::<i64, _>("aid")?);
            stored.insert(
                key,
                u64::try_from(row.try_get::<i64, _>("filesize")?).unwrap_or(0),
            );
        }
        let sql = format!(
            "SELECT event_key,payload FROM gonof_common_outbox WHERE event_key IN ({})",
            vec!["?"; stored.len()].join(",")
        );
        let mut events = crate::db::query(&sql);
        for key in stored.keys() {
            events = events.bind(key.as_str());
        }
        let events = match tx {
            Some(tx) => events.fetch_all(db.observe(&mut **tx)).await?,
            None => events.fetch_all(db.executor()).await?,
        };
        let mut bytes = 0_u64;
        for row in events {
            let payload: serde_json::Value = serde_json::from_str(&db::text(&row, "payload")?)
                .context("invalid upload quota record")?;
            let original = payload["original_size"]
                .as_u64()
                .context("upload quota record lacks the original size")?;
            let size = stored
                .get(&db::text(&row, "event_key")?)
                .copied()
                .unwrap_or(0);
            bytes = bytes.saturating_add(original.max(size));
        }
        Ok(bytes)
    }

    /// The member's uploads not yet referenced by a post. The caller checks the member status: `upload_for_actor`
    /// reads the member row first, and `register` rechecked it under a lock in the transaction; this only fetches
    /// the attachment by primary key, and ownership and references are judged in code.
    async fn upload(&self, uid: i64, aid: i64) -> Result<Upload> {
        let row = crate::db::query(&format!(
            "SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=?"
        ))
        .bind(aid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        let upload = decode_upload(&row)?;
        if upload.uid != uid {
            return Err(MediaError::NotFound);
        }
        let references: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=?",
        )
        .bind(aid)
        .fetch_one(self.db.executor())
        .await?;
        if references != 0 {
            return Err(MediaError::NotFound);
        }
        Ok(upload)
    }

    pub async fn upload_for_actor(&self, actor: identity::Actor, aid: i64) -> Result<Upload> {
        if !self.contributing_actor(actor).await? {
            return Err(MediaError::NotFound);
        }
        self.upload(actor.uid, aid).await
    }

    /// Metadata for a reference in its authorized post context, including staff edits.
    pub async fn upload_for_post(
        &self,
        actor: identity::Actor,
        aid: i64,
        pid: i64,
    ) -> Result<Upload> {
        let Some(viewer) = self.verified_viewer(actor).await? else {
            return Err(MediaError::NotFound);
        };
        if !self.post_attachment_allowed(aid, pid, Some(viewer)).await? {
            return Err(MediaError::NotFound);
        }
        let row = crate::db::query(&format!(
            "SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=?"
        ))
        .bind(aid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        let mut upload = decode_upload(&row)?;
        upload.bound_pid = Some(pid);
        Ok(upload)
    }

    async fn active_actor(&self, actor: identity::Actor) -> Result<bool> {
        Ok(identity::valid_actor(&self.db, actor).await?)
    }

    /// Moderator check (downloading attachments in hidden or withdrawn posts): the role comes from the member row the
    /// session layer read fresh for this request ([`MediaViewer`], freeze and account status already checked), and
    /// appointments from the process moderator roster ([`identity::manages_forum`], docs/queries.md, "Process
    /// caches").
    /// Administrators and global moderators do not use the roster.
    async fn manages_forum(
        &self,
        viewer: MediaViewer,
        fid: i64,
        parent: Option<i64>,
    ) -> Result<bool> {
        let MediaViewer { actor, adminid, .. } = viewer;
        if !identity::forum_role(adminid) {
            return Ok(identity::manages_forum(
                adminid,
                true,
                &[],
                (actor.uid, fid, parent),
            ));
        }
        let roster = match &self.global {
            Some(global) => {
                let ttl = global
                    .settings(&self.db)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?
                    .settings
                    .cache_ttl();
                global.moderators(&self.db, ttl).await?
            }
            None => Arc::new(crate::organization::moderator_roster(&self.db).await?),
        };
        Ok(identity::manages_forum(
            adminid,
            true,
            &roster,
            (actor.uid, fid, parent),
        ))
    }

    async fn contributing_actor(&self, actor: identity::Actor) -> Result<bool> {
        let member = crate::db::query("SELECT credentials_version,`freeze`,adminid,emailstatus,mobile,account_state FROM gonof_common_member WHERE uid=?",)
        .bind(actor.uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(member) = member else {
            return Ok(false);
        };
        Ok(
            member.try_get::<i64, _>("credentials_version")? == actor.credentials_version
                && member.try_get::<i64, _>("freeze")? == 0
                && db::text(&member, "account_state")? == "active"
                && (member.try_get::<i64, _>("adminid")? == 1
                    || crate::login::permits(
                        self.db.executor(),
                        actor.uid,
                        member.try_get::<i64, _>("emailstatus")? == 1,
                        member.try_get::<Option<String>, _>("mobile")?.is_some(),
                    )
                    .await?),
        )
    }

    /// A batch of attachments in the editor (echo and preview on the edit page): the member's own ones not yet
    /// referenced by a post, and those already bound to `edit_pid` while editing that the member can see (with
    /// `bound_pid` filled in). Same rules as calling [`Self::upload_for_post`] one by one and falling back to
    /// [`Self::upload_for_actor`], but the whole batch checks the member once, fetches attachment rows and
    /// references once by `IN`, and judges post permissions for `edit_pid` once (instead of three to five queries per
    /// attachment). Returned in the order of `aids`; missing ones are skipped.
    pub async fn editor_uploads(
        &self,
        actor: identity::Actor,
        aids: &[i64],
        edit_pid: Option<i64>,
    ) -> Result<Vec<Upload>> {
        let mut wanted: Vec<i64> = Vec::new();
        for aid in aids {
            if *aid > 0 && !wanted.contains(aid) {
                wanted.push(*aid);
            }
        }
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let Some(viewer) = self.verified_viewer(actor).await? else {
            return Ok(Vec::new());
        };
        let marks = vec!["?"; wanted.len()].join(",");
        let sql = format!(
            "SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid IN ({marks})"
        );
        let mut query = crate::db::query(&sql);
        for aid in &wanted {
            query = query.bind(*aid);
        }
        let mut uploads = BTreeMap::new();
        for row in query.fetch_all(self.db.executor()).await? {
            let upload = decode_upload(&row)?;
            uploads.insert(upload.aid, upload);
        }
        let sql =
            format!("SELECT aid,pid FROM gonof_forum_attachment_reference WHERE aid IN ({marks})");
        let mut query = crate::db::query(&sql);
        for aid in &wanted {
            query = query.bind(*aid);
        }
        let mut references: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for row in query.fetch_all(self.db.executor()).await? {
            references
                .entry(row.try_get("aid")?)
                .or_default()
                .push(row.try_get("pid")?);
        }
        let bound_here = |aid: i64| {
            edit_pid.is_some_and(|pid| references.get(&aid).is_some_and(|pids| pids.contains(&pid)))
        };
        let post_readable = match edit_pid {
            Some(pid) if wanted.iter().any(|aid| bound_here(*aid)) => {
                self.post_allowed(pid, Some(viewer)).await?
            }
            _ => false,
        };
        let mut found = Vec::new();
        for aid in wanted {
            let Some(mut upload) = uploads.remove(&aid) else {
                continue;
            };
            if post_readable && bound_here(aid) {
                upload.bound_pid = edit_pid;
                found.push(upload);
            } else if upload.uid == viewer.actor.uid
                && viewer.contributes
                && !references.contains_key(&aid)
            {
                found.push(upload);
            }
        }
        Ok(found)
    }

    pub async fn uploads(&self, uid: i64) -> Result<Vec<Upload>> {
        let rows=crate::db::query(&format!("SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.uid=? AND d.lifecycle NOT IN ('purged','purging') AND d.trashed_at IS NULL ORDER BY a.aid DESC LIMIT 100"))
            .bind(uid).fetch_all(self.db.executor()).await?;
        rows.iter().map(decode_upload).collect()
    }

    #[cfg(test)]
    async fn test_actor(&self, uid: i64) -> Result<identity::Actor> {
        let credentials_version = crate::db::query_scalar(
            "SELECT credentials_version FROM gonof_common_member WHERE uid=?",
        )
        .bind(uid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::Forbidden)?;
        Ok(identity::Actor {
            uid,
            credentials_version,
            api_key: None,
        })
    }

    /// Recheck the member under a lock and return the mute end time (`mute_until`, 0 when not muted), for the
    /// callers that need it to judge.
    async fn member_actor_locked(
        &self,
        tx: &mut Tx<'_>,
        actor: identity::Actor,
        admin: bool,
    ) -> Result<i64> {
        let (version, mute_until) = self.member_locked(tx, actor.uid, admin).await?;
        if version != actor.credentials_version {
            return Err(MediaError::Forbidden);
        }
        Ok(mute_until)
    }

    /// Returns the credentials version and the mute end time.
    async fn member_locked(&self, tx: &mut Tx<'_>, uid: i64, admin: bool) -> Result<(i64, i64)> {
        let row = crate::db::query("SELECT emailstatus,mobile,adminid,`freeze`,credentials_version,account_state,mute_until FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(uid)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?
        .ok_or(MediaError::Forbidden)?;
        let verified = crate::login::permits(
            self.db.observe(&mut **tx),
            uid,
            row.try_get::<i64, _>("emailstatus")? == 1,
            row.try_get::<Option<String>, _>("mobile")?.is_some(),
        )
        .await?;
        Ok((
            member_allowed(&row, verified, admin)?,
            row.try_get("mute_until")?,
        ))
    }

    /// Same as [`Self::member_locked`], but read through the pool without locks or a write transaction: only for
    /// read-only pages and prechecks before a transaction starts; writes still rely on the recheck inside the
    /// transaction (on SQLite a write transaction takes the site-wide write lock as soon as it opens).
    async fn member_unlocked(&self, actor: identity::Actor, admin: bool) -> Result<()> {
        let row = crate::db::query("SELECT emailstatus,mobile,adminid,`freeze`,credentials_version,account_state FROM gonof_common_member WHERE uid=?")
        .bind(actor.uid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::Forbidden)?;
        let verified = crate::login::permits(
            self.db.executor(),
            actor.uid,
            row.try_get::<i64, _>("emailstatus")? == 1,
            row.try_get::<Option<String>, _>("mobile")?.is_some(),
        )
        .await?;
        if member_allowed(&row, verified, admin)? != actor.credentials_version {
            return Err(MediaError::Forbidden);
        }
        Ok(())
    }

    async fn enqueue(
        &self,
        tx: &mut Tx<'_>,
        aid: i64,
        kind: &str,
        key: &str,
        payload: serde_json::Value,
        now: i64,
    ) -> Result<()> {
        let present: Option<i64> =
            crate::db::query_scalar("SELECT eventid FROM gonof_common_outbox WHERE event_key=?")
                .bind(key)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?;
        if present.is_none() {
            crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'attachment',?,?,?,?)")
                .bind(key).bind(kind).bind(aid).bind(payload.to_string()).bind(now).bind(now).execute(self.db.observe(&mut **tx)).await?;
        }
        Ok(())
    }

    async fn audit(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        kind: &str,
        data: serde_json::Value,
        now: i64,
    ) -> Result<()> {
        crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
            .bind(uid).bind(uid).bind(kind).bind(data.to_string()).bind(now).execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }
}

/// The forum and its parent forum are enabled and visible to the current identity, same rules as
/// `organization::forum_readable_sql`. The row must have `forum_status`, `forum_visibility`, `parent_fid`,
/// `ancestor_status` and `ancestor_visibility`.
fn forum_path_readable(row: &DbRow, member: bool) -> Result<bool> {
    let visible = |scope: &str| scope == "public" || (scope == "members" && member);
    let parent_readable = row.try_get::<Option<i64>, _>("parent_fid")?.is_none()
        || (row.try_get::<Option<i64>, _>("ancestor_status")? == Some(1)
            && row
                .try_get::<Option<String>, _>("ancestor_visibility")?
                .is_some_and(|scope| visible(&scope)));
    Ok(row.try_get::<i64, _>("forum_status")? == 1
        && visible(&db::text(row, "forum_visibility")?)
        && parent_readable)
}

fn decode_upload(row: &DbRow) -> Result<Upload> {
    Ok(Upload {
        aid: row.try_get("aid")?,
        uid: row.try_get("uid")?,
        filename: db::text(row, "filename")?,
        filesize: row.try_get("filesize")?,
        mime_type: db::text(row, "mime_type")?,
        width: row.try_get("width")?,
        height: row.try_get("height")?,
        description: db::text(row, "description")?,
        alt_text: db::text(row, "alt_text")?,
        lifecycle: db::text(row, "lifecycle")?,
        scan_status: db::text(row, "scan_status")?,
        scan_detail: check_text(&db::text(row, "scan_detail")?),
        bound_pid: None,
        dateline: row.try_get("dateline")?,
        purge_after: row.try_get("purge_after")?,
        trashed_at: row.try_get("trashed_at")?,
    })
}

async fn private_directory(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}
/// Whether the member may upload or manage storage: not frozen and the account is normal; managing storage requires
/// an administrator, uploading requires an administrator or meeting the participation conditions. Returns the
/// credentials version.
fn member_allowed(row: &DbRow, verified: bool, admin: bool) -> Result<i64> {
    let role: i64 = row.try_get("adminid")?;
    let frozen: i64 = row.try_get("freeze")?;
    if frozen != 0
        || db::text(row, "account_state")? != "active"
        || if admin {
            role != 1
        } else {
            role != 1 && !verified
        }
    {
        return Err(MediaError::Forbidden);
    }
    Ok(row.try_get("credentials_version")?)
}

async fn private_file(path: &Path) -> Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    Ok(options.open(path).await?)
}
async fn sync_directory(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all())
        .await
        .context("cannot persist the private directory")??;
    Ok(())
}

async fn hash_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).await?;
        if count == 0 {
            break;
        }
        hasher.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
