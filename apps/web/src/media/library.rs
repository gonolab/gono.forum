//! My attachments and the attachment trash.
//!
//! The trash adds no `lifecycle` value: attachments in the trash are still `ready/clean`, `trashed_at` records when it
//! was moved to the trash, and the expiry time is written in the usual `purge_after`, so the regular cleanup task
//! purges it on expiry through the `(lifecycle, purge_after)` index. Deleting, restoring, binding and cleanup all lock
//! the attachment row by primary key first and then re-check, so they are serialized.
//!
//! Moving to the trash, restoring and deleting check only that the member still owns the account
//! ([`Media::owner_locked`]), not the participation requirement: a member who stops meeting it keeps managing the
//! files already uploaded, including restoring one before its trash period runs out.
use super::*;

/// Rows per page for both lists.
pub const LIBRARY_PAGE_SIZE: usize = 20;
/// States in the list that still belong to the member; `purging` and `purged` are already being cleaned up and are no
/// longer listed.
const LISTED: &str = "d.lifecycle IN ('pending','processing','ready','rejected')";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryView {
    /// Attachments not in the trash.
    Kept,
    /// The trash.
    Trash,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UsageBucket {
    pub files: i64,
    pub bytes: i64,
}

impl UsageBucket {
    fn add(&mut self, files: i64, bytes: i64) {
        self.files = self.files.saturating_add(files);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

/// Storage used by the member's own attachments. There is no total storage limit; it is only displayed.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    /// Referenced by posts, avatars, brand images or forum icons.
    pub in_use: UsageBucket,
    /// Passed the check but not referenced; cleaned up automatically when the unused period ends.
    pub unused: UsageBucket,
    /// The trash.
    pub trash: UsageBucket,
    /// Still being checked; the size is the original size.
    pub checking: UsageBucket,
    /// Failed the check; there is no stored object, so it takes no space.
    pub rejected: i64,
    /// Upload bytes used today (the calendar day of the site time zone) and the limit.
    pub daily_used: u64,
    pub daily_max: u64,
}

impl Usage {
    /// Total stored: in use, unused and in the trash.
    pub fn stored(&self) -> UsageBucket {
        let mut total = self.in_use;
        total.add(self.unused.files, self.unused.bytes);
        total.add(self.trash.files, self.trash.bytes);
        total
    }

    /// Row count of the "Attachments" list: everything not in the trash.
    pub fn kept_files(&self) -> i64 {
        self.in_use.files + self.unused.files + self.checking.files + self.rejected
    }
}

/// One entry in the list, with where it is used.
pub struct LibraryItem {
    pub upload: Upload,
    /// Number of posts referencing it, and the first of them the member can read, through which the image is
    /// previewed and which the row links to ("edit this post first, then delete"). `None` with posts means none of
    /// them is readable any more.
    pub posts: i64,
    pub first_pid: Option<i64>,
    /// It is the member's current avatar.
    pub avatar: bool,
}

pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub usage: Usage,
    pub total: usize,
}

impl Media {
    /// Trash retention period, in seconds.
    pub fn trash_seconds(&self) -> i64 {
        self.config.trash_seconds
    }

    /// Usage of the member's own attachments. Reads all of the member's attachments not yet purged (through
    /// `gonof_attachment_owner`) and groups and sums them in SQL.
    /// `offset_minutes` is the site time zone; "used today" counts by its calendar day.
    pub async fn usage(&self, actor: identity::Actor, offset_minutes: i64) -> Result<Usage> {
        if !self.active_actor(actor).await? {
            return Err(MediaError::Forbidden);
        }
        let rows = crate::db::query(&format!(
            "SELECT d.lifecycle, d.purge_after IS NULL AS bound, d.trashed_at IS NULL AS kept, COUNT(*) AS files, CAST(SUM(d.filesize) AS SIGNED) AS bytes
             FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid
             WHERE a.uid=? AND {LISTED} GROUP BY d.lifecycle, bound, kept"
        ))
        .bind(actor.uid)
        .fetch_all(self.db.executor())
        .await?;
        let mut usage = Usage {
            daily_max: self.config.daily_max_bytes,
            ..Usage::default()
        };
        for row in rows {
            let files: i64 = row.try_get("files")?;
            let bytes = row.try_get::<Option<i64>, _>("bytes")?.unwrap_or(0);
            let bound = row.try_get::<i64, _>("bound")? != 0;
            let kept = row.try_get::<i64, _>("kept")? != 0;
            match db::text(&row, "lifecycle")?.as_str() {
                "rejected" => usage.rejected += files,
                "ready" if !kept => usage.trash.add(files, bytes),
                "ready" if bound => usage.in_use.add(files, bytes),
                "ready" => usage.unused.add(files, bytes),
                _ => usage.checking.add(files, bytes),
            }
        }
        usage.daily_used =
            Self::daily_bytes(&self.db, None, actor.uid, identity::now(), offset_minutes).await?;
        Ok(usage)
    }

    /// One page of "Attachments" or "Attachment bin", paged in SQL, with the total taken from the usage groups.
    pub async fn library(
        &self,
        actor: identity::Actor,
        view: LibraryView,
        page: usize,
        offset_minutes: i64,
    ) -> Result<LibraryPage> {
        let usage = self.usage(actor, offset_minutes).await?;
        let total = usize::try_from(match view {
            LibraryView::Kept => usage.kept_files(),
            LibraryView::Trash => usage.trash.files,
        })
        .unwrap_or(0);
        let pages = total.div_ceil(LIBRARY_PAGE_SIZE).max(1);
        if page == 0 || page > pages {
            return Ok(LibraryPage {
                items: Vec::new(),
                usage,
                total,
            });
        }
        let (filter, order) = match view {
            LibraryView::Kept => (format!("d.trashed_at IS NULL AND {LISTED}"), "a.aid DESC"),
            LibraryView::Trash => (
                "d.trashed_at IS NOT NULL AND d.lifecycle='ready'".to_owned(),
                "d.trashed_at DESC, a.aid DESC",
            ),
        };
        let rows = crate::db::query(&format!(
            "SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid
             WHERE a.uid=? AND {filter} ORDER BY {order} LIMIT ? OFFSET ?"
        ))
        .bind(actor.uid)
        .bind(i64::try_from(LIBRARY_PAGE_SIZE).unwrap_or(20))
        .bind(i64::try_from((page - 1) * LIBRARY_PAGE_SIZE).unwrap_or(0))
        .fetch_all(self.db.executor())
        .await?;
        let uploads = rows.iter().map(decode_upload).collect::<Result<Vec<_>>>()?;
        let mut posts = BTreeMap::new();
        if !uploads.is_empty() {
            let placeholders = vec!["?"; uploads.len()].join(",");
            let sql = format!(
                "SELECT aid, COUNT(*) AS posts FROM gonof_forum_attachment_reference WHERE aid IN ({placeholders}) GROUP BY aid"
            );
            let mut query = crate::db::query(&sql);
            for upload in &uploads {
                query = query.bind(upload.aid);
            }
            let counts = query.fetch_all(self.db.executor()).await?;
            let viewer = if counts.is_empty() {
                None
            } else {
                self.verified_viewer(actor).await?
            };
            for row in counts {
                let aid = row.try_get::<i64, _>("aid")?;
                let first = self.first_readable_post(aid, viewer).await?;
                posts.insert(aid, (row.try_get::<i64, _>("posts")?, first));
            }
        }
        let avatar: Option<i64> =
            crate::db::query_scalar("SELECT avatar_aid FROM gonof_common_member WHERE uid=?")
                .bind(actor.uid)
                .fetch_optional(self.db.executor())
                .await?
                .flatten();
        let items = uploads
            .into_iter()
            .map(|upload| {
                let (count, first) = posts.get(&upload.aid).copied().unwrap_or((0, None));
                LibraryItem {
                    posts: count,
                    first_pid: first,
                    avatar: avatar == Some(upload.aid),
                    upload,
                }
            })
            .collect();
        Ok(LibraryPage {
            items,
            usage,
            total,
        })
    }

    /// Moves to the trash: own attachment, passed the check, no references at all (posts, avatars, brand images, forum
    /// icons).
    /// Does nothing when already in the trash. Only affects the member and works as usual during read-only periods
    /// (consistent with cancelling an attachment).
    pub async fn trash_actor(&self, actor: identity::Actor, aid: i64) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        self.owner_locked(&mut tx, actor).await?;
        let row = crate::db::query("SELECT a.uid,d.lifecycle,d.scan_status,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .ok_or(MediaError::NotFound)?;
        if row.try_get::<i64, _>("uid")? != actor.uid {
            return Err(MediaError::NotFound);
        }
        if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
            tx.commit().await?;
            return Ok(());
        }
        if db::text(&row, "lifecycle")? != "ready" || db::text(&row, "scan_status")? != "clean" {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_trash_ready().into(),
            ));
        }
        if self.attachment_reference_count(&mut tx, aid).await? > 0 {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_trash_in_use().into(),
            ));
        }
        let now = identity::now();
        let expiry = now
            .checked_add(self.config.trash_seconds)
            .context("trash expiry out of range")?;
        crate::db::query(
            "UPDATE gonof_forum_attachment_0 SET trashed_at=?,purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=?",
        )
        .bind(now)
        .bind(expiry)
        .bind(now)
        .bind(aid)
        .execute(self.db.observe(&mut *tx))
        .await?;
        self.audit(
            &mut tx,
            actor.uid,
            "media.trash",
            serde_json::json!({"aid":aid,"purge_after":expiry}),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Restores from the trash: no re-check, goes back to "Unused", and the unused period starts over.
    /// Attachments the cleanup task has already taken over (`purging`/`purged`), or that expired before the cleanup
    /// ran, cannot be restored.
    pub async fn restore_actor(&self, actor: identity::Actor, aid: i64) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        self.owner_locked(&mut tx, actor).await?;
        let row = crate::db::query("SELECT a.uid,d.lifecycle,d.purge_after,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .ok_or(MediaError::NotFound)?;
        if row.try_get::<i64, _>("uid")? != actor.uid {
            return Err(MediaError::NotFound);
        }
        if row.try_get::<Option<i64>, _>("trashed_at")?.is_none() {
            return Err(MediaError::NotFound);
        }
        let now = identity::now();
        if db::text(&row, "lifecycle")? != "ready"
            || row
                .try_get::<Option<i64>, _>("purge_after")?
                .is_some_and(|expiry| expiry <= now)
        {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_error_trash_expired().into(),
            ));
        }
        let expiry = now
            .checked_add(self.config.orphan_seconds)
            .context("attachment purge time out of range")?;
        crate::db::query("UPDATE gonof_forum_attachment_0 SET trashed_at=NULL,purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=?")
            .bind(expiry)
            .bind(now)
            .bind(aid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        self.audit(
            &mut tx,
            actor.uid,
            "media.restore",
            serde_json::json!({"aid":aid,"purge_after":expiry}),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes permanently from "My attachments": an attachment in the trash, or one that failed the check. Purged
    /// right away like a cancelled upload; anything else, such as an attachment restored since the page was opened,
    /// is refused.
    pub async fn delete_actor(&self, actor: identity::Actor, aid: i64) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        self.owner_locked(&mut tx, actor).await?;
        self.purge_own(tx, actor.uid, aid, true).await
    }
}
