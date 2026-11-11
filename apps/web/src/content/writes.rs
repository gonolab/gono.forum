//! Primary-database posting transactions; serializes the account quota first, then locks content and attachments.
use crate::db::DbRow;
use crate::db::Tx;
use std::fmt;

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    db::{self, Database},
    identity,
};

type Result<T> = std::result::Result<T, ContentWriteError>;

/// Title limit, equal to the column width of `gonof_forum_thread.subject` and of the revision table (`varchar(255)`).
/// A constant rather than a setting: lowering it has no operational need.
pub const MAX_TITLE_CHARS: usize = 255;
/// Hard limit on the body: when editing, the old body is stored whole in the revision table's `message text` (65535
/// bytes). The admin setting `settings.posting.body_max_bytes` can only be lowered within it.
pub const MAX_BODY_BYTES: usize = 60_000;

/// Deployment config `[content]`: only the attachment count, which sits with the other attachment limits in the
/// deployment configuration. Posting quotas, the body limit, the edit time window and growth points are all admin
/// settings, read by the write transaction's gate (docs/settings-and-caching.md, "Write gate and locks").
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WriteConfig {
    pub max_attachments: usize,
}

impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            max_attachments: crate::markdown::MAX_ATTACHMENTS,
        }
    }
}

impl WriteConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        // Body parsing caps attachment references at 20, so a larger config value is never used.
        ensure!(
            (1..=crate::markdown::MAX_ATTACHMENTS).contains(&self.max_attachments),
            "content.max_attachments must be between 1 and {}",
            crate::markdown::MAX_ATTACHMENTS
        );
        Ok(())
    }
}

/// The visitor scope the author chose for the thread, stored in `gonof_forum_thread.visibility`.
///
/// The effective scope is the stricter of the forum (including the parent forum) and the thread: read paths such as
/// lists, details and attachments compare both `forum_visibility` and `visibility`; this only records the author's
/// choice and does not copy the forum scope in.
/// The column also allows `private`, which is not offered to authors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadVisibility {
    #[default]
    Public,
    Members,
}

impl ThreadVisibility {
    /// Only lowercase `public` and `members` are recognized; anything else (including case variants and the empty
    /// string) is not a valid value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "members" => Some(Self::Members),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Members => "members",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct NewThread {
    pub activity: crate::activities::ActivitySpec,
    pub fid: i64,
    pub title: String,
    pub body: String,
    pub attachment_ids: Vec<i64>,
    pub request_key: String,
    pub visibility: ThreadVisibility,
}

#[derive(Clone, Debug, Serialize)]
pub struct NewReply {
    pub tid: i64,
    pub root_pid: Option<i64>,
    pub reply_to_pid: Option<i64>,
    pub body: String,
    pub attachment_ids: Vec<i64>,
    pub request_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
    pub pid: i64,
    pub tid: i64,
    pub position: i64,
    /// The same request key was written before, and this time the original result is only replayed inside the
    /// transaction. Callers use this to tell 201 from 200 and must not query the request table outside the
    /// transaction: with concurrent requests on the same key both sides would see "not written yet".
    pub replayed: bool,
}

#[derive(Debug)]
pub enum ContentWriteError {
    Validation(&'static str),
    Unauthorized,
    Forbidden(&'static str),
    NotFound,
    Conflict(&'static str),
    Quota(&'static str),
    /// A rejection (403) whose text is computed from the current settings, such as the new-member external link block
    /// that must state the observation period in days.
    Restricted(String),
    Internal(anyhow::Error),
}

impl fmt::Display for ContentWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::Forbidden(message)
            | Self::Conflict(message)
            | Self::Quota(message) => f.write_str(message),
            Self::Restricted(message) => f.write_str(message),
            Self::Unauthorized => f.write_str(crate::i18n::current().editor_error_unauthorized()),
            Self::NotFound => f.write_str(crate::i18n::current().editor_error_not_found()),
            Self::Internal(error) => write!(f, "content write failed: {error}"),
        }
    }
}

impl std::error::Error for ContentWriteError {}
impl ContentWriteError {
    pub(crate) fn retryable(&self) -> bool {
        match self {
            Self::Internal(error) => error.chain().any(|cause| {
                cause
                    .downcast_ref::<sqlx::Error>()
                    .is_some_and(crate::db::is_lock_conflict)
            }),
            _ => false,
        }
    }
}

impl From<anyhow::Error> for ContentWriteError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

impl From<sqlx::Error> for ContentWriteError {
    fn from(value: sqlx::Error) -> Self {
        Self::Internal(value.into())
    }
}

#[derive(Clone, Debug)]
pub struct ContentWriter {
    db: Database,
    config: WriteConfig,
    source: &'static str,
    /// Key ring for encrypting lottery redemption codes (`[identity.two_factor]`); without it, redemption code prizes
    /// cannot be published.
    codes: Option<identity::TwoFactorConfig>,
    /// The site URL of this request, used by the new-member external link block to recognize on-site links written as
    /// absolute URLs; when the admin area has a forum URL, the one read by the write gate wins. The CLI and tests
    /// without a request leave it unset, and only relative URLs count as on-site.
    site_url: Option<String>,
}

struct Author {
    regdate: i64,
    admin: bool,
    /// `adminid`: 1 administrator, 2 global moderator, 3 moderator; anti-spam exemptions are decided by it and the
    /// forum path.
    role: i64,
    username: String,
    /// The member row is already locked in this transaction; when posting a thread it is written straight into the
    /// thread row's author copy.
    copies: super::copies::AuthorCopies,
}

impl ContentWriter {
    pub fn new(db: Database, config: WriteConfig) -> Self {
        Self {
            db,
            config,
            source: "web",
            codes: None,
            site_url: None,
        }
    }

    /// Web pages take the site URL of the page frame; the member API takes the one computed during authentication.
    pub fn with_site_url(mut self, site_url: impl Into<String>) -> Self {
        self.site_url = Some(site_url.into());
        self
    }

    /// Lottery redemption codes and two-factor authentication share the `[identity.two_factor]` key ring.
    pub fn with_codes(mut self, ring: identity::TwoFactorConfig) -> Self {
        self.codes = Some(ring);
        self
    }

    /// The `source` column of the action log; `web` for web pages, `api` for the member API.
    pub fn with_source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }

    pub async fn publish_actor(
        &self,
        actor: identity::Actor,
        request: NewThread,
    ) -> Result<Published> {
        self.publish_retry(actor.uid, Some(actor), request, identity::now())
            .await
    }

    pub async fn reply_actor(
        &self,
        actor: identity::Actor,
        request: NewReply,
    ) -> Result<Published> {
        self.reply_retry(actor.uid, Some(actor), request, identity::now())
            .await
    }

    pub async fn publish(&self, uid: i64, request: NewThread) -> Result<Published> {
        self.publish_at(uid, request, identity::now()).await
    }

    pub async fn reply(&self, uid: i64, request: NewReply) -> Result<Published> {
        self.reply_at(uid, request, identity::now()).await
    }

    /// Entry point with explicit UTC Unix seconds, for quota boundaries and concurrency contracts.
    pub async fn publish_at(
        &self,
        uid: i64,
        request: NewThread,
        timestamp: i64,
    ) -> Result<Published> {
        self.publish_retry(uid, None, request, timestamp).await
    }

    async fn publish_retry(
        &self,
        uid: i64,
        credentials: Option<identity::Actor>,
        request: NewThread,
        timestamp: i64,
    ) -> Result<Published> {
        for attempt in 0..3 {
            let result = self
                .publish_inner(uid, credentials, request.clone(), timestamp)
                .await;
            match &result {
                Err(error) if attempt < 2 && error.retryable() => {
                    tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await
                }
                _ => return result,
            }
        }
        unreachable!("bounded retry always returns")
    }

    async fn publish_inner(
        &self,
        uid: i64,
        credentials: Option<identity::Actor>,
        mut request: NewThread,
        timestamp: i64,
    ) -> Result<Published> {
        self.config.validate()?;
        request.title = request.title.trim().to_owned();
        self.validate_body(&request.body, &request.request_key, timestamp)?;
        if request.fid <= 0
            || request.title.is_empty()
            || request.title.chars().count() > MAX_TITLE_CHARS
            || request.title.chars().any(char::is_control)
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_title(),
            ));
        }
        self.normalize_attachments(&mut request.attachment_ids)?;
        // Normalize before the digest: fields outside the kind do not enter the digest, so a retry with or without
        // them counts as the same publication.
        request.activity = request.activity.normalized();
        // The digest keeps the old format's empty tags array, so existing tag-less requests can still replay after
        // the upgrade.
        let hash = identity::token_hash(
            &serde_json::json!({
                "operation": "thread", "fid": request.fid, "title": request.title,
                "body": request.body, "attachments": request.attachment_ids, "activity": request.activity,
                "tags": [], "visibility": request.visibility,
            })
            .to_string(),
        );
        let mut tx = self.db.begin_content_write().await?;
        let settings = crate::runtime::guard_write(&self.db, &mut tx, false)
            .await
            .map_err(crate::organization::identity_error)?;
        let author = self.author_locked(&mut tx, uid, credentials).await?;
        if let Some(published) = self
            .replay(&mut tx, uid, &request.request_key, &hash)
            .await?
        {
            tx.commit().await?;
            return Ok(published);
        }
        body_within(&settings, &request.body)?;
        let parent = self.forum_locked(&mut tx, request.fid).await?;
        let timestamp = write_timestamp(credentials, timestamp);
        self.check_quota(&mut tx, &settings, uid, &author, true, timestamp)
            .await?;
        self.check_spam(
            &mut tx,
            &settings,
            uid,
            &author,
            (request.fid, parent),
            true,
            // The title and a news thread's source link are shown as well; `normalized` left the source link empty
            // for other kinds.
            super::antispam::Linked {
                body: &request.body,
                title: &request.title,
                source_url: &request.activity.source_url,
            },
            timestamp,
        )
        .await?;
        // Level threshold for starting red packets (2) and lotteries (3) (docs/roles.md, "Level gates"): people
        // who manage the target forum are not limited, the same rule as anti-spam; no SQL when the required level is 1.
        if matches!(request.activity.kind, 2 | 3)
            && settings.growth.activity_level > 1
            && !super::antispam::manages(&self.db, &mut tx, uid, author.role, request.fid, parent)
                .await?
        {
            let required = settings.growth.activity_level;
            if let Some(level) =
                crate::growth::level_gate(&self.db, &mut tx, &settings.growth, uid, required)
                    .await?
            {
                return Err(ContentWriteError::Restricted(
                    crate::i18n::current().editor_error_activity_level(level),
                ));
            }
        }
        let copies = super::copies::ThreadCopies {
            author: author.copies.clone(),
            forum: super::copies::ForumCopies::load(&self.db, &mut tx, request.fid).await?,
        };
        let tid = self
            .insert_thread(&mut tx, uid, &request, &copies, timestamp)
            .await?;
        let pid = self.allocate_pid(&mut tx).await?;
        let published = Published {
            pid,
            tid,
            position: 1,
            replayed: false,
        };
        self.insert_post(
            &mut tx,
            uid,
            request.fid,
            published,
            None,
            None,
            &request.body,
            timestamp,
        )
        .await?;
        // The new thread is listed (displayorder 0): the forum thread count and the member's own thread count each go
        // up by one.
        super::copies::count_thread(
            &self.db,
            &mut tx,
            (
                request.fid,
                request.visibility.as_str(),
                request.activity.kind,
            ),
            1,
        )
        .await?;
        super::copies::count_member(&self.db, &mut tx, Some(uid), (1, 0, 0)).await?;
        self.bind_attachments(&mut tx, uid, pid, &request.attachment_ids, timestamp)
            .await?;
        // The list thumbnail takes a static image already bound in the first post body.
        super::thread_images::rewrite(&self.db, &mut tx, tid, pid, &request.body).await?;
        self.finish(
            &mut tx,
            uid,
            &request.request_key,
            &hash,
            published,
            "content.thread.publish",
            timestamp,
        )
        .await?;
        // The site's name for its points in this request's language: activity messages and the shorthand lottery
        // prize name use it (docs/architecture.md, "Languages").
        let credit = settings.site.credit_word(crate::i18n::current());
        if credentials.is_some() {
            crate::activities::publish_live(
                &self.db,
                &mut tx,
                uid,
                tid,
                &request.activity,
                self.codes.as_ref(),
                &credit,
            )
            .await?;
            crate::activities::reward_content_live(
                &self.db,
                &mut tx,
                uid,
                pid,
                true,
                settings.time.offset_minutes,
            )
            .await?;
        } else {
            crate::activities::publish(
                &self.db,
                &mut tx,
                uid,
                tid,
                &request.activity,
                self.codes.as_ref(),
                &credit,
                timestamp,
            )
            .await?;
            crate::activities::reward_content(
                &self.db,
                &mut tx,
                uid,
                pid,
                true,
                timestamp,
                settings.time.offset_minutes,
            )
            .await?;
        }
        crate::growth::record_post(
            &self.db,
            &mut tx,
            &settings.growth,
            uid,
            pid,
            true,
            timestamp,
        )
        .await?;
        crate::notifications::enqueue_content(&self.db, &mut tx, pid, uid, timestamp).await?;
        tx.commit().await?;
        Ok(published)
    }

    pub async fn reply_at(&self, uid: i64, request: NewReply, timestamp: i64) -> Result<Published> {
        self.reply_retry(uid, None, request, timestamp).await
    }

    async fn reply_retry(
        &self,
        uid: i64,
        credentials: Option<identity::Actor>,
        request: NewReply,
        timestamp: i64,
    ) -> Result<Published> {
        for attempt in 0..3 {
            let result = self
                .reply_inner(uid, credentials, request.clone(), timestamp)
                .await;
            match &result {
                Err(error) if attempt < 2 && error.retryable() => {
                    tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await
                }
                _ => return result,
            }
        }
        unreachable!("bounded retry always returns")
    }

    async fn reply_inner(
        &self,
        uid: i64,
        credentials: Option<identity::Actor>,
        mut request: NewReply,
        timestamp: i64,
    ) -> Result<Published> {
        self.config.validate()?;
        self.validate_body(&request.body, &request.request_key, timestamp)?;
        if request.tid <= 0
            || request.root_pid.is_some() != request.reply_to_pid.is_some()
            || request.root_pid.is_some_and(|pid| pid <= 0)
            || request.reply_to_pid.is_some_and(|pid| pid <= 0)
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_reply_target(),
            ));
        }
        self.normalize_attachments(&mut request.attachment_ids)?;
        let hash = identity::token_hash(
            &serde_json::json!({
                "operation": "reply", "tid": request.tid, "root_pid": request.root_pid,
                "reply_to_pid": request.reply_to_pid, "body": request.body,
                "attachments": request.attachment_ids,
            })
            .to_string(),
        );
        let owner = crate::activities::discover_owner(&self.db, request.tid)
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        let mut tx = self.db.begin_content_write().await?;
        let settings = crate::runtime::guard_write(&self.db, &mut tx, false)
            .await
            .map_err(crate::organization::identity_error)?;
        crate::activities::lock_members(&self.db, &mut tx, &[uid, owner]).await?;
        let author = self.author_locked(&mut tx, uid, credentials).await?;
        if let Some(published) = self
            .replay(&mut tx, uid, &request.request_key, &hash)
            .await?
        {
            tx.commit().await?;
            return Ok(published);
        }
        body_within(&settings, &request.body)?;
        // Only discovers the fid, then locks in a fixed order and re-validates the thread; reading to discover grants
        // no write permission.
        let fid: i64 = crate::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid = ?")
            .bind(request.tid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        let parent = self.forum_locked(&mut tx, fid).await?;
        let sql = "SELECT fid, maxposition, root_replies, closed, displayorder, readperm, visibility, publication_state, deleted_at
             FROM gonof_forum_thread WHERE tid = ? FOR UPDATE";
        let thread = crate::db::query(sql)
            .bind(request.tid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        if thread.try_get::<i64, _>("fid")? != fid
            || thread.try_get::<i64, _>("displayorder")? < 0
            || thread.try_get::<i64, _>("readperm")? != 0
            || !matches!(
                db::text(&thread, "visibility")?.as_str(),
                "public" | "members"
            )
            || db::text(&thread, "publication_state")? != "published"
        {
            return Err(ContentWriteError::NotFound);
        }
        if thread.try_get::<i64, _>("closed")? != 0 {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_closed(),
            ));
        }
        let first_sql =
            "SELECT pid FROM gonof_forum_post WHERE first_tid = ? AND invisible = 0 FOR UPDATE";
        if crate::db::query(first_sql)
            .bind(request.tid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .is_none()
        {
            return Err(ContentWriteError::NotFound);
        }
        if let Some(root) = request.root_pid {
            self.reply_target_locked(
                &mut tx,
                request.tid,
                root,
                request.reply_to_pid.expect("validated target"),
            )
            .await?;
        }
        // The actual stored time is read after the content lock, so a reply that waited past the deadline cannot
        // qualify for the activity.
        let timestamp = write_timestamp(credentials, timestamp);
        self.check_quota(&mut tx, &settings, uid, &author, false, timestamp)
            .await?;
        self.check_spam(
            &mut tx,
            &settings,
            uid,
            &author,
            (fid, parent),
            false,
            super::antispam::Linked::body(&request.body),
            timestamp,
        )
        .await?;
        let position = thread
            .try_get::<i64, _>("maxposition")?
            .checked_add(1)
            .context("thread floor number out of range")?;
        // A reply only increments the thread version (the floor cache is invalidated with it) and leaves the site-wide
        // version alone: lists change keys through the new-post watermark.
        crate::db::query("UPDATE gonof_forum_thread SET maxposition = maxposition + 1, replies = replies + 1, root_replies = root_replies + ?, lastpost = CASE WHEN lastpost > ? THEN lastpost ELSE ? END, cache_revision = cache_revision + 1 WHERE tid = ?")
            .bind(i64::from(request.root_pid.is_none())).bind(timestamp).bind(timestamp).bind(request.tid)
            .execute(self.db.observe(&mut *tx)).await?;
        let pid = self.allocate_pid(&mut tx).await?;
        let published = Published {
            pid,
            tid: request.tid,
            position,
            replayed: false,
        };
        self.insert_post(
            &mut tx,
            uid,
            fid,
            published,
            request.root_pid,
            request.reply_to_pid,
            &request.body,
            timestamp,
        )
        .await?;
        // A new valid reply only does a constant number of copy updates and does not scan the whole thread's counts.
        let summary: String = request.body.chars().take(255).collect();
        crate::db::query("UPDATE gonof_forum_thread SET valid_replies=valid_replies+1,last_reply_pid=IF(lastpost<=?,?,last_reply_pid),last_reply_author=IF(lastpost<=?,?,last_reply_author),last_reply_summary=IF(lastpost<=?,?,last_reply_summary),cache_revision=cache_revision+1 WHERE tid=?")
            .bind(timestamp).bind(pid).bind(timestamp).bind(&author.username).bind(timestamp).bind(summary).bind(request.tid)
            .execute(self.db.observe(&mut *tx)).await?;
        if let Some(root) = request.root_pid {
            crate::db::query(
                "UPDATE gonof_forum_post SET sub_replies_count = sub_replies_count + 1 WHERE pid = ?",
            )
            .bind(root)
            .execute(self.db.observe(&mut *tx))
            .await?;
        }
        super::copies::count_member(&self.db, &mut tx, Some(uid), (0, 1, 0)).await?;
        self.bind_attachments(&mut tx, uid, pid, &request.attachment_ids, timestamp)
            .await?;
        self.finish(
            &mut tx,
            uid,
            &request.request_key,
            &hash,
            published,
            "content.reply.publish",
            timestamp,
        )
        .await?;
        if credentials.is_some() {
            crate::activities::on_reply_live(&self.db, &mut tx, uid, request.tid, pid).await?;
            crate::activities::reward_content_live(
                &self.db,
                &mut tx,
                uid,
                pid,
                false,
                settings.time.offset_minutes,
            )
            .await?;
        } else {
            crate::activities::on_reply(&self.db, &mut tx, uid, request.tid, pid, timestamp)
                .await?;
            crate::activities::reward_content(
                &self.db,
                &mut tx,
                uid,
                pid,
                false,
                timestamp,
                settings.time.offset_minutes,
            )
            .await?;
        }
        crate::growth::record_post(
            &self.db,
            &mut tx,
            &settings.growth,
            uid,
            pid,
            false,
            timestamp,
        )
        .await?;
        crate::notifications::enqueue_content(&self.db, &mut tx, pid, uid, timestamp).await?;
        tx.commit().await?;
        Ok(published)
    }

    fn validate_body(&self, body: &str, request_key: &str, timestamp: i64) -> Result<()> {
        // Only the hard limit is checked here; a limit lowered in the admin area is compared after the write gate has
        // read the settings ([`body_within`]).
        if body.trim().is_empty() || body.len() > MAX_BODY_BYTES || body.contains('\0') {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_body(),
            ));
        }
        if !crate::identity::valid_request_key(request_key) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_request_key(),
            ));
        }
        if timestamp < 0 {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_time(),
            ));
        }
        crate::markdown::references(body).map_err(|_| {
            ContentWriteError::Validation(crate::i18n::current().editor_error_references())
        })?;
        Ok(())
    }

    fn normalize_attachments(&self, ids: &mut Vec<i64>) -> Result<()> {
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > self.config.max_attachments || ids.iter().any(|id| *id <= 0) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_attachment_ids(),
            ));
        }
        Ok(())
    }

    async fn author_locked(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        credentials: Option<identity::Actor>,
    ) -> Result<Author> {
        let sql = "SELECT username, avatar, avatar_aid, regdate, emailstatus, mobile, adminid, `freeze`, mute_until, credentials_version, account_state FROM gonof_common_member WHERE uid = ? FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(uid)
            .fetch_optional(self.db.observe(&mut **tx))
            .await?
            .ok_or(ContentWriteError::Unauthorized)?;
        if credentials.is_some_and(|actor| {
            row.try_get::<i64, _>("credentials_version").ok() != Some(actor.credentials_version)
        }) {
            return Err(ContentWriteError::Unauthorized);
        }
        if db::text(&row, "account_state")? != "active" {
            return Err(ContentWriteError::Unauthorized);
        }
        if row.try_get::<i64, _>("freeze")? != 0 {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_frozen(),
            ));
        }
        let mute_until: i64 = row.try_get("mute_until")?;
        if crate::moderation::muted(mute_until, identity::now()) {
            return Err(ContentWriteError::Restricted(
                crate::moderation::muted_message(mute_until),
            ));
        }
        if let Some(actor) = credentials
            && !identity::api_key_locked(&self.db, tx, actor).await?
        {
            return Err(ContentWriteError::Unauthorized);
        }
        let role = row.try_get::<i64, _>("adminid")?;
        let admin = role == 1;
        let participation = crate::login::Participation::current();
        if !admin
            && !crate::login::permits(
                self.db.observe(&mut **tx),
                uid,
                row.try_get::<i64, _>("emailstatus")? == 1,
                row.try_get::<Option<String>, _>("mobile")?.is_some(),
            )
            .await?
        {
            return Err(ContentWriteError::Forbidden(
                participation.message(crate::i18n::current().editor_error_verify()),
            ));
        }
        let username: String = row.try_get("username")?;
        Ok(Author {
            admin,
            role,
            regdate: row.try_get("regdate")?,
            copies: super::copies::AuthorCopies {
                username: username.clone(),
            },
            username,
        })
    }

    async fn replay(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        key: &str,
        hash: &str,
    ) -> Result<Option<Published>> {
        // The account row already serializes all requests of this account. Do not FOR UPDATE a request that does not
        // exist, otherwise MySQL's gap locks would deadlock with the forum lock when different accounts insert new
        // requests.
        let sql = "SELECT r.request_hash, p.pid, p.tid, p.position FROM gonof_forum_post_request r
             JOIN gonof_forum_post p ON p.pid = r.pid WHERE r.uid = ? AND r.request_key = ?";
        let Some(row) = crate::db::query(sql)
            .bind(uid)
            .bind(key)
            .fetch_optional(self.db.observe(&mut **tx))
            .await?
        else {
            return Ok(None);
        };
        if db::text(&row, "request_hash")? != hash {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_request_reused(),
            ));
        }
        Ok(Some(Published {
            pid: row.try_get("pid")?,
            tid: row.try_get("tid")?,
            position: row.try_get("position")?,
            replayed: true,
        }))
    }

    /// Daily quotas count by the calendar day of the site time zone; the time zone
    /// and the quota both come from the settings read by this transaction's write gate.
    async fn check_quota(
        &self,
        tx: &mut Tx<'_>,
        settings: &crate::runtime::SiteSettings,
        uid: i64,
        author: &Author,
        first: bool,
        timestamp: i64,
    ) -> Result<()> {
        if author.admin {
            return Ok(());
        }
        let quota = &settings.posting;
        // The "new member" period is computed live from the registration time, so changing the setting applies
        // retroactively to existing members at once (docs/settings-and-caching.md, "When a change takes effect").
        let new_member =
            timestamp.saturating_sub(author.regdate) < quota.new_member_hours.saturating_mul(3600);
        let since = if new_member {
            author.regdate
        } else {
            crate::clock::day_start(timestamp, settings.time.offset_minutes)
        };
        let limit = match (new_member, first) {
            (true, true) => quota.first_day_threads,
            (true, false) => quota.first_day_replies,
            (false, true) => quota.daily_threads,
            (false, false) => quota.daily_replies,
        };
        let total: i64 = crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid = ? AND first = ? AND dateline >= ?").bind(uid).bind(i64::from(first)).bind(since)
            .fetch_one(self.db.observe(&mut **tx)).await?;
        if total >= limit {
            return Err(ContentWriteError::Quota(match (new_member, first) {
                (true, true) => crate::i18n::current().editor_error_new_member_threads(),
                (true, false) => crate::i18n::current().editor_error_new_member_replies(),
                (false, true) => crate::i18n::current().editor_error_daily_threads(),
                (false, false) => crate::i18n::current().editor_error_daily_replies(),
            }));
        }
        Ok(())
    }

    /// New anti-spam rules (`content::antispam`): interval, hourly limit, duplicate content and new-member external
    /// links. Checked after the quota and before writing, using the same settings read by this transaction's write
    /// gate as the quota.
    #[allow(clippy::too_many_arguments)]
    async fn check_spam(
        &self,
        tx: &mut Tx<'_>,
        settings: &crate::runtime::SiteSettings,
        uid: i64,
        author: &Author,
        (fid, parent): (i64, Option<i64>),
        first: bool,
        post: super::antispam::Linked<'_>,
        timestamp: i64,
    ) -> Result<()> {
        let poster = super::antispam::Poster {
            uid,
            regdate: author.regdate,
            role: author.role,
            fid,
            parent,
        };
        super::antispam::check_post(
            &self.db,
            tx,
            settings,
            &poster,
            first,
            post,
            self.site_url.as_deref(),
            timestamp,
        )
        .await
    }

    /// Locks the forum path and returns the parent forum id (the anti-spam moderator exemption is decided by the forum
    /// path).
    async fn forum_locked(&self, tx: &mut Tx<'_>, fid: i64) -> Result<Option<i64>> {
        // Posting does not write the forum row, so a shared lock is enough to block disabling the forum, changing its
        // visibility and moving it, and posts and interactions in the same forum do not queue behind each other.
        let row = crate::organization::share_forum_path(&self.db, tx, fid).await?;
        if row.try_get::<i64, _>("readable")? != 1 {
            return Err(ContentWriteError::NotFound);
        }
        Ok(row.try_get("parent_fid")?)
    }

    async fn reply_target_locked(
        &self,
        tx: &mut Tx<'_>,
        tid: i64,
        root: i64,
        target: i64,
    ) -> Result<()> {
        let sql = "SELECT pid, root_pid, first, invisible, erased_at FROM gonof_forum_post WHERE tid = ? AND pid = ? FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(tid)
            .bind(root)
            .fetch_optional(self.db.observe(&mut **tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        if !visible_reply(&row)? || row.try_get::<Option<i64>, _>("root_pid")?.is_some() {
            return Err(ContentWriteError::NotFound);
        }
        if root != target {
            let row = crate::db::query(sql)
                .bind(tid)
                .bind(target)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?
                .ok_or(ContentWriteError::NotFound)?;
            if !visible_reply(&row)? || row.try_get::<Option<i64>, _>("root_pid")? != Some(root) {
                return Err(ContentWriteError::NotFound);
            }
        }
        Ok(())
    }

    /// Denormalized copy columns are written all at once (`content::copies`): the three permission columns and the
    /// top-level forum id have no defaults, so missing one fails with 1364. A new thread does not increment the
    /// site-wide version; lists change keys through the new-post watermark.
    async fn insert_thread(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        request: &NewThread,
        copies: &super::copies::ThreadCopies,
        timestamp: i64,
    ) -> Result<i64> {
        let result = crate::db::query("INSERT INTO gonof_forum_thread (fid, authorid, subject, dateline, lastpost, category,visibility,author,family_fid) VALUES (?, ?, ?, ?, ?, ?,?,?,?)")
            .bind(request.fid).bind(uid).bind(&request.title).bind(timestamp).bind(timestamp).bind(request.activity.kind)
            .bind(request.visibility.as_str())
            .bind(&copies.author.username).bind(copies.forum.family_fid)
            .execute(self.db.observe(&mut **tx)).await?;
        Ok(i64::try_from(result.last_insert_id()).context("thread ID out of range")?)
    }

    async fn allocate_pid(&self, tx: &mut Tx<'_>) -> Result<i64> {
        // Inserts pid=NULL into forum_post_tableid to take the auto-increment value: both backends accept
        // this statement.
        let result = crate::db::query("INSERT INTO gonof_forum_post_tableid (pid) VALUES (NULL)")
            .execute(self.db.observe(&mut **tx))
            .await?;
        Ok(i64::try_from(result.last_insert_id()).context("post ID out of range")?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_post(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        fid: i64,
        post: Published,
        root: Option<i64>,
        target: Option<i64>,
        body: &str,
        timestamp: i64,
    ) -> Result<()> {
        let (root_position, sub_position): (Option<i64>, Option<i64>) = if post.position == 1 {
            (None, None)
        } else if let Some(root) = root {
            let count: i64 = crate::db::query_scalar(
                "SELECT sub_replies_count FROM gonof_forum_post WHERE pid=? FOR UPDATE",
            )
            .bind(root)
            .fetch_one(self.db.observe(&mut **tx))
            .await?;
            (
                None,
                Some(
                    count
                        .checked_add(1)
                        .context("nested reply number out of range")?,
                ),
            )
        } else {
            let count: i64 =
                crate::db::query_scalar("SELECT root_replies FROM gonof_forum_thread WHERE tid=?")
                    .bind(post.tid)
                    .fetch_one(self.db.observe(&mut **tx))
                    .await?;
            (Some(count), None)
        };
        let shape = PostShape {
            pid: post.pid,
            tid: post.tid,
            position: post.position,
            root_pid: root,
            reply_to_pid: target,
            root_position,
            sub_position,
            sub_replies_count: 0,
        };
        shape.check()?;
        crate::db::query("INSERT INTO gonof_forum_post (pid, fid, tid, authorid, first, first_tid, position, root_pid, reply_to_pid, message, dateline,root_position,sub_position) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,?,?)")
            .bind(post.pid).bind(fid).bind(post.tid).bind(uid).bind(i64::from(shape.first())).bind(shape.first_tid()).bind(post.position)
            .bind(root).bind(target).bind(body).bind(timestamp).bind(root_position).bind(sub_position)
            .execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }

    async fn bind_attachments(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        pid: i64,
        ids: &[i64],
        timestamp: i64,
    ) -> Result<()> {
        for aid in ids {
            let sql = "SELECT a.uid, d.lifecycle, d.scan_status, d.purge_after, d.trashed_at FROM gonof_forum_attachment a
                 JOIN gonof_forum_attachment_0 d ON d.aid = a.aid WHERE a.aid = ? FOR UPDATE";
            let row = crate::db::query(sql)
                .bind(aid)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?
                .ok_or(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_attachment_missing(),
                ))?;
            if row.try_get::<i64, _>("uid")? != uid
                || db::text(&row, "lifecycle")? != "ready"
                || db::text(&row, "scan_status")? != "clean"
                || row
                    .try_get::<Option<i64>, _>("purge_after")?
                    .is_some_and(|expiry| expiry <= timestamp)
            {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_attachment_unusable(),
                ));
            }
            // Attachments in the trash must be restored first; moving to the trash and this check are decided under
            // the same row lock, so a reference and the trash can never both hold.
            if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_attachment_trashed(),
                ));
            }
            crate::db::query(
                "INSERT INTO gonof_forum_attachment_reference (aid, pid, dateline) VALUES (?, ?, ?)",
            )
            .bind(aid)
            .bind(pid)
            .bind(timestamp)
            .execute(self.db.observe(&mut **tx))
            .await?;
            crate::db::query(
                "UPDATE gonof_forum_attachment_0 SET purge_after = NULL WHERE aid = ?",
            )
            .bind(aid)
            .execute(self.db.observe(&mut **tx))
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        key: &str,
        hash: &str,
        post: Published,
        operation: &str,
        timestamp: i64,
    ) -> Result<()> {
        crate::identity::rules::insert_post_request(
            &self.db, tx, uid, key, hash, post.pid, timestamp,
        )
        .await?;
        let data = serde_json::json!({"pid": post.pid, "tid": post.tid, "position": post.position});
        let [pid, fid, to_fid] = crate::manage::log_targets(operation, &data);
        crate::db::query("INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline, pid, fid, to_fid) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(uid).bind(uid).bind(operation).bind(data.to_string())
            .bind(self.source).bind(timestamp).bind(pid).bind(fid).bind(to_fid)
            .execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }
}

/// Actual stored time: member requests take the program clock after acquiring the content lock, so a request whose
/// wait for the lock crossed the deadline cannot count as before the deadline; requests without member credentials
/// (fixtures, tests) use the time the caller gives.
fn write_timestamp(credentials: Option<identity::Actor>, timestamp: i64) -> i64 {
    if credentials.is_some() {
        crate::identity::now()
    } else {
        timestamp
    }
}

/// Shape of a new post row (the in-row rules, checked here before the row is written; the schema has no validation
/// triggers). The first post is
/// on floor 1, with no reply target and no in-floor sequence number; a top-level reply has a floor number above 1 and a
/// top-level sequence number; a nested reply has both a parent floor and a target, an in-floor sequence number, and no
/// nested replies of its own. That the parent floor is a top-level reply of the same thread and that the target is on
/// the parent floor's level depends on other rows, so `reply_target_locked` checks it under the lock (fixtures are
/// versioned fixed data).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PostShape {
    pub pid: i64,
    pub tid: i64,
    pub position: i64,
    pub root_pid: Option<i64>,
    pub reply_to_pid: Option<i64>,
    pub root_position: Option<i64>,
    pub sub_position: Option<i64>,
    pub sub_replies_count: i64,
}

impl PostShape {
    /// Whether a post is the first post is decided only by the floor number: `first` and `first_tid` are both derived
    /// from it and can never contradict each other.
    pub(crate) fn first(&self) -> bool {
        self.position == 1
    }

    /// For the first post `first_tid` is the thread id; for replies it is NULL; `UNIQUE(first_tid)` ensures a thread
    /// has only one first post.
    pub(crate) fn first_tid(&self) -> Option<i64> {
        self.first().then_some(self.tid)
    }

    pub(crate) fn check(&self) -> anyhow::Result<()> {
        let positive = |value: Option<i64>| value.is_some_and(|value| value > 0);
        let valid = self.pid > 0
            && self.tid > 0
            && self.position >= 1
            && self.sub_replies_count >= 0
            && self.root_pid != Some(self.pid)
            && self.reply_to_pid != Some(self.pid)
            && match (self.first(), self.root_pid, self.reply_to_pid) {
                (true, None, None) => self.root_position.is_none() && self.sub_position.is_none(),
                (false, None, None) => positive(self.root_position) && self.sub_position.is_none(),
                (false, Some(root), Some(target)) => {
                    root > 0
                        && target > 0
                        && self.root_position.is_none()
                        && positive(self.sub_position)
                        && self.sub_replies_count == 0
                }
                _ => false,
            };
        anyhow::ensure!(valid, "invalid post shape: {self:?}");
        Ok(())
    }
}

/// Withdrawal only masks this floor's body, which can still be replied to; moderation hiding and permanent erasure
/// can no longer be replied to, matching the entry condition of `can_reply` on the thread page.
fn visible_reply(row: &DbRow) -> Result<bool> {
    Ok(row.try_get::<i64, _>("first")? == 0
        && row.try_get::<i64, _>("invisible")? == 0
        && row.try_get::<i64, _>("erased_at")? == 0)
}

/// The body does not exceed the admin setting's limit (`settings.posting.body_max_bytes`, lowered within
/// [`MAX_BODY_BYTES`]).
/// Posting, replying and editing all call it after the write gate has read the settings: once a setting is committed,
/// older requests no longer write under the old limit.
pub(crate) fn body_within(settings: &crate::runtime::SiteSettings, body: &str) -> Result<()> {
    if body.len() > usize::try_from(settings.posting.body_max_bytes).unwrap_or(0) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().editor_error_body(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PostShape;

    fn first() -> PostShape {
        PostShape {
            pid: 10,
            tid: 3,
            position: 1,
            root_pid: None,
            reply_to_pid: None,
            root_position: None,
            sub_position: None,
            sub_replies_count: 0,
        }
    }

    #[test]
    fn post_shapes_keep_the_rules_the_triggers_had() {
        let opening = first();
        let reply = PostShape {
            position: 2,
            root_position: Some(1),
            ..opening
        };
        let child = PostShape {
            position: 3,
            root_pid: Some(11),
            reply_to_pid: Some(11),
            root_position: None,
            sub_position: Some(1),
            ..opening
        };
        for ok in [opening, reply, child] {
            assert!(ok.check().is_ok(), "must be accepted: {ok:?}");
        }
        assert_eq!(opening.first_tid(), Some(3));
        assert_eq!(reply.first_tid(), None);
        assert_eq!(child.first_tid(), None);
        for broken in [
            PostShape { pid: 0, ..opening },
            PostShape {
                position: 0,
                ..opening
            },
            PostShape {
                root_position: Some(1),
                ..opening
            },
            PostShape {
                root_pid: Some(9),
                reply_to_pid: Some(9),
                ..opening
            },
            PostShape {
                root_position: None,
                ..reply
            },
            PostShape {
                root_position: Some(0),
                ..reply
            },
            PostShape {
                sub_position: Some(1),
                ..reply
            },
            PostShape {
                reply_to_pid: Some(11),
                ..reply
            },
            PostShape {
                reply_to_pid: None,
                ..child
            },
            PostShape {
                root_pid: None,
                ..child
            },
            PostShape {
                sub_position: None,
                ..child
            },
            PostShape {
                root_position: Some(1),
                ..child
            },
            PostShape {
                root_pid: Some(10),
                ..child
            },
            PostShape {
                reply_to_pid: Some(10),
                ..child
            },
            PostShape {
                sub_replies_count: 1,
                ..child
            },
            PostShape {
                sub_replies_count: -1,
                ..reply
            },
        ] {
            assert!(broken.check().is_err(), "must be rejected: {broken:?}");
        }
    }
}
