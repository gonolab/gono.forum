//! Favorites and per-floor positive feedback: relations are facts, counts are aggregated when read.
use crate::db::DbRow;
use crate::db::Tx;
use std::collections::HashMap;

use crate::{
    content::{self, writes::ContentWriteError},
    db::{self, Database},
    identity::{self, Actor},
};

type Result<T> = std::result::Result<T, ContentWriteError>;
pub const PAGE_SIZE: usize = 20;

pub(crate) mod counters;
pub mod forum_filter;
mod highlights;
pub mod social;
pub use highlights::{Highlight, HighlightRules, highlights, rank_highlights};

#[derive(Clone, Debug)]
pub struct Target {
    pub pid: i64,
    pub tid: i64,
    pub fid: i64,
    pub authorid: Option<i64>,
    pub visible: bool,
    pub position: i64,
    pub root_pid: Option<i64>,
    pub first: bool,
    /// The thread or first post has been withdrawn by its author. Only meaningful when `visible`.
    pub withdrawn: bool,
    pub title: String,
}

fn target_sql(post_condition: &str) -> String {
    target_sql_for(content::PUBLIC_THREAD, post_condition)
}

/// `thread_condition` is the readability condition for forum and thread: `PUBLIC_THREAD` for the public view,
/// `readable_thread_sql(Some(uid))` for the view of a given authenticated member.
fn target_sql_for(thread_condition: &str, post_condition: &str) -> String {
    format!(
        "SELECT p.pid,p.tid,p.fid,p.authorid,p.position,p.root_pid,p.first,
            CASE WHEN {thread_condition} AND {post_condition} THEN 1 ELSE 0 END AS visible,
            t.subject,t.deleted_at,firstpost.deleted_at AS first_deleted_at
         FROM gonof_forum_post p
         JOIN gonof_forum_thread t ON t.tid=p.tid
         JOIN gonof_forum_forum f ON f.fid=t.fid
         JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid
         WHERE p.pid=?"
    )
}

fn decode_target(row: DbRow) -> anyhow::Result<Target> {
    let visible = row.try_get::<i64, _>("visible")? == 1;
    let withdrawn =
        row.try_get::<i64, _>("deleted_at")? > 0 || row.try_get::<i64, _>("first_deleted_at")? > 0;
    let title = if !visible {
        crate::i18n::current().my_content_unavailable().into()
    } else if withdrawn {
        crate::i18n::current().common_withdrawn_title().into()
    } else {
        db::text(&row, "subject")?
    };
    Ok(Target {
        pid: row.try_get("pid")?,
        tid: row.try_get("tid")?,
        fid: row.try_get("fid")?,
        authorid: row.try_get("authorid")?,
        position: row.try_get("position")?,
        root_pid: row.try_get("root_pid")?,
        first: row.try_get::<i64, _>("first")? == 1,
        visible,
        withdrawn,
        title,
    })
}

/// Neither nonexistent bodies nor bodies without public permission return a content snapshot to the caller.
pub async fn target(db: &Database, pid: i64) -> anyhow::Result<Option<Target>> {
    crate::db::query(&target_sql(&content::public_post("p")))
        .bind(pid)
        .fetch_optional(db.executor())
        .await?
        .map(decode_target)
        .transpose()
}

/// All interaction writes first lock the account and recheck credentials; reading lists may use verified=false.
/// Returns the mute expiry on the member row (0 for none); the few actions not allowed while muted (likes, downvotes)
/// check it themselves, while favorites, follows and so on proceed as usual.
pub async fn lock_actor(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    verified: bool,
) -> Result<i64> {
    let row=crate::db::query("SELECT credentials_version,`freeze`,mute_until,emailstatus,mobile,adminid,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(actor.uid).fetch_optional(db.observe(&mut **tx)).await?.ok_or(ContentWriteError::Unauthorized)?;
    if row.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || db::text(&row, "account_state")? != "active"
    {
        return Err(ContentWriteError::Unauthorized);
    }
    if row.try_get::<i64, _>("freeze")? != 0 {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().my_error_frozen(),
        ));
    }
    if !crate::identity::api_key_locked(db, tx, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    let participation = crate::login::Participation::current();
    if verified
        && row.try_get::<i64, _>("adminid")? != 1
        && !crate::login::permits(
            db.observe(&mut **tx),
            actor.uid,
            row.try_get::<i64, _>("emailstatus")? == 1,
            row.try_get::<Option<String>, _>("mobile")?.is_some(),
        )
        .await?
    {
        return Err(ContentWriteError::Forbidden(
            participation.message(crate::i18n::current().governance_error_verify_email()),
        ));
    }
    Ok(row.try_get("mute_until")?)
}

/// Rejects while muted (docs/roles.md, "Mutes"): externally visible interactions such as likes and downvotes.
pub(crate) fn reject_muted(mute_until: i64) -> Result<()> {
    if crate::moderation::muted(mute_until, crate::identity::now()) {
        return Err(ContentWriteError::Restricted(
            crate::moderation::muted_message(mute_until),
        ));
    }
    Ok(())
}

/// Discovery and reading grant no permission. The lock order matches posting/moderation; the final query must use a
/// current read.
///
/// The forum row and target floor are read with shared locks: interactions do not change the forum row, and a shared
/// lock is enough to queue exclusive writes such as forum management and moderation hiding behind this transaction.
/// A like on a reply does update the target floor's `reactions` counter afterwards, upgrading that lock; this cannot
/// deadlock with another interaction, because every interaction first holds the thread row exclusively, so
/// interactions on one thread run one at a time. The thread row and first post get exclusive locks: favorites, tips,
/// downvotes and likes on the first post write counters on them, and taking a shared lock first and then upgrading
/// would make two such writes on the same thread deadlock each other. Follows and opening notifications also go through this lock;
/// whether each of them could do with a shared lock has not been assessed.
pub async fn lock_target(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<Target> {
    lock_target_with(db, tx, pid, content::PUBLIC_THREAD).await
}

/// Locks the same way as [`lock_target`], but `visible` is decided by what this authenticated member can read:
/// forums and threads visible to members count as visible, while private forums and floors hidden by moderation,
/// withdrawn or erased stay invisible.
/// Blocking is ignored: the caller (reporting) must keep working for authors the member has blocked.
pub async fn lock_target_readable_by(
    db: &Database,
    tx: &mut Tx<'_>,
    pid: i64,
    uid: i64,
) -> Result<Target> {
    lock_target_with(db, tx, pid, &content::readable_thread_sql(Some(uid))).await
}

async fn lock_target_with(
    db: &Database,
    tx: &mut Tx<'_>,
    pid: i64,
    thread_condition: &str,
) -> Result<Target> {
    let found = crate::db::query("SELECT fid,tid,root_pid FROM gonof_forum_post WHERE pid=?")
        .bind(pid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let fid: i64 = found.try_get("fid")?;
    let tid: i64 = found.try_get("tid")?;
    let root: Option<i64> = found.try_get("root_pid")?;
    crate::organization::share_forum_path(db, tx, fid).await?;
    crate::db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    crate::db::query("SELECT pid FROM gonof_forum_post WHERE first_tid=? FOR UPDATE")
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let parent_visible = if let Some(root) = root {
        let parent=crate::db::query("SELECT invisible FROM gonof_forum_post WHERE pid=? AND tid=? AND first=0 AND root_pid IS NULL FOR UPDATE")
            .bind(root).bind(tid).fetch_optional(db.observe(&mut **tx)).await?.ok_or(ContentWriteError::NotFound)?;
        parent.try_get::<i64, _>("invisible")? == 0
    } else {
        true
    };
    // The parent floor state comes from the current read above, so the MySQL subquery does not use an old snapshot
    // established by the account idempotency query.
    let condition = if parent_visible {
        content::public_post_body("p")
    } else {
        "1=0".into()
    };
    // This join locks every row involved, including the forum row, so only a shared lock can be used (5.7 has no FOR
    // SHARE, nor FOR UPDATE OF to lock only some tables). The thread row and first post already got exclusive locks
    // above.
    let row = crate::db::query(&format!(
        "{}{}",
        target_sql_for(thread_condition, &condition),
        " LOCK IN SHARE MODE"
    ))
    .bind(pid)
    .fetch_optional(db.observe(&mut **tx))
    .await?
    .ok_or(ContentWriteError::NotFound)?;
    let target = decode_target(row)?;
    if target.fid != fid || target.tid != tid || target.root_pid != root {
        return Err(ContentWriteError::NotFound);
    }
    Ok(target)
}

#[derive(Clone)]
pub struct Interaction {
    db: Database,
}

impl Interaction {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn set_favorite(
        &self,
        actor: Actor,
        tid: i64,
        desired: bool,
        key: &str,
    ) -> Result<()> {
        self.set(actor, tid, desired, key, true).await
    }

    pub async fn set_reaction(
        &self,
        actor: Actor,
        pid: i64,
        desired: bool,
        key: &str,
    ) -> Result<()> {
        self.set(actor, pid, desired, key, false).await
    }

    async fn set(
        &self,
        actor: Actor,
        id: i64,
        desired: bool,
        key: &str,
        favorite: bool,
    ) -> Result<()> {
        if id <= 0 || !identity::valid_request_key(key) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().my_error_interaction_target(),
            ));
        }
        let operation = if favorite {
            "interaction.favorite"
        } else {
            "interaction.reaction"
        };
        let hash = identity::token_hash(
            &serde_json::json!({"operation":operation,"id":id,"desired":desired}).to_string(),
        );
        let mut tx = self.db.begin_write().await?;
        // Read-only mode only blocks public content: like counts are publicly visible, so they are blocked; favorites
        // are only visible to the member, so the website allows them. The member API refuses every write in read-only
        // mode (docs/api.md, "Account and site rules"), so an API favorite reads the switch under the gate lock too.
        if favorite && actor.api_key.is_some() {
            crate::runtime::guard_write(&self.db, &mut tx, false)
                .await
                .map_err(|error| ContentWriteError::Internal(error.into()))?;
        }
        if !favorite {
            crate::runtime::guard_write(&self.db, &mut tx, false)
                .await
                .map_err(|error| match error {
                    identity::IdentityError::Suspended(_) | identity::IdentityError::Forbidden => {
                        ContentWriteError::Forbidden(
                            crate::i18n::current().my_error_read_only_like(),
                        )
                    }
                    other => ContentWriteError::Internal(other.into()),
                })?;
        }
        let mute_until = lock_actor(&self.db, &mut tx, actor, true).await?;
        if !favorite {
            reject_muted(mute_until)?;
        }
        // Do not lock nonexistent request keys, to avoid MySQL gap-lock deadlocks between different accounts.
        if let Some(row) = crate::db::query(
            "SELECT request_hash FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
        )
        .bind(actor.uid)
        .bind(key)
        .fetch_optional(self.db.observe(&mut *tx))
        .await?
        {
            if db::text(&row, "request_hash")? != hash {
                return Err(ContentWriteError::Conflict(
                    crate::i18n::current().my_error_request_used_refresh(),
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        let pid = if favorite {
            crate::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE first_tid=?")
                .bind(id)
                .fetch_optional(self.db.observe(&mut *tx))
                .await?
                .ok_or(ContentWriteError::NotFound)?
        } else {
            id
        };
        let target = social::lock_target_for(&self.db, &mut tx, pid, actor.uid).await?;
        if desired && !target.visible {
            return Err(ContentWriteError::NotFound);
        }
        if !favorite && desired && target.authorid == Some(actor.uid) {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().my_error_like_own(),
            ));
        }
        let (table, column) = if favorite {
            ("gonof_home_favorite", "tid")
        } else {
            ("gonof_forum_post_reaction", "pid")
        };
        // Likes count towards the author's growth value: lock this post's growth row before writing the like relation,
        // so that counting likes sees all committed likes.
        if !favorite {
            crate::growth::lock_post(&self.db, &mut tx, pid).await?;
        }
        if desired {
            let conflict = " ON DUPLICATE KEY UPDATE uid=uid";
            // This table only has the relation primary key; INSERT IGNORE, which would swallow foreign key/check
            // failures, is not used.
            crate::db::query(&format!(
                "INSERT INTO {table}(uid,{column},dateline) VALUES(?,?,?){conflict}"
            ))
            .bind(actor.uid)
            .bind(id)
            .bind(identity::now())
            .execute(self.db.observe(&mut *tx))
            .await?;
            // Likes and downvotes are mutually exclusive: liking withdraws the member's downvote on this floor in the
            // same transaction.
            if !favorite {
                let undone =
                    crate::db::query("DELETE FROM gonof_forum_post_downvote WHERE pid=? AND uid=?")
                        .bind(id)
                        .bind(actor.uid)
                        .execute(self.db.observe(&mut *tx))
                        .await?;
                if undone.rows_affected() > 0 {
                    counters::recount_downvotes(&self.db, &mut tx, id).await?;
                }
            }
        } else {
            crate::db::query(&format!("DELETE FROM {table} WHERE uid=? AND {column}=?"))
                .bind(actor.uid)
                .bind(id)
                .execute(self.db.observe(&mut *tx))
                .await?;
        }
        // Favorite and like counts (the `favorites` and `reactions` counter columns): the thread row is exclusively
        // locked; recount from the details and write back.
        if favorite {
            counters::recount_favorites(&self.db, &mut tx, id).await?;
        } else {
            counters::recount_reactions(&self.db, &mut tx, id).await?;
        }
        if !favorite {
            let cause = if desired {
                crate::growth::Cause::Like
            } else {
                crate::growth::Cause::Unlike
            };
            crate::growth::sync_posts(&self.db, &mut tx, &[pid], cause, identity::now()).await?;
        }
        crate::identity::rules::insert_post_request(
            &self.db,
            &mut tx,
            actor.uid,
            key,
            &hash,
            pid,
            identity::now(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Explicitly sets the member's downvote on a thread. Only applies to the first post; mutually exclusive with
    /// likes, so downvoting withdraws the member's like on this floor in the same transaction. Thresholds, lock order
    /// and idempotency follow likes: lock the member's own row first, then recheck readability with a shared lock on
    /// the forum and exclusive locks on the thread and first post; request keys reuse `gonof_forum_post_request`.
    pub async fn set_downvote(
        &self,
        actor: Actor,
        pid: i64,
        desired: bool,
        key: &str,
    ) -> Result<()> {
        if pid <= 0 || !identity::valid_request_key(key) {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().my_error_interaction_target(),
            ));
        }
        let hash = identity::token_hash(
            &serde_json::json!({"operation":"interaction.downvote","id":pid,"desired":desired})
                .to_string(),
        );
        let mut tx = self.db.begin_write().await?;
        crate::runtime::guard_write(&self.db, &mut tx, false)
            .await
            .map_err(|error| match error {
                identity::IdentityError::Suspended(_) | identity::IdentityError::Forbidden => {
                    ContentWriteError::Forbidden(
                        crate::i18n::current().my_error_read_only_downvote(),
                    )
                }
                other => ContentWriteError::Internal(other.into()),
            })?;
        reject_muted(lock_actor(&self.db, &mut tx, actor, true).await?)?;
        if let Some(row) = crate::db::query(
            "SELECT request_hash FROM gonof_forum_post_request WHERE uid=? AND request_key=?",
        )
        .bind(actor.uid)
        .bind(key)
        .fetch_optional(self.db.observe(&mut *tx))
        .await?
        {
            if db::text(&row, "request_hash")? != hash {
                return Err(ContentWriteError::Conflict(
                    crate::i18n::current().my_error_request_used_refresh(),
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        let target = social::lock_target_for(&self.db, &mut tx, pid, actor.uid).await?;
        if !target.first {
            return Err(ContentWriteError::NotFound);
        }
        if desired {
            if !target.visible {
                return Err(ContentWriteError::NotFound);
            }
            if target.authorid == Some(actor.uid) {
                return Err(ContentWriteError::Forbidden(
                    crate::i18n::current().my_error_downvote_own(),
                ));
            }
            // When the member has blocked the author, `visible` is already false; a member blocked by the author
            // cannot downvote either, consistent with private messages.
            if let Some(author) = target.authorid
                && blocked_by(&self.db, &mut tx, author, actor.uid).await?
            {
                return Err(ContentWriteError::Forbidden(
                    crate::i18n::current().my_error_downvote_unavailable(),
                ));
            }
            // A downvote withdraws the member's like, and the author's growth value is deducted accordingly: same lock
            // order as likes, locking this post's growth row first.
            crate::growth::lock_post(&self.db, &mut tx, pid).await?;
            crate::db::query(
                "INSERT INTO gonof_forum_post_downvote(pid,uid,dateline) VALUES(?,?,?) ON DUPLICATE KEY UPDATE uid=uid",
            )
            .bind(pid)
            .bind(actor.uid)
            .bind(identity::now())
            .execute(self.db.observe(&mut *tx))
            .await?;
            counters::recount_downvotes(&self.db, &mut tx, pid).await?;
            let unliked =
                crate::db::query("DELETE FROM gonof_forum_post_reaction WHERE pid=? AND uid=?")
                    .bind(pid)
                    .bind(actor.uid)
                    .execute(self.db.observe(&mut *tx))
                    .await?;
            if unliked.rows_affected() > 0 {
                counters::recount_reactions(&self.db, &mut tx, pid).await?;
            }
            crate::growth::sync_posts(
                &self.db,
                &mut tx,
                &[pid],
                crate::growth::Cause::Unlike,
                identity::now(),
            )
            .await?;
        } else {
            let undone =
                crate::db::query("DELETE FROM gonof_forum_post_downvote WHERE pid=? AND uid=?")
                    .bind(pid)
                    .bind(actor.uid)
                    .execute(self.db.observe(&mut *tx))
                    .await?;
            if undone.rows_affected() > 0 {
                counters::recount_downvotes(&self.db, &mut tx, pid).await?;
            }
        }
        crate::identity::rules::insert_post_request(
            &self.db,
            &mut tx,
            actor.uid,
            key,
            &hash,
            pid,
            identity::now(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Whether `owner` has blocked `uid`. Called in a transaction that already locked both members or the content.
pub async fn blocked_by(db: &Database, tx: &mut Tx<'_>, owner: i64, uid: i64) -> Result<bool> {
    let found: Option<i64> = crate::db::query_scalar(
        "SELECT 1 FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
    )
    .bind(owner)
    .bind(uid)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    Ok(found.is_some())
}

/// The page state of a thread downvote: whether the member downvoted; the count is only for the thread author.
#[derive(Clone, Copy, Debug, Default)]
pub struct DownvoteState {
    pub active: bool,
    pub count: Option<i64>,
}

/// Two primary-key point lookups: whether the member downvoted, and the downvote count the author sees. Anonymous
/// visitors and non-authors do not query the count.
pub async fn downvote_state(
    db: &Database,
    viewer: Option<i64>,
    pid: i64,
    author: Option<i64>,
) -> anyhow::Result<DownvoteState> {
    let Some(uid) = viewer else {
        return Ok(DownvoteState::default());
    };
    if author == Some(uid) {
        // The downvote count is read from the counter column on the first post.
        let count: i64 =
            crate::db::query_scalar("SELECT downvotes FROM gonof_forum_post WHERE pid=?")
                .bind(pid)
                .fetch_optional(db.executor())
                .await?
                .unwrap_or(0);
        return Ok(DownvoteState {
            active: false,
            count: Some(count),
        });
    }
    let active: Option<i64> =
        crate::db::query_scalar("SELECT 1 FROM gonof_forum_post_downvote WHERE pid=? AND uid=?")
            .bind(pid)
            .bind(uid)
            .fetch_optional(db.executor())
            .await?;
    Ok(DownvoteState {
        active: active.is_some(),
        count: None,
    })
}

/// The reader's state for a thread (docs/queries.md, "Thread page"): favorite, follow, whether the thread starter
/// blocked the reader, downvote on the first post, and whether the reader tipped. One statement anchored on the thread
/// row's primary key reads them all, and all five tables are constant lookups on primary or unique keys.
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewerThreadState {
    pub favorited: bool,
    pub followed: bool,
    pub blocked_by_author: bool,
    pub downvoted: bool,
    pub tipped: bool,
}

pub async fn viewer_thread_state(
    db: &Database,
    uid: i64,
    tid: i64,
    first_pid: i64,
) -> anyhow::Result<ViewerThreadState> {
    let row = crate::db::query(
        "SELECT fav.tid AS favorited,fol.tid AS followed,blk.uid AS blocked_by_author,dv.uid AS downvoted,tip.id AS tipped
        FROM gonof_forum_thread t
        LEFT JOIN gonof_home_favorite fav ON fav.uid=? AND fav.tid=t.tid
        LEFT JOIN gonof_home_thread_follow fol ON fol.uid=? AND fol.tid=t.tid
        LEFT JOIN gonof_home_member_block blk ON blk.uid=t.authorid AND blk.blocked_uid=?
        LEFT JOIN gonof_forum_post_downvote dv ON dv.pid=? AND dv.uid=?
        LEFT JOIN gonof_common_credit_ledger tip ON tip.event_key=?
        WHERE t.tid=?",
    )
    .bind(uid)
    .bind(uid)
    .bind(uid)
    .bind(first_pid)
    .bind(uid)
    .bind(crate::activities::tip_out_key(tid, uid))
    .bind(tid)
    .fetch_optional(db.executor())
    .await?;
    let Some(row) = row else {
        return Ok(ViewerThreadState::default());
    };
    let found = |column: &str| -> anyhow::Result<bool> {
        Ok(row.try_get::<Option<i64>, _>(column)?.is_some())
    };
    Ok(ViewerThreadState {
        favorited: found("favorited")?,
        followed: found("followed")?,
        blocked_by_author: found("blocked_by_author")?,
        downvoted: found("downvoted")?,
        tipped: found("tipped")?,
    })
}

/// Whether the reader is blocked by `owner` (for pages, without locking).
pub async fn is_blocked_by(db: &Database, owner: i64, uid: i64) -> anyhow::Result<bool> {
    let found: Option<i64> = crate::db::query_scalar(
        "SELECT 1 FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
    )
    .bind(owner)
    .bind(uid)
    .fetch_optional(db.executor())
    .await?;
    Ok(found.is_some())
}

pub async fn favorite_state(db: &Database, uid: i64, tid: i64) -> anyhow::Result<bool> {
    let count: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=? AND tid=?")
            .bind(uid)
            .bind(tid)
            .fetch_one(db.executor())
            .await?;
    Ok(count != 0)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReactionState {
    pub count: i64,
    pub active: bool,
}

/// Like counts are read from the counter column on post rows, and logged-in members also join their own row, all
/// read in batches by primary key instead of GROUP BY over the details; state and counts come from the same snapshot
/// of one statement. Batching limits the number of bind parameters in a single MySQL query.
pub async fn reactions(
    db: &Database,
    uid: Option<i64>,
    pids: &[i64],
) -> anyhow::Result<HashMap<i64, ReactionState>> {
    let mut result = HashMap::new();
    for ids in pids.chunks(400) {
        if ids.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = if uid.is_some() {
            format!(
                "SELECT p.pid,p.reactions,r.uid AS mine FROM gonof_forum_post p LEFT JOIN gonof_forum_post_reaction r ON r.pid=p.pid AND r.uid=? WHERE p.pid IN ({placeholders})"
            )
        } else {
            format!("SELECT pid,reactions FROM gonof_forum_post WHERE pid IN ({placeholders})")
        };
        let mut query = crate::db::query(&sql);
        if let Some(uid) = uid {
            query = query.bind(uid);
        }
        for pid in ids {
            query = query.bind(pid);
        }
        for row in query.fetch_all(db.executor()).await? {
            let active = uid.is_some() && row.try_get::<Option<i64>, _>("mine")?.is_some();
            result.insert(
                row.try_get("pid")?,
                ReactionState {
                    count: row.try_get("reactions")?,
                    active,
                },
            );
        }
        for pid in ids {
            result.entry(*pid).or_default();
        }
    }
    Ok(result)
}

pub struct FavoriteItem {
    pub tid: i64,
    pub title: String,
    pub available: bool,
    pub dateline: i64,
    pub time: String,
}
pub struct FavoritePage {
    pub rows: Vec<FavoriteItem>,
    pub total: usize,
}

/// `filters` are the member's own forum filters; favorites in blocked forums show a placeholder and can still be
/// removed.
pub async fn favorites(
    db: &Database,
    uid: i64,
    page: usize,
    filters: &forum_filter::ForumFilters,
) -> Result<FavoritePage> {
    let total: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await?;
    let total = usize::try_from(total).map_err(|_| ContentWriteError::NotFound)?;
    if page == 0 || page > total.div_ceil(PAGE_SIZE).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    // Forum readability uses the joined forum table (the thread row carries no forum copies), first post visibility
    // uses the first post itself, and blocking uses the list read for the member. Thread withdrawal and first post
    // withdrawal are written in the same statement; both are checked here.
    let blocked = content::blocked_authors(db, uid).await?;
    let visible = format!(
        "{} AND t.deleted_at = 0 AND {}{}",
        content::thread_visible_by_forum_sql("f", Some(uid)),
        content::post_visible("firstpost"),
        content::not_blocked("t.authorid", &blocked)
    );
    let sql = &format!(
        "SELECT fav.tid,fav.dateline,CASE WHEN {visible} THEN 1 ELSE 0 END AS available,
        t.subject,t.fid,f.parent_fid
        FROM gonof_home_favorite fav JOIN gonof_forum_thread t ON t.tid=fav.tid
        JOIN gonof_forum_forum f ON f.fid=t.fid
        JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid
        WHERE fav.uid=? ORDER BY fav.dateline DESC,fav.tid DESC LIMIT ? OFFSET ?"
    );
    let rows = crate::db::query(sql)
        .bind(uid)
        .bind(PAGE_SIZE as i64)
        .bind(((page - 1) * PAGE_SIZE) as i64)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| -> anyhow::Result<FavoriteItem> {
            let dateline: i64 = row.try_get("dateline")?;
            let available = row.try_get::<i64, _>("available")? != 0
                && !filters.blocks(row.try_get("fid")?, row.try_get("parent_fid")?);
            Ok(FavoriteItem {
                tid: row.try_get("tid")?,
                title: if available {
                    db::text(&row, "subject")?
                } else {
                    crate::i18n::current().my_content_unavailable().into()
                },
                available,
                dateline,
                time: crate::clock::datetime(dateline, &crate::clock::current()),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(FavoritePage { rows, total })
}
