//! Author corrections and content moderation share the same primary database transaction, permission checks and
//! idempotency ledger.
use crate::db::Tx;
use std::collections::BTreeSet;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::writes::{ContentWriteError, Published, ThreadVisibility, WriteConfig};
use crate::{
    db::{self, Database},
    identity::{self, Actor},
    markdown,
};

type Result<T> = std::result::Result<T, ContentWriteError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditRequest {
    pub pid: i64,
    pub expected_post_version: i64,
    pub expected_thread_version: i64,
    pub title: Option<String>,
    /// Fields not provided keep the original content under the lock; the idempotency digest still keeps the
    /// "not provided" meaning.
    pub body: Option<String>,
    pub request_key: String,
    pub reason: String,
    /// The thread's visitor scope, which can only be submitted with the first post; `None` keeps the current value.
    pub visibility: Option<ThreadVisibility>,
}

/// The short code of a moderation result at the end of its notification's event key (`moderation.{actor}.{key}.{code}`,
/// at most 98 characters with a 64-character request key): the notification row keeps only the reason the moderator
/// wrote, and the list puts the action in front in its own language ([`moderation_label`],
/// `moderation::describe_notices`).
pub(crate) const MODERATION_EDITED: &str = "e";

impl ContentAction {
    pub(crate) fn notice_code(self) -> &'static str {
        match self {
            Self::Hide => "h",
            Self::Unhide => "uh",
            Self::Restore => "r",
            Self::Lock => "l",
            Self::Unlock => "ul",
            Self::Pin | Self::Pin2 | Self::Pin3 => "p",
            Self::Unpin => "up",
            Self::Feature => "f",
            Self::Unfeature => "uf",
            Self::Withdraw => "w",
        }
    }
}

/// What a moderation notification says happened, by the code at the end of its event key, in this request's language;
/// `None` for an unknown code.
pub(crate) fn moderation_label(code: &str) -> Option<&'static str> {
    let t = crate::i18n::current();
    Some(match code {
        MODERATION_EDITED => t.editor_moderation_edited(),
        "h" => t.editor_moderation_hidden(),
        "uh" => t.editor_moderation_unhidden(),
        "r" => t.editor_moderation_restored(),
        "l" => t.editor_moderation_locked(),
        "ul" => t.editor_moderation_unlocked(),
        "p" => t.editor_moderation_pinned(),
        "up" => t.editor_moderation_unpinned(),
        "f" => t.editor_moderation_featured(),
        "uf" => t.editor_moderation_unfeatured(),
        "w" => t.editor_moderation_withdrawn(),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAction {
    Withdraw,
    Restore,
    Hide,
    Unhide,
    Lock,
    Unlock,
    Pin,
    Pin2,
    Pin3,
    Unpin,
    Feature,
    Unfeature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeRequest {
    pub pid: i64,
    pub expected_post_version: i64,
    pub expected_thread_version: i64,
    pub request_key: String,
    pub reason: String,
    pub action: ContentAction,
}

#[derive(Clone, Debug)]
pub struct EditablePost {
    pub pid: i64,
    pub tid: i64,
    pub fid: i64,
    pub authorid: Option<i64>,
    pub position: i64,
    pub root_pid: Option<i64>,
    pub first: bool,
    pub title: String,
    pub body: String,
    pub version: i64,
    pub thread_version: i64,
    pub deleted_at: i64,
    pub erased_at: i64,
    pub invisible: i64,
    pub thread_deleted_at: i64,
    pub thread_invisible: i64,
    pub closed: bool,
    pub pinned: bool,
    pub featured: bool,
    /// The original `visibility` of the thread itself.
    pub visibility: String,
    pub dateline: i64,
    pub edited_at: i64,
    pub can_author_edit: bool,
    pub can_manage: bool,
}
/// The result of an edit: where the post is, and the post version this edit left behind (the version it wrote, or the
/// version it found when nothing changed). A replay returns the version recorded with the request, not the post's
/// current one, so a client never receives a version for a body it has not seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edited {
    pub published: Published,
    pub version: i64,
}

impl EditablePost {
    fn published(&self) -> Published {
        Published {
            pid: self.pid,
            tid: self.tid,
            position: self.position,
            replayed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContentEditor {
    db: Database,
    config: WriteConfig,
    orphan_seconds: i64,
    source: &'static str,
    /// This request's site URL; see [`super::writes::ContentWriter::with_site_url`].
    site_url: Option<String>,
    /// In-process global cache: after pinning, unpinning, hiding and unhiding commit, the pin index is cleared and
    /// broadcast.
    global: Option<crate::cache::GlobalCache>,
}
struct Account {
    uid: i64,
    role: i64,
    verified: bool,
    /// The new-member external link block computes the observation period from the registration time.
    regdate: i64,
    /// Mute end time (0 for none): a muted member cannot edit, but withdrawing and restoring their own content work as
    /// usual.
    mute_until: i64,
}
impl Account {
    fn reject_muted(&self) -> Result<()> {
        if crate::moderation::muted(self.mute_until, crate::identity::now()) {
            return Err(ContentWriteError::Restricted(
                crate::moderation::muted_message(self.mute_until),
            ));
        }
        Ok(())
    }
}
struct LockedPost {
    post: EditablePost,
    /// The parent of the containing forum (the moderator exemption of the anti-spam link rule is decided by forum
    /// path).
    parent_fid: Option<i64>,
    author_access: bool,
    /// This post, its parent floor or the first post is under moderation hiding (any non-zero `invisible`).
    hidden: bool,
}

impl ContentEditor {
    pub fn new(db: Database, config: WriteConfig, orphan_seconds: i64) -> Self {
        Self {
            db,
            config,
            orphan_seconds,
            source: "web",
            site_url: None,
            global: None,
        }
    }

    /// The web and the member API pass the application's global cache handle; without it (CLI, tests) the in-process
    /// pin index is simply not cleared, and new pins appear after at most one cache TTL.
    pub fn with_global(mut self, global: crate::cache::GlobalCache) -> Self {
        self.global = Some(global);
        self
    }

    /// The web uses the page frame's site URL; the member API uses the one computed during authentication.
    pub fn with_site_url(mut self, site_url: impl Into<String>) -> Self {
        self.site_url = Some(site_url.into());
        self
    }

    /// The `source` column of the operation log; `web` for the web, `api` for the member API.
    pub fn with_source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }

    pub async fn editable(&self, actor: Actor, pid: i64) -> Result<EditablePost> {
        self.editable_at(actor, pid, identity::now()).await
    }
    pub async fn editable_at(
        &self,
        actor: Actor,
        pid: i64,
        timestamp: i64,
    ) -> Result<EditablePost> {
        self.load_editable(actor, pid, timestamp, false).await
    }
    /// Opens the edit page: on top of [`Self::editable`] adds the same "own content hidden" check as saving, so a
    /// moderator who is the author does not get a form that is bound to fail on submit. The move-thread page still uses
    /// `editable` and is not affected.
    pub async fn edit_form(&self, actor: Actor, pid: i64) -> Result<EditablePost> {
        self.edit_form_at(actor, pid, identity::now()).await
    }
    pub async fn edit_form_at(
        &self,
        actor: Actor,
        pid: i64,
        timestamp: i64,
    ) -> Result<EditablePost> {
        self.load_editable(actor, pid, timestamp, true).await
    }
    async fn load_editable(
        &self,
        actor: Actor,
        pid: i64,
        timestamp: i64,
        edit_form: bool,
    ) -> Result<EditablePost> {
        self.config.validate()?;
        let owner = self.owner(pid).await?;
        let mut tx = self.db.begin_write().await?;
        // Whether the author can edit follows the admin edit time window: the form reads the same value the gate will
        // read at write time.
        let settings = crate::runtime::locked(&self.db, &mut tx)
            .await
            .map_err(crate::organization::identity_error)?;
        let account = self.account(&mut tx, actor, owner).await?;
        if edit_form {
            account.reject_muted()?;
        }
        let mut locked = self
            .locked_post(&mut tx, &account, pid, timestamp, edit_window(&settings))
            .await?;
        // The edit form is only for roles allowed to edit on others' behalf; trainee moderators and reviewers only use
        // the status actions under "Moderate" (the move-thread page is blocked separately in the router).
        if edit_form && !staff_can_edit(account.role) {
            locked.post.can_manage = false;
        }
        if !locked.post.can_manage && !locked.post.can_author_edit {
            return Err(ContentWriteError::NotFound);
        }
        if edit_form && locked.post.can_manage {
            self.own_hidden(&account, &locked)?;
        }
        tx.commit().await?;
        Ok(locked.post)
    }

    pub async fn edit(&self, actor: Actor, request: EditRequest) -> Result<Published> {
        self.edit_at(actor, request, identity::now()).await
    }
    pub async fn edit_at(
        &self,
        actor: Actor,
        request: EditRequest,
        timestamp: i64,
    ) -> Result<Published> {
        Ok(self
            .edit_versioned_at(actor, request, timestamp)
            .await?
            .published)
    }
    /// Like [`Self::edit`], also returning the post version the edit left behind, read inside the edit transaction.
    pub async fn edit_versioned(&self, actor: Actor, request: EditRequest) -> Result<Edited> {
        self.edit_versioned_at(actor, request, identity::now())
            .await
    }
    pub async fn edit_versioned_at(
        &self,
        actor: Actor,
        request: EditRequest,
        timestamp: i64,
    ) -> Result<Edited> {
        for attempt in 0..3 {
            let result = self.edit_inner(actor, request.clone(), timestamp).await;
            match &result {
                Err(error) if attempt < 2 && error.retryable() => {
                    tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await;
                }
                _ => return result,
            }
        }
        unreachable!("bounded transaction retry returns its final result")
    }

    /// Parses attachment and mention references in the body and rechecks the attachment count limit.
    fn references(&self, body: &str) -> Result<BTreeSet<i64>> {
        let attachments = markdown::references(body)
            .map_err(|_| {
                ContentWriteError::Validation(crate::i18n::current().editor_error_edit_references())
            })?
            .attachments;
        if attachments.len() > self.config.max_attachments {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_attachments(),
            ));
        }
        Ok(attachments)
    }

    async fn edit_inner(
        &self,
        actor: Actor,
        mut request: EditRequest,
        timestamp: i64,
    ) -> Result<Edited> {
        self.validate_request(
            request.pid,
            request.expected_post_version,
            request.expected_thread_version,
            &request.request_key,
            timestamp,
        )?;
        request.reason = request.reason.trim().to_owned();
        request.title = request.title.map(|v| v.trim().to_owned());
        if request.title.is_none() && request.body.is_none() && request.visibility.is_none() {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_empty(),
            ));
        }
        if request.body.as_ref().is_some_and(|body| {
            body.trim().is_empty()
                || body.len() > super::writes::MAX_BODY_BYTES
                || body.contains('\0')
        }) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_body(),
            ));
        }
        if request.title.as_ref().is_some_and(|v| {
            v.is_empty()
                || v.chars().count() > super::writes::MAX_TITLE_CHARS
                || v.chars().any(char::is_control)
        }) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_title(),
            ));
        }
        // When the request carries its own body it is parsed right here, without holding the post and member row locks
        // for it; only an edit that keeps the original body cannot do this early, so it waits until `post.body` is read
        // under the lock.
        let provided_attachments = request
            .body
            .as_deref()
            .map(|body| self.references(body))
            .transpose()?;
        // The hash only covers action, target, title, body, visibility and reason: the version is a concurrency check,
        // not content, so replaying the same request after the version has advanced must still return the original
        // result instead of being judged a same-key, different-content request.
        // Omitted fields go into the digest as None; current content that changes with other edits must not be counted
        // as part of the request.
        let hash = identity::token_hash(
            &serde_json::json!({
                "operation":"edit","pid":request.pid,"title":request.title,
                "body":request.body,"reason":request.reason,"visibility":request.visibility,
            })
            .to_string(),
        );
        let owner = self.owner(request.pid).await?;
        let mut tx = self.db.begin_write().await?;
        let settings = crate::runtime::guard_write(
            &self.db,
            &mut tx,
            !request.reason.is_empty() || owner != Some(actor.uid),
        )
        .await
        .map_err(crate::organization::identity_error)?;
        if let Some(body) = &request.body {
            super::writes::body_within(&settings, body)?;
        }
        let account = self.account(&mut tx, actor, owner).await?;
        account.reject_muted()?;
        let locked = self
            .locked_post(
                &mut tx,
                &account,
                request.pid,
                timestamp,
                edit_window(&settings),
            )
            .await?;
        if locked.post.authorid != owner {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_edit_author(),
            ));
        }
        let staff = self.authorize(&account, &locked, &request.reason, false)?;
        if staff && !staff_can_edit(account.role) {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_edit_role(),
            ));
        }
        if staff {
            self.own_hidden(&account, &locked)?;
        }
        if let Some((published, version)) = self
            .replay(&mut tx, actor.uid, &request.request_key, &hash)
            .await?
        {
            tx.commit().await?;
            return Ok(Edited { published, version });
        }
        self.check_window(&locked, staff)?;
        let post = &locked.post;
        self.versions(
            post,
            request.expected_post_version,
            request.expected_thread_version,
        )?;
        if !post.first && request.title.is_some() {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_reply_title(),
            ));
        }
        if !post.first && request.visibility.is_some() {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_reply_visibility(),
            ));
        }
        // The column also has a reserved `private` value; as with moderation fields, edits must not overwrite reserved
        // values.
        if request.visibility.is_some() && ThreadVisibility::parse(&post.visibility).is_none() {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_reserved_visibility(),
            ));
        }
        let visibility = match request.visibility {
            Some(value) => value.as_str(),
            None => post.visibility.as_str(),
        };
        let visibility_changed = visibility != post.visibility;
        let body = request.body.as_deref().unwrap_or(&post.body);
        // Only an opening post has a title of its own; the field is empty on both sides for replies.
        let (title, title_before) = if post.first {
            (
                request.title.as_deref().unwrap_or(&post.title),
                post.title.as_str(),
            )
        } else {
            ("", "")
        };
        // Authors in the observation period or below the external link level cannot add off-site links to the body or
        // title through edits (links already present do not count). A news thread's source link cannot be edited, so
        // it is on neither side. Moderators and administrators editing others' posts are exempt; so is an author who
        // moderates this forum (same as posting).
        if locked.post.authorid == Some(actor.uid)
            && (body != post.body || title != title_before)
            && super::antispam::link_gates_apply(&settings, account.regdate, timestamp)
            && !super::antispam::manages(
                &self.db,
                &mut tx,
                account.uid,
                account.role,
                post.fid,
                locked.parent_fid,
            )
            .await?
        {
            super::antispam::check_links(
                &self.db,
                &mut tx,
                &settings,
                (account.uid, account.regdate),
                timestamp,
                super::antispam::Linked {
                    body,
                    title,
                    ..Default::default()
                },
                Some(super::antispam::Linked {
                    body: &post.body,
                    title: title_before,
                    ..Default::default()
                }),
                self.site_url.as_deref(),
            )
            .await?;
        }
        let attachments = match provided_attachments {
            Some(attachments) => attachments,
            None => self.references(&post.body)?,
        };
        // Only a changed body or title writes a revision, bumps this floor's version and marks the post as edited;
        // changing only the visibility is a thread state change like locking or pinning, which bumps only the thread
        // version and produces no body revision.
        let content_changed =
            post.body != body || request.title.as_ref().is_some_and(|v| v != &post.title);
        let changed = content_changed || visibility_changed;
        // The post version after this edit: only a body or title change bumps it.
        let version = if content_changed {
            next(post.version)?
        } else {
            post.version
        };
        // Changing the first post (title, visibility and body all appear in lists, feeds and in-site cards elsewhere)
        // or the excerpt of the last reply is a structural change, so the site version is bumped at the end of the
        // transaction; changing only a floor in the middle bumps only the thread version.
        let mut structural = changed && post.first;
        if content_changed {
            self.attachments(&mut tx, actor.uid, post.pid, &attachments, timestamp)
                .await?;
            // When the first post's body changed, the list thumbnail is rewritten with it; first post
            // changes already advance the site version, so list caches switch keys accordingly.
            if post.first {
                super::thread_images::rewrite(&self.db, &mut tx, post.tid, post.pid, body).await?;
            }
            crate::db::query("INSERT INTO gonof_forum_post_revision (pid,version,editor_uid,subject,message,dateline) VALUES (?,?,?,?,?,?)")
                .bind(post.pid).bind(post.version).bind(actor.uid).bind(post.first.then_some(post.title.as_str())).bind(&post.body).bind(timestamp)
                .execute(self.db.observe(&mut *tx)).await?;
            crate::db::query(
                "UPDATE gonof_forum_post SET message=?,version=?,edited_at=? WHERE pid=?",
            )
            .bind(body)
            .bind(version)
            .bind(timestamp)
            .bind(post.pid)
            .execute(self.db.observe(&mut *tx))
            .await?;
            if !post.first {
                // The connection has CLIENT_FOUND_ROWS on, so affected rows equal matched rows: 1 means this floor is
                // indeed the last reply.
                // The excerpt is cut to 255 characters in code before writing, one statement
                // for both backends.
                let summary: String = body.chars().take(255).collect();
                let summary = crate::db::query("UPDATE gonof_forum_thread SET last_reply_summary=?,cache_revision=cache_revision+1 WHERE tid=? AND last_reply_pid=?")
                    .bind(summary).bind(post.tid).bind(post.pid)
                    .execute(self.db.observe(&mut *tx)).await?;
                if summary.rows_affected() == 1 {
                    structural = true;
                } else {
                    super::copies::touch_thread(&self.db, &mut tx, post.tid).await?;
                }
            }
        }
        // A first post change bumps the thread version and the site version, so anonymous list, sidebar and floor
        // caches are invalidated; after switching to members-only nothing lingers in the shared cache.
        if changed && post.first {
            let title = request.title.as_deref().unwrap_or(&post.title);
            // The thread row is already locked in `locked_post`. A listable thread whose visibility changed must move
            // from one count slot to another.
            let before = super::copies::ListedThread::load(&self.db, &mut tx, post.tid).await?;
            crate::db::query(
                "UPDATE gonof_forum_thread SET subject=?,visibility=?,version=?,cache_revision=cache_revision+1 WHERE tid=?",
            )
            .bind(title)
            .bind(visibility)
            .bind(next(post.thread_version)?)
            .bind(post.tid)
            .execute(self.db.observe(&mut *tx))
            .await?;
            if before.listed() && before.visibility != visibility {
                super::copies::count_thread(
                    &self.db,
                    &mut tx,
                    (before.fid, &before.visibility, before.category),
                    -1,
                )
                .await?;
                super::copies::count_thread(
                    &self.db,
                    &mut tx,
                    (before.fid, visibility, before.category),
                    1,
                )
                .await?;
            }
        }
        self.audit(
            &mut tx,
            actor.uid,
            post,
            "content.edit",
            &request.reason,
            timestamp,
            changed,
        )
        .await?;
        if changed && staff {
            self.notify_result(
                &mut tx,
                actor.uid,
                post,
                &request.request_key,
                MODERATION_EDITED,
                &request.reason,
                timestamp,
            )
            .await?;
        }
        self.record(
            &mut tx,
            actor.uid,
            &request.request_key,
            &hash,
            (post.pid, Some(version)),
            timestamp,
        )
        .await?;
        if structural {
            super::copies::bump_site(&self.db, &mut tx).await?;
        }
        tx.commit().await?;
        Ok(Edited {
            published: post.published(),
            version,
        })
    }

    pub async fn change(&self, actor: Actor, request: ChangeRequest) -> Result<Published> {
        self.change_at(actor, request, identity::now()).await
    }
    pub async fn change_at(
        &self,
        actor: Actor,
        request: ChangeRequest,
        timestamp: i64,
    ) -> Result<Published> {
        for attempt in 0..3 {
            let result = self.change_inner(actor, request.clone(), timestamp).await;
            match &result {
                Err(error) if attempt < 2 && error.retryable() => {
                    tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await;
                }
                _ => return result,
            }
        }
        unreachable!("bounded transaction retry returns its final result")
    }

    async fn change_inner(
        &self,
        actor: Actor,
        mut request: ChangeRequest,
        timestamp: i64,
    ) -> Result<Published> {
        self.validate_request(
            request.pid,
            request.expected_post_version,
            request.expected_thread_version,
            &request.request_key,
            timestamp,
        )?;
        request.reason = request.reason.trim().to_owned();
        let hash = identity::token_hash(
            &serde_json::json!({"operation":"change","request":request}).to_string(),
        );
        let owner = self.owner(request.pid).await?;
        // Hiding, withdrawing and restoring recompute the valid reply count and the last reply after taking the thread
        // lock; REPEATABLE READ would keep the snapshot from before waiting on the lock and miss replies committed
        // meanwhile.
        let mut tx = self.db.begin_content_write().await?;
        let management = request.action != ContentAction::Withdraw
            && (!request.reason.is_empty()
                || owner != Some(actor.uid)
                || request.action != ContentAction::Restore);
        let settings = crate::runtime::guard_write(&self.db, &mut tx, management)
            .await
            .map_err(crate::organization::identity_error)?;
        let account = self.account(&mut tx, actor, owner).await?;
        let result = self
            .change_locked(
                &mut tx,
                actor.uid,
                &account,
                owner,
                &request,
                &hash,
                (timestamp, edit_window(&settings)),
            )
            .await?;
        tx.commit().await?;
        // The pin index only holds published level-2 and level-3 pins whose first post is not hidden: these actions
        // move threads in or out of the index. Rows are rechecked against the current state when fetched, so this is
        // only to make new pins appear immediately in this process; a failed broadcast returns 503 (saved)
        // (docs/settings-and-caching.md, "Invalidation").
        if matches!(
            request.action,
            ContentAction::Pin
                | ContentAction::Pin2
                | ContentAction::Pin3
                | ContentAction::Unpin
                | ContentAction::Hide
                | ContentAction::Unhide
        ) && let Some(global) = &self.global
        {
            global
                .announce(&[crate::cache::Block::Pins])
                .await
                .map_err(ContentWriteError::Internal)?;
        }
        Ok(result)
    }

    /// Accepting a restore request: within the caller's content write transaction, removes this floor's moderation
    /// hiding and restores the withdrawal under the moderation identity.
    ///
    /// The caller has already passed the read-only gate (as a moderation action), locked the operator and the applicant
    /// (the original author) in UID order, and done the recusal check. Here the state is re-read under the lock and only
    /// the actions actually needed now are performed; when both are needed, unhiding comes before restoring the
    /// withdrawal. Each action takes the same path as the admin buttons (forum permissions, versions, counts, growth
    /// points, logs and author notification). Request keys are derived from `seed` and the action, so retries of the
    /// same request are idempotent. Returns whether the content changed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn restore_for_request(
        &self,
        tx: &mut Tx<'_>,
        settings: &crate::runtime::SiteSettings,
        actor: Actor,
        owner: i64,
        pid: i64,
        reason: &str,
        seed: &str,
        timestamp: i64,
    ) -> Result<bool> {
        let owner = Some(owner);
        let window = edit_window(settings);
        let account = self.account(tx, actor, owner).await?;
        let locked = self
            .locked_post(tx, &account, pid, timestamp, window)
            .await?;
        let mut actions = Vec::new();
        if locked.post.invisible == -1 {
            actions.push((ContentAction::Unhide, "unhide"));
        }
        if locked.post.deleted_at > 0 {
            if !staff_action_allowed(account.role, ContentAction::Restore) {
                return Err(ContentWriteError::Forbidden(
                    crate::i18n::current().editor_error_intern_restore(),
                ));
            }
            actions.push((ContentAction::Restore, "restore"));
        }
        if actions.is_empty() && !locked.post.can_manage {
            return Err(ContentWriteError::NotFound);
        }
        for (action, name) in &actions {
            // The previous action bumped the version; submit with the current version read under the lock.
            let current = self
                .locked_post(tx, &account, pid, timestamp, window)
                .await?;
            let request = ChangeRequest {
                pid,
                expected_post_version: current.post.version,
                expected_thread_version: current.post.thread_version,
                request_key: identity::token_hash(&format!("{seed}:{name}")),
                reason: reason.trim().to_owned(),
                action: *action,
            };
            let hash = identity::token_hash(
                &serde_json::json!({"operation":"change","request":request}).to_string(),
            );
            self.change_locked(
                tx,
                actor.uid,
                &account,
                owner,
                &request,
                &hash,
                (timestamp, window),
            )
            .await?;
        }
        Ok(!actions.is_empty())
    }

    /// The body of a status action: the caller has opened the content write transaction, passed the read-only gate and
    /// locked the member rows of the operator and the original author.
    /// Does not commit; a same-key replay returns the original result.
    #[allow(clippy::too_many_arguments)]
    async fn change_locked(
        &self,
        tx: &mut Tx<'_>,
        actor_uid: i64,
        account: &Account,
        owner: Option<i64>,
        request: &ChangeRequest,
        hash: &str,
        (timestamp, window): (i64, i64),
    ) -> Result<Published> {
        let locked = self
            .locked_post(tx, account, request.pid, timestamp, window)
            .await?;
        if locked.post.authorid != owner {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_edit_author(),
            ));
        }
        let staff_only = !matches!(
            request.action,
            ContentAction::Withdraw | ContentAction::Restore
        );
        let staff = if request.action == ContentAction::Withdraw {
            // Withdrawal is a time-limited action of the original author; the moderation identity cannot act for the
            // author or extend the time window.
            if locked.post.authorid != Some(account.uid) || !locked.author_access {
                return Err(ContentWriteError::NotFound);
            }
            if request.reason.chars().count() > 500 || request.reason.contains('\0') {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_reason_long(),
                ));
            }
            false
        } else {
            self.authorize(account, &locked, &request.reason, staff_only)?
        };
        if request.action == ContentAction::Unhide {
            self.own_hidden(account, &locked)?;
        }
        if staff && !staff_action_allowed(account.role, request.action) {
            return Err(ContentWriteError::Forbidden(if account.role == 5 {
                crate::i18n::current().editor_error_reviewer_actions()
            } else {
                crate::i18n::current().editor_error_intern_actions()
            }));
        }
        if let Some((published, _)) = self
            .replay(tx, actor_uid, &request.request_key, hash)
            .await?
        {
            return Ok(published);
        }
        self.check_window(&locked, staff)?;
        let post = &locked.post;
        self.versions(
            post,
            request.expected_post_version,
            request.expected_thread_version,
        )?;
        if matches!(request.action, ContentAction::Hide | ContentAction::Unhide)
            && ![0, -1].contains(&post.invisible)
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_reserved_post(),
            ));
        }
        let (column, value, mut current, operation) = match request.action {
            ContentAction::Withdraw => (
                "deleted_at",
                timestamp.max(1),
                post.deleted_at,
                "content.withdraw",
            ),
            ContentAction::Restore => ("deleted_at", 0, post.deleted_at, "content.restore"),
            ContentAction::Hide => ("invisible", -1, post.invisible, "content.hide"),
            ContentAction::Unhide => ("invisible", 0, post.invisible, "content.unhide"),
            ContentAction::Lock => ("closed", 1, i64::from(post.closed), "content.lock"),
            ContentAction::Unlock => ("closed", 0, i64::from(post.closed), "content.unlock"),
            ContentAction::Pin => ("displayorder", 1, i64::from(post.pinned), "content.pin"),
            ContentAction::Pin2 => ("displayorder", 2, 0, "content.pin"),
            ContentAction::Pin3 => ("displayorder", 3, 0, "content.pin"),
            ContentAction::Unpin => ("displayorder", 0, i64::from(post.pinned), "content.unpin"),
            ContentAction::Feature => ("digest", 1, i64::from(post.featured), "content.feature"),
            ContentAction::Unfeature => {
                ("digest", 0, i64::from(post.featured), "content.unfeature")
            }
        };
        let thread_field = matches!(column, "closed" | "displayorder" | "digest");
        if thread_field && !post.first {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_thread_action(),
            ));
        }
        if thread_field {
            let sql = &format!("SELECT {column} FROM gonof_forum_thread WHERE tid=? FOR UPDATE");
            let raw: i64 = crate::db::query_scalar(sql)
                .bind(post.tid)
                .fetch_one(self.db.observe(&mut **tx))
                .await?;
            current = raw;
            if !(if column == "displayorder" {
                (0..=3).contains(&raw)
            } else {
                [0, 1].contains(&raw)
            }) {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_reserved_thread(),
                ));
            }
        }
        let changed = if matches!(request.action, ContentAction::Withdraw) {
            current == 0
        } else {
            current != value
        };
        if changed {
            if thread_field {
                crate::db::query(&format!(
                    "UPDATE gonof_forum_thread SET {column}=?,version=?,cache_revision=cache_revision+1 WHERE tid=?"
                ))
                .bind(value)
                .bind(next(post.thread_version)?)
                .bind(post.tid)
                .execute(self.db.observe(&mut **tx))
                .await?;
                // Adding or removing a highlight: while the first post still counts toward the author, the highlight
                // count changes with it (member counts).
                if column == "digest"
                    && super::copies::counted_post(post.invisible, post.deleted_at)
                {
                    let delta = if value > 0 { 1 } else { -1 };
                    super::copies::count_member(&self.db, tx, post.authorid, (0, 0, delta)).await?;
                }
            } else {
                // The post as the live-reply copies see it, before the change.
                let live_before = super::ReplyState::read(&self.db, tx, post.pid).await?;
                crate::db::query(&format!(
                    "UPDATE gonof_forum_post SET {column}=?,version=? WHERE pid=?"
                ))
                .bind(value)
                .bind(next(post.version)?)
                .bind(post.pid)
                .execute(self.db.observe(&mut **tx))
                .await?;
                if post.first {
                    let sql = if column == "deleted_at" {
                        "UPDATE gonof_forum_thread SET version=?,deleted_at=?,cache_revision=cache_revision+1 WHERE tid=?"
                    } else {
                        "UPDATE gonof_forum_thread SET version=?,cache_revision=cache_revision+1 WHERE tid=?"
                    };
                    let mut query = crate::db::query(sql).bind(next(post.thread_version)?);
                    if column == "deleted_at" {
                        query = query.bind(value);
                    }
                    query
                        .bind(post.tid)
                        .execute(self.db.observe(&mut **tx))
                        .await?;
                    // Moderation hiding and unhiding: a thread whose first post is hidden gets a negative
                    // displayorder and is no longer listed; unhiding sets it back to 0.
                    if column == "invisible" {
                        super::copies::sync_first_hidden(&self.db, tx, post.tid).await?;
                    }
                }
                // A root floor's moderation hiding is copied to its nested replies (root_invisible), so checking a
                // nested reply's visibility does not look up the parent floor.
                let nested = if column == "invisible" && !post.first && post.root_pid.is_none() {
                    super::copies::sync_root_invisible(&self.db, tx, post.pid, value).await?
                } else {
                    0
                };
                // Member counts look only at the post itself: it counts only when neither hidden nor withdrawn.
                let (invisible, deleted_at) = if column == "invisible" {
                    (value, post.deleted_at)
                } else {
                    (post.invisible, value)
                };
                let before = super::copies::counted_post(post.invisible, post.deleted_at);
                let after = super::copies::counted_post(invisible, deleted_at);
                if before != after {
                    super::copies::post_counted_changed(
                        &self.db,
                        tx,
                        (post.authorid, post.first, i64::from(post.featured)),
                        if after { 1 } else { -1 },
                    )
                    .await?;
                }
                super::reply_visibility_changed(&self.db, tx, live_before, nested).await?;
                if post.first
                    && matches!(
                        request.action,
                        ContentAction::Hide | ContentAction::Withdraw
                    )
                {
                    crate::activities::terminate(&self.db, tx, post.tid, timestamp).await?;
                }
            }
            // Growth points look only at the post's own state and highlight; locking and pinning do not
            // matter.
            let cause = match request.action {
                ContentAction::Hide | ContentAction::Withdraw => Some(crate::growth::Cause::Revoke),
                ContentAction::Unhide | ContentAction::Restore => {
                    Some(crate::growth::Cause::Restore)
                }
                ContentAction::Feature => Some(crate::growth::Cause::Digest),
                ContentAction::Unfeature => Some(crate::growth::Cause::Undigest),
                _ => None,
            };
            if let Some(cause) = cause {
                crate::growth::sync_posts(&self.db, tx, &[post.pid], cause, timestamp).await?;
            }
        }
        self.audit(
            tx,
            actor_uid,
            post,
            operation,
            &request.reason,
            timestamp,
            changed,
        )
        .await?;
        if changed && staff {
            self.notify_result(
                tx,
                actor_uid,
                post,
                &request.request_key,
                request.action.notice_code(),
                &request.reason,
                timestamp,
            )
            .await?;
        }
        self.record(
            tx,
            actor_uid,
            &request.request_key,
            hash,
            (post.pid, None),
            timestamp,
        )
        .await?;
        // Withdrawing, hiding and restoring change list-visible state or the valid reply count and last reply; locking,
        // pinning and highlighting change list markers: all are structural changes, so anonymous caches are
        // invalidated immediately at commit.
        if changed {
            super::copies::bump_site(&self.db, tx).await?;
        }
        Ok(post.published())
    }

    /// Tells the author about a moderator's action. The row keeps only the moderator's reason (always given for staff
    /// actions); `code` goes to the end of the event key and the list shows "Content hidden: …" in its own language.
    #[allow(clippy::too_many_arguments)]
    async fn notify_result(
        &self,
        tx: &mut Tx<'_>,
        actor: i64,
        post: &EditablePost,
        key: &str,
        code: &str,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        if let Some(uid) = post.authorid.filter(|uid| *uid != actor) {
            crate::moderation::notify(
                &self.db,
                tx,
                crate::moderation::Notice {
                    uid,
                    authorid: actor,
                    kind: crate::notifications::record::Kind::Moderation,
                    target: crate::notifications::record::Target::Post(post.pid),
                    key: &format!("moderation.{actor}.{key}.{code}"),
                    reason,
                    now,
                },
            )
            .await?;
        }
        Ok(())
    }

    fn validate_request(
        &self,
        pid: i64,
        post_version: i64,
        thread_version: i64,
        key: &str,
        timestamp: i64,
    ) -> Result<()> {
        self.config.validate()?;
        if pid <= 0
            || post_version <= 0
            || thread_version <= 0
            || timestamp < 0
            || !crate::identity::valid_request_key(key)
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().editor_error_edit_request(),
            ));
        }
        Ok(())
    }
    fn authorize(
        &self,
        account: &Account,
        locked: &LockedPost,
        reason: &str,
        staff_only: bool,
    ) -> Result<bool> {
        let staff = staff_only || !reason.is_empty() || locked.post.authorid != Some(account.uid);
        if staff {
            if !locked.post.can_manage {
                return Err(ContentWriteError::NotFound);
            }
            if !(1..=500).contains(&reason.chars().count()) || reason.contains('\0') {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_reason_required(),
                ));
            }
        } else if !locked.author_access {
            return Err(ContentWriteError::NotFound);
        }
        Ok(staff)
    }
    /// Content under moderation hiding cannot be edited or unhidden by its author, not even by a moderator or global
    /// moderator using the moderation identity; administrators are the final reviewers and are exempt.
    fn own_hidden(&self, account: &Account, locked: &LockedPost) -> Result<()> {
        if account.role != 1 && locked.hidden && locked.post.authorid == Some(account.uid) {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_own_hidden(),
            ));
        }
        Ok(())
    }
    fn check_window(&self, locked: &LockedPost, staff: bool) -> Result<()> {
        if !staff && !locked.post.can_author_edit {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_window(),
            ));
        }
        Ok(())
    }
    fn versions(&self, post: &EditablePost, version: i64, thread_version: i64) -> Result<()> {
        if post.version != version || post.thread_version != thread_version {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_version(),
            ));
        }
        Ok(())
    }
    async fn owner(&self, pid: i64) -> Result<Option<i64>> {
        // Discovering the author happens outside the transaction; no old MySQL snapshot may be established before
        // waiting on the account locks.
        let row = crate::db::query("SELECT authorid FROM gonof_forum_post WHERE pid=?")
            .bind(pid)
            .fetch_optional(self.db.executor())
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        Ok(row.try_get("authorid")?)
    }
    async fn account(&self, tx: &mut Tx<'_>, actor: Actor, owner: Option<i64>) -> Result<Account> {
        // The audit uid foreign key points to the original author; both accounts are locked in UID order before the
        // forum, thread and attachments.
        let mut uids = BTreeSet::from([actor.uid]);
        uids.extend(owner);
        let sql = "SELECT adminid,emailstatus,mobile,`freeze`,mute_until,credentials_version,account_state,regdate FROM gonof_common_member WHERE uid=? FOR UPDATE";
        let mut account = None;
        for uid in uids {
            let row = crate::db::query(sql)
                .bind(uid)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?
                .ok_or(ContentWriteError::Unauthorized)?;
            if uid == actor.uid {
                if row.try_get::<i64, _>("credentials_version")? != actor.credentials_version
                    || row.try_get::<i64, _>("freeze")? != 0
                    || db::text(&row, "account_state")? != "active"
                {
                    return Err(ContentWriteError::Unauthorized);
                }
                account = Some(Account {
                    uid,
                    role: row.try_get("adminid")?,
                    // Meets the participation conditions in effect for this request; the field name keeps the
                    // pre-change "verified".
                    verified: crate::login::permits(
                        self.db.observe(&mut **tx),
                        uid,
                        row.try_get::<i64, _>("emailstatus")? == 1,
                        row.try_get::<Option<String>, _>("mobile")?.is_some(),
                    )
                    .await?,
                    regdate: row.try_get("regdate")?,
                    mute_until: row.try_get("mute_until")?,
                });
            }
        }
        // The member row is locked, serialized with token revocation; see `identity::api_key_locked`.
        if !crate::identity::api_key_locked(&self.db, tx, actor).await? {
            return Err(ContentWriteError::Unauthorized);
        }
        account.ok_or(ContentWriteError::Unauthorized)
    }
    async fn locked_post(
        &self,
        tx: &mut Tx<'_>,
        account: &Account,
        pid: i64,
        timestamp: i64,
        window: i64,
    ) -> Result<LockedPost> {
        let row = crate::db::query("SELECT tid,fid,root_pid FROM gonof_forum_post WHERE pid=?")
            .bind(pid)
            .fetch_optional(self.db.observe(&mut **tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
        let tid: i64 = row.try_get("tid")?;
        let fid: i64 = row.try_get("fid")?;
        let root_pid: Option<i64> = row.try_get("root_pid")?;
        let forum = crate::organization::lock_forum_path(&self.db, tx, fid).await?;
        let thread=crate::db::query("SELECT fid,subject,version,deleted_at,closed,displayorder,digest,readperm,visibility,publication_state FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
            .bind(tid).fetch_one(self.db.observe(&mut **tx)).await?;
        if thread.try_get::<i64, _>("fid")? != fid {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_thread_moved(),
            ));
        }
        // The first post is always locked first, then the parent floor and the target floor; transactions on the same
        // thread are serialized by the thread row.
        let sql = "SELECT invisible FROM gonof_forum_post WHERE first_tid=? FOR UPDATE";
        let thread_invisible: i64 = crate::db::query_scalar(sql)
            .bind(tid)
            .fetch_one(self.db.observe(&mut **tx))
            .await?;
        let mut root_invisible = 0;
        if let Some(root) = root_pid {
            let sql = "SELECT invisible FROM gonof_forum_post WHERE pid=? FOR UPDATE";
            root_invisible = crate::db::query_scalar::<i64>(sql)
                .bind(root)
                .fetch_one(self.db.observe(&mut **tx))
                .await?;
        }
        let sql = "SELECT tid,fid,authorid,position,first,message,version,deleted_at,erased_at,invisible,dateline,edited_at FROM gonof_forum_post WHERE pid=? FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(pid)
            .fetch_one(self.db.observe(&mut **tx))
            .await?;
        if row.try_get::<i64, _>("tid")? != tid || row.try_get::<i64, _>("fid")? != fid {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().editor_error_post_moved(),
            ));
        }
        if row.try_get::<i64, _>("erased_at")? > 0 {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().editor_error_erased(),
            ));
        }
        let can_manage = match account.role {
            1 | 2 => true,
            // Moderators and trainee moderators manage the forums they are appointed to and their child forums; the
            // parent forum ID comes from the locked forum path; what trainee moderators can
            // do is narrowed further in [`staff_action_allowed`].
            3 | 4 => {
                crate::organization::moderator_locked(
                    &self.db,
                    tx,
                    account.uid,
                    fid,
                    forum.try_get::<Option<i64>, _>("parent_fid")?,
                )
                .await?
            }
            // Reviewers only handle thread first posts and replies that have been reported. Reports are only ever
            // added, never deleted, so no lock is needed.
            5 => {
                let (kind, target) = if row.try_get::<i64, _>("first")? == 1 {
                    ("thread", tid)
                } else {
                    ("reply", pid)
                };
                crate::db::query_scalar::<i64>(
                    "SELECT id FROM gonof_common_report WHERE target_type=? AND target_id=? LIMIT 1",
                )
                .bind(kind)
                .bind(target)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?
                .is_some()
            }
            _ => false,
        };
        let authorid = row.try_get::<Option<i64>, _>("authorid")?;
        let invisible = row.try_get::<i64, _>("invisible")?;
        let dateline = row.try_get::<i64, _>("dateline")?;
        let author_access = authorid == Some(account.uid)
            && (account.verified || account.role == 1)
            && forum.try_get::<i64, _>("readable")? == 1
            && thread.try_get::<i64, _>("readperm")? == 0
            && thread.try_get::<i64, _>("displayorder")? >= 0
            && matches!(
                db::text(&thread, "visibility")?.as_str(),
                "public" | "members"
            )
            && db::text(&thread, "publication_state")? == "published"
            && thread_invisible == 0
            && root_invisible == 0
            && invisible == 0;
        let post = EditablePost {
            pid,
            tid,
            fid,
            authorid,
            root_pid,
            position: row.try_get("position")?,
            first: row.try_get::<i64, _>("first")? == 1,
            title: db::text(&thread, "subject")?,
            body: db::text(&row, "message")?,
            version: row.try_get("version")?,
            thread_version: thread.try_get("version")?,
            deleted_at: row.try_get("deleted_at")?,
            erased_at: row.try_get("erased_at")?,
            invisible,
            thread_deleted_at: thread.try_get("deleted_at")?,
            thread_invisible,
            closed: thread.try_get::<i64, _>("closed")? != 0,
            pinned: thread.try_get::<i64, _>("displayorder")? > 0,
            featured: thread.try_get::<i64, _>("digest")? > 0,
            visibility: db::text(&thread, "visibility")?,
            dateline,
            edited_at: row.try_get("edited_at")?,
            can_author_edit: author_access
                && timestamp >= dateline
                && timestamp - dateline < window,
            can_manage,
        };
        Ok(LockedPost {
            parent_fid: forum.try_get("parent_fid")?,
            post,
            author_access,
            hidden: thread_invisible != 0 || root_invisible != 0 || invisible != 0,
        })
    }
    /// The original result of a request already recorded under this key, with the post version an edit recorded.
    /// Records without a version (status actions, and edits recorded before migration 0007) give the post's current
    /// version, read under the post lock; status actions discard it.
    async fn replay(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        key: &str,
        hash: &str,
    ) -> Result<Option<(Published, i64)>> {
        // The account lock already serializes requests; keys that do not exist are not locked, avoiding MySQL gap
        // locks.
        let sql = "SELECT r.request_hash,r.version,p.pid,p.tid,p.position,p.version AS current_version FROM gonof_forum_post_request r JOIN gonof_forum_post p ON p.pid=r.pid WHERE r.uid=? AND r.request_key=?";
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
                crate::i18n::current().editor_error_edit_request_reused(),
            ));
        }
        let version = match row.try_get::<Option<i64>, _>("version")? {
            Some(version) => version,
            None => row.try_get("current_version")?,
        };
        Ok(Some((
            Published {
                pid: row.try_get("pid")?,
                tid: row.try_get("tid")?,
                position: row.try_get("position")?,
                replayed: true,
            },
            version,
        )))
    }
    /// Records the request; an edit passes the post version it left behind, a status action passes none.
    async fn record(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        key: &str,
        hash: &str,
        (pid, version): (i64, Option<i64>),
        timestamp: i64,
    ) -> Result<()> {
        match version {
            Some(version) => {
                crate::identity::rules::insert_edit_request(
                    &self.db, tx, uid, key, hash, pid, timestamp, version,
                )
                .await?
            }
            None => {
                crate::identity::rules::insert_post_request(
                    &self.db, tx, uid, key, hash, pid, timestamp,
                )
                .await?
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn audit(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        post: &EditablePost,
        operation: &str,
        reason: &str,
        timestamp: i64,
        changed: bool,
    ) -> Result<()> {
        let before = serde_json::json!({
            "version":post.version,"thread_version":post.thread_version,
            "deleted_at":post.deleted_at,"invisible":post.invisible,
            "thread_deleted_at":post.thread_deleted_at,"thread_invisible":post.thread_invisible,
            "closed":post.closed,"pinned":post.pinned,"featured":post.featured,"edited_at":post.edited_at,
            "visibility":post.visibility,
        });
        let sql = "SELECT p.version,p.deleted_at,p.invisible,p.edited_at,t.version AS thread_version,t.deleted_at AS thread_deleted_at,t.closed,t.displayorder,t.digest,t.visibility,firstpost.invisible AS thread_invisible FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE p.pid=? FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(post.pid)
            .fetch_one(self.db.observe(&mut **tx))
            .await?;
        let after = serde_json::json!({
            "version":row.try_get::<i64,_>("version")?,"thread_version":row.try_get::<i64,_>("thread_version")?,
            "deleted_at":row.try_get::<i64,_>("deleted_at")?,"invisible":row.try_get::<i64,_>("invisible")?,
            "thread_deleted_at":row.try_get::<i64,_>("thread_deleted_at")?,"thread_invisible":row.try_get::<i64,_>("thread_invisible")?,
            "closed":row.try_get::<i64,_>("closed")? != 0,"pinned":row.try_get::<i64,_>("displayorder")? > 0,
            "featured":row.try_get::<i64,_>("digest")? > 0,"edited_at":row.try_get::<i64,_>("edited_at")?,
            "visibility":db::text(&row, "visibility")?,
        });
        let data = serde_json::json!({"pid":post.pid,"tid":post.tid,"fid":post.fid,"position":post.position,"before":before,"after":after,"reason":reason,"result":if changed {"applied"}else{"unchanged"}});
        let [pid, fid, to_fid] = crate::manage::log_targets(operation, &data);
        crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline,pid,fid,to_fid) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(post.authorid).bind(uid).bind(operation).bind(data.to_string())
            .bind(self.source).bind(timestamp).bind(pid).bind(fid).bind(to_fid)
            .execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }
    async fn attachments(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        pid: i64,
        ids: &BTreeSet<i64>,
        timestamp: i64,
    ) -> Result<()> {
        // The target version has been checked, and this PID's references in the snapshot match the client version;
        // references are not locked before the attachment locks, avoiding cross-thread deadlocks.
        let sql = "SELECT aid FROM gonof_forum_attachment_reference WHERE pid=? ORDER BY aid";
        let old: BTreeSet<i64> = crate::db::query_scalar(sql)
            .bind(pid)
            .fetch_all(self.db.observe(&mut **tx))
            .await?
            .into_iter()
            .collect();
        let all: BTreeSet<i64> = old.union(ids).copied().collect();
        for aid in all {
            let sql = "SELECT a.uid,d.lifecycle,d.scan_status,d.purge_after,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE";
            let row = crate::db::query(sql)
                .bind(aid)
                .fetch_optional(self.db.observe(&mut **tx))
                .await?
                .ok_or(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_attachment_missing(),
                ))?;
            // A purged attachment (e.g. a non-image attachment, which is no longer served and whose references the
            // purge command removes) left in an old body is no longer bound and does not raise an error; the body is
            // saved as is. Referencing it in a new post still errors (`bind_attachments` in `writes.rs`).
            if ids.contains(&aid)
                && !old.contains(&aid)
                && matches!(db::text(&row, "lifecycle")?.as_str(), "purging" | "purged")
            {
                continue;
            }
            if ids.contains(&aid) {
                if db::text(&row, "lifecycle")? != "ready"
                    || db::text(&row, "scan_status")? != "clean"
                    || (!old.contains(&aid) && row.try_get::<i64, _>("uid")? != uid)
                    || row
                        .try_get::<Option<i64>, _>("purge_after")?
                        .is_some_and(|expiry| expiry <= timestamp)
                {
                    return Err(ContentWriteError::Validation(
                        crate::i18n::current().editor_error_edit_attachment_unusable(),
                    ));
                }
                // Attachments bound to this post cannot go to the trash, so only new references can hit this.
                if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
                    return Err(ContentWriteError::Validation(
                        crate::i18n::current().editor_error_attachment_trashed(),
                    ));
                }
                if !old.contains(&aid) {
                    crate::db::query("INSERT INTO gonof_forum_attachment_reference(aid,pid,dateline) VALUES(?,?,?)")
                        .bind(aid).bind(pid).bind(timestamp).execute(self.db.observe(&mut **tx)).await?;
                }
                crate::db::query(
                    "UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=?",
                )
                .bind(aid)
                .execute(self.db.observe(&mut **tx))
                .await?;
            } else {
                crate::db::query(
                    "DELETE FROM gonof_forum_attachment_reference WHERE aid=? AND pid=?",
                )
                .bind(aid)
                .bind(pid)
                .execute(self.db.observe(&mut **tx))
                .await?;
                let sql = "SELECT pid FROM gonof_forum_attachment_reference WHERE aid=? FOR UPDATE";
                let references = crate::db::query(sql)
                    .bind(aid)
                    .fetch_all(self.db.observe(&mut **tx))
                    .await?;
                // Once no post references it, check whether avatars, brand images, forum icons, domain logos or
                // favicons reference it; one single-table COUNT per table (rather than NOT EXISTS checks inside the
                // UPDATE), and only when none does does it enter
                // the orphan period. The attachment row is locked, so references cannot change in between.
                if references.is_empty() && !self.referenced_elsewhere(tx, aid).await? {
                    crate::db::query(
                        "UPDATE gonof_forum_attachment_0 SET purge_after=GREATEST(dateline,?) WHERE aid=?",
                    )
                    .bind(
                        timestamp
                            .checked_add(self.orphan_seconds)
                            .context("attachment purge time out of range")?,
                    )
                    .bind(aid)
                    .execute(self.db.observe(&mut **tx))
                    .await?;
                }
            }
        }
        Ok(())
    }
    /// References other than posts: avatars, brand images, forum icons, domain logos and favicons, with one
    /// single-table COUNT per table.
    async fn referenced_elsewhere(&self, tx: &mut Tx<'_>, aid: i64) -> Result<bool> {
        for (sql, binds) in [
            (
                "SELECT COUNT(*) FROM gonof_common_member WHERE avatar_aid=?",
                1,
            ),
            (
                "SELECT COUNT(*) FROM gonof_common_site_asset WHERE aid=?",
                1,
            ),
            ("SELECT COUNT(*) FROM gonof_forum_forum WHERE icon_aid=?", 1),
            (
                "SELECT COUNT(*) FROM gonof_common_site_domain WHERE logo_aid=? OR favicon_aid=?",
                2,
            ),
        ] {
            let mut count = crate::db::query_scalar::<i64>(sql);
            for _ in 0..binds {
                count = count.bind(aid);
            }
            if count.fetch_one(self.db.observe(&mut **tx)).await? > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
/// Which moderation-identity actions each role may take (docs/roles.md, "Staff roles"): trainee
/// moderators can only hide and unhide, lock and unlock (not restore withdrawals, pin or highlight, so accepting a
/// restore request that needs a withdrawal restored is refused); reviewers can only hide and unhide.
/// Administrators, global moderators and moderators are not limited here. For editing on others' behalf see
/// [`ContentEditor::edit`].
pub fn staff_action_allowed(role: i64, action: ContentAction) -> bool {
    match role {
        crate::identity::ROLE_INTERN => matches!(
            action,
            ContentAction::Hide
                | ContentAction::Unhide
                | ContentAction::Lock
                | ContentAction::Unlock
        ),
        crate::identity::ROLE_REVIEWER => {
            matches!(action, ContentAction::Hide | ContentAction::Unhide)
        }
        _ => true,
    }
}

/// Whether the role may edit others' content: neither trainee moderators nor reviewers may.
pub fn staff_can_edit(role: i64) -> bool {
    !matches!(
        role,
        crate::identity::ROLE_INTERN | crate::identity::ROLE_REVIEWER
    )
}

fn next(version: i64) -> Result<i64> {
    version.checked_add(1).ok_or(ContentWriteError::Conflict(
        crate::i18n::current().editor_error_version_range(),
    ))
}

/// The time window (seconds) for authors to edit, withdraw and restore on their own, taken from
/// `settings.posting.edit_window_hours`. Computed live from the posting time, so a change applies to existing posts
/// immediately (docs/settings-and-caching.md, "When a change takes effect").
pub fn edit_window(settings: &crate::runtime::SiteSettings) -> i64 {
    settings.posting.edit_window_hours.saturating_mul(3600)
}
