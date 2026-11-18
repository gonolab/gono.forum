//! Private manual subscriptions, member follows and one-way member blocks.
use crate::db::Tx;
use std::collections::BTreeSet;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Thread,
    Forum,
    Block,
    /// Following a member: one-way, no consent from the other side needed.
    Member,
}

/// The maximum number of members one person can follow. The follow-feed query is bounded by it.
pub const MEMBER_FOLLOW_LIMIT: i64 = 500;

impl Relation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Forum => "forum",
            Self::Block => "block",
            Self::Member => "member",
        }
    }
    /// The audit event of a change, spelled out so that the registry scan in `manage.rs` finds it.
    fn log_type(self) -> &'static str {
        match self {
            Self::Thread => "interaction.thread",
            Self::Forum => "interaction.forum",
            Self::Block => "interaction.block",
            Self::Member => "interaction.member",
        }
    }
    fn storage(self) -> (&'static str, &'static str) {
        match self {
            Self::Thread => ("gonof_home_thread_follow", "tid"),
            Self::Forum => ("gonof_home_forum_follow", "fid"),
            Self::Block => ("gonof_home_member_block", "blocked_uid"),
            Self::Member => ("gonof_home_member_follow", "followed_uid"),
        }
    }
}

pub async fn blocked_uids(
    db: &Database,
    uid: i64,
    candidates: &[i64],
) -> anyhow::Result<BTreeSet<i64>> {
    anyhow::ensure!(candidates.len() <= 500, "block lookup exceeds one page");
    if candidates.is_empty() {
        return Ok(BTreeSet::new());
    }
    let placeholders = vec!["?"; candidates.len()].join(",");
    let sql = format!(
        "SELECT blocked_uid FROM gonof_home_member_block WHERE uid=? AND blocked_uid IN ({placeholders})"
    );
    let mut query = crate::db::query_scalar::<i64>(&sql).bind(uid);
    for candidate in candidates {
        query = query.bind(*candidate);
    }
    Ok(query.fetch_all(db.executor()).await?.into_iter().collect())
}

pub async fn following(
    db: &Database,
    uid: i64,
    target: i64,
    relation: Relation,
) -> anyhow::Result<bool> {
    let (table, column) = relation.storage();
    Ok(crate::db::query_scalar::<i64>(&format!(
        "SELECT COUNT(*) FROM {table} WHERE uid=? AND {column}=?"
    ))
    .bind(uid)
    .bind(target)
    .fetch_one(db.executor())
    .await?
        > 0)
}

/// Reuse the common content locks, then evaluate the authenticated reader's visibility.
/// The caller has already locked and authenticated uid.
pub async fn lock_target_for(db: &Database, tx: &mut Tx<'_>, pid: i64, uid: i64) -> Result<Target> {
    let mut target = super::lock_target(db, tx, pid).await?;
    let visibility = content::readable_thread_sql(Some(uid));
    let post = content::public_post("p");
    let row=crate::db::query(&format!("SELECT t.subject,t.deleted_at,firstpost.deleted_at AS first_deleted_at,
        CASE WHEN {visibility} AND {post} AND NOT EXISTS(SELECT 1 FROM gonof_home_member_block b WHERE b.uid=? AND b.blocked_uid=p.authorid) THEN 1 ELSE 0 END AS available
        FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_forum f ON f.fid=t.fid
        JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE p.pid=? LOCK IN SHARE MODE"))
        .bind(uid).bind(pid).fetch_one(db.observe(&mut **tx)).await?;
    target.visible = row.try_get::<i64, _>("available")? == 1;
    target.withdrawn =
        row.try_get::<i64, _>("deleted_at")? > 0 || row.try_get::<i64, _>("first_deleted_at")? > 0;
    target.title = if !target.visible {
        crate::i18n::current().my_content_unavailable().into()
    } else if target.withdrawn {
        crate::i18n::current().common_withdrawn_title().into()
    } else {
        db::text(&row, "subject")?
    };
    Ok(target)
}

pub async fn set_follow_thread(
    db: &Database,
    actor: Actor,
    tid: i64,
    desired: bool,
    key: &str,
) -> Result<()> {
    set(db, actor, tid, desired, key, Relation::Thread).await
}
pub async fn set_follow_forum(
    db: &Database,
    actor: Actor,
    fid: i64,
    desired: bool,
    key: &str,
) -> Result<()> {
    set(db, actor, fid, desired, key, Relation::Forum).await
}
pub async fn set_follow_member(
    db: &Database,
    actor: Actor,
    target: i64,
    desired: bool,
    key: &str,
) -> Result<()> {
    set(db, actor, target, desired, key, Relation::Member).await
}
pub async fn set_block(
    db: &Database,
    actor: Actor,
    target: i64,
    desired: bool,
    key: &str,
) -> Result<()> {
    set(db, actor, target, desired, key, Relation::Block).await
}

pub async fn set(
    db: &Database,
    actor: Actor,
    target: i64,
    desired: bool,
    key: &str,
    relation: Relation,
) -> Result<()> {
    if target <= 0 || !identity::valid_request_key(key) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().social_error_target(),
        ));
    }
    if relation == Relation::Block && target == actor.uid {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().social_error_block_self(),
        ));
    }
    if relation == Relation::Member && target == actor.uid {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().social_error_follow_self(),
        ));
    }
    let hash = identity::token_hash(
        &serde_json::json!({"relation":relation.name(),"target":target,"desired":desired})
            .to_string(),
    );
    // Following threads and forums and blocking only affect oneself and are invisible to others, so they work as usual
    // in read-only mode. Following a member changes their public follower count, so it is
    // blocked in read-only mode.
    let mut tx = db.begin_write().await?;
    if relation == Relation::Member {
        crate::runtime::guard_write(db, &mut tx, false)
            .await
            .map_err(|error| match error {
                identity::IdentityError::Suspended(_) | identity::IdentityError::Forbidden => {
                    ContentWriteError::Forbidden(crate::i18n::current().social_error_read_only())
                }
                other => ContentWriteError::Internal(other.into()),
            })?;
    }
    // Blocks and member follows coordinate through both account rows in UID order.
    let mut target_state = String::new();
    if matches!(relation, Relation::Block | Relation::Member) {
        for uid in BTreeSet::from([actor.uid, target]) {
            if uid == actor.uid {
                lock_actor(db, &mut tx, actor, relation == Relation::Member).await?;
            } else {
                let row = crate::db::query(
                    "SELECT account_state FROM gonof_common_member WHERE uid=? FOR UPDATE",
                )
                .bind(uid)
                .fetch_optional(db.observe(&mut *tx))
                .await?
                .ok_or(ContentWriteError::NotFound)?;
                target_state = db::text(&row, "account_state")?;
            }
        }
    } else {
        lock_actor(db, &mut tx, actor, true).await?;
    }
    if let Some(existing) = crate::db::query(
        "SELECT request_hash FROM gonof_home_interaction_request WHERE uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        if db::text(&existing, "request_hash")? != hash {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().my_error_request_used(),
            ));
        }
        tx.commit().await?;
        return Ok(());
    }
    match relation {
        Relation::Thread => {
            let pid: Option<i64> =
                crate::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE first_tid=?")
                    .bind(target)
                    .fetch_optional(db.observe(&mut *tx))
                    .await?;
            if let Some(pid) = pid {
                let content = lock_target_for(db, &mut tx, pid, actor.uid).await?;
                if desired && !content.visible {
                    return Err(ContentWriteError::NotFound);
                }
            } else {
                return Err(ContentWriteError::NotFound);
            }
        }
        Relation::Forum => {
            crate::organization::share_forum_path(db, &mut tx, target).await?;
            let visible = crate::organization::forum_readable_sql("f", Some(actor.uid));
            let exists: i64 = crate::db::query_scalar(&format!(
                "SELECT COUNT(*) FROM gonof_forum_forum f WHERE f.fid=? AND {visible}"
            ))
            .bind(target)
            .fetch_one(db.observe(&mut *tx))
            .await?;
            if desired && exists != 1 {
                return Err(ContentWriteError::NotFound);
            }
        }
        Relation::Block => (),
        Relation::Member if desired => {
            if target_state != "active" {
                return Err(ContentWriteError::NotFound);
            }
            // While blocked, the blocked member cannot follow the blocker, and the message does not reveal the block
            // (consistent with private messages).
            if super::blocked_by(db, &mut tx, target, actor.uid).await? {
                return Err(ContentWriteError::Forbidden(
                    crate::i18n::current().social_follow_unavailable(),
                ));
            }
            if super::blocked_by(db, &mut tx, actor.uid, target).await? {
                return Err(ContentWriteError::Forbidden(
                    crate::i18n::current().social_error_blocked(),
                ));
            }
        }
        Relation::Member => (),
    }
    let (table, column) = relation.storage();
    let timestamp = identity::now();
    let changed = if desired {
        // The connection has CLIENT_FOUND_ROWS on, so `ON DUPLICATE KEY UPDATE` also reports 1 row when the record
        // already exists and cannot tell whether one was added; hence query first, then insert. The actor's member row
        // is already locked above, so relation writes of the same member serialize here; this transaction's first
        // consistent read also comes after that lock and sees relations committed before.
        let exists: Option<i64> =
            crate::db::query_scalar(&format!("SELECT 1 FROM {table} WHERE uid=? AND {column}=?"))
                .bind(actor.uid)
                .bind(target)
                .fetch_optional(db.observe(&mut *tx))
                .await?;
        if exists.is_none() && relation == Relation::Member {
            let following: i64 = crate::db::query_scalar(
                "SELECT COUNT(*) FROM gonof_home_member_follow WHERE uid=?",
            )
            .bind(actor.uid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
            if following >= MEMBER_FOLLOW_LIMIT {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().social_error_follow_limit(),
                ));
            }
        }
        if exists.is_none() {
            crate::db::query(&format!(
                "INSERT INTO {table}(uid,{column},dateline) VALUES(?,?,?)"
            ))
            .bind(actor.uid)
            .bind(target)
            .bind(timestamp)
            .execute(db.observe(&mut *tx))
            .await?;
        }
        exists.is_none()
    } else {
        crate::db::query(&format!("DELETE FROM {table} WHERE uid=? AND {column}=?"))
            .bind(actor.uid)
            .bind(target)
            .execute(db.observe(&mut *tx))
            .await?
            .rows_affected()
            > 0
    };
    // Blocking cancels follows in both directions, and unblocking does not restore them. Both member rows are
    // already locked in ascending uid order.
    if relation == Relation::Block && desired {
        for (follower, followed) in [(actor.uid, target), (target, actor.uid)] {
            crate::db::query("DELETE FROM gonof_home_member_follow WHERE uid=? AND followed_uid=?")
                .bind(follower)
                .bind(followed)
                .execute(db.observe(&mut *tx))
                .await?;
        }
    }
    crate::identity::rules::insert_interaction_request(
        db, &mut tx, actor.uid, key, &hash, timestamp,
    )
    .await?;
    if changed {
        crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
            .bind(actor.uid).bind(actor.uid).bind(relation.log_type())
            .bind(serde_json::json!({"target":target,"desired":desired}).to_string()).bind(timestamp).execute(db.observe(&mut *tx)).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub struct RelationRow {
    pub target: i64,
    pub title: String,
    pub href: String,
    pub available: bool,
    pub dateline: i64,
}
pub struct RelationPage {
    pub rows: Vec<RelationRow>,
    pub total: i64,
}
/// `filters` is the member's own forum filter: followed threads in blocked forums show a placeholder and can still be
/// unfollowed.
pub async fn list(
    db: &Database,
    actor: Actor,
    relation: Relation,
    page: usize,
    filters: &super::forum_filter::ForumFilters,
) -> Result<RelationPage> {
    if page == 0 || page > i64::MAX as usize / PAGE_SIZE {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_page_number(),
        ));
    }
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    let (table, column) = relation.storage();
    let total: i64 = crate::db::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE uid=?"))
        .bind(actor.uid)
        .fetch_one(db.executor())
        .await?;
    if page > (total as usize).div_ceil(PAGE_SIZE).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    let sql=match relation {
        Relation::Thread => format!("SELECT r.{column} AS target,r.dateline,t.subject AS title,t.deleted_at,t.deleted_at AS first_deleted_at,t.fid,f.parent_fid,
            CASE WHEN {}{} THEN 1 ELSE 0 END AS available
            FROM {table} r JOIN gonof_forum_thread t ON t.tid=r.tid JOIN gonof_forum_forum f ON f.fid=t.fid
            WHERE r.uid=? ORDER BY r.dateline DESC,r.{column} DESC LIMIT ? OFFSET ?",content::thread_visible_by_forum_sql("f", Some(actor.uid)),content::not_blocked("t.authorid", &content::blocked_authors(db, actor.uid).await.map_err(ContentWriteError::Internal)?)),
        Relation::Forum => format!("SELECT r.{column} AS target,r.dateline,f.name AS title,f.slug,f.fid,f.parent_fid,CASE WHEN {} THEN 1 ELSE 0 END AS available
            FROM {table} r JOIN gonof_forum_forum f ON f.fid=r.fid WHERE r.uid=? ORDER BY r.dateline DESC,r.{column} DESC LIMIT ? OFFSET ?",crate::organization::forum_readable_sql("f",Some(actor.uid))),
        Relation::Block | Relation::Member => format!("SELECT r.{column} AS target,r.dateline,m.username AS title,m.account_state FROM {table} r JOIN gonof_common_member m ON m.uid=r.{column} WHERE r.uid=? ORDER BY r.dateline DESC,r.{column} DESC LIMIT ? OFFSET ?"),
    };
    let data = crate::db::query(&sql)
        .bind(actor.uid)
        .bind(PAGE_SIZE as i64)
        .bind(((page - 1) * PAGE_SIZE) as i64)
        .fetch_all(db.executor())
        .await?;
    let mut rows = Vec::new();
    for row in data {
        let target = row.try_get("target")?;
        let member = matches!(relation, Relation::Block | Relation::Member);
        let deleted = member && db::text(&row, "account_state")? == "deleted";
        // Threads in forums the member blocked (including those blocked along with their parent forum), and followed
        // forums that are themselves blocked, all get a placeholder.
        let available = member
            || (row.try_get::<i64, _>("available")? == 1
                && !(matches!(relation, Relation::Thread | Relation::Forum)
                    && filters.blocks(row.try_get("fid")?, row.try_get("parent_fid")?)));
        let withdrawn = relation == Relation::Thread
            && (row.try_get::<i64, _>("deleted_at")? > 0
                || row.try_get::<i64, _>("first_deleted_at")? > 0);
        let title = if deleted {
            crate::i18n::current().common_deleted_user().into()
        } else if !available {
            crate::i18n::current().my_unavailable().into()
        } else if withdrawn {
            crate::i18n::current().common_withdrawn_title().into()
        } else {
            db::text(&row, "title")?
        };
        let href = if !available || deleted {
            String::new()
        } else {
            match relation {
                Relation::Thread => format!("/thread-{target}"),
                Relation::Forum => format!("/{}", db::text(&row, "slug")?),
                Relation::Block | Relation::Member => crate::urls::space(target),
            }
        };
        rows.push(RelationRow {
            target,
            title,
            href,
            available,
            dateline: row.try_get("dateline")?,
        });
    }
    Ok(RelationPage { rows, total })
}

/// Follower list: members who follow this member, newest follow first (`gonof_member_follow_fans`). Only the member
/// themselves can view it.
pub async fn followers(db: &Database, actor: Actor, page: usize) -> Result<RelationPage> {
    if page == 0 || page > i64::MAX as usize / PAGE_SIZE {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().my_error_page_number(),
        ));
    }
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    let total: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_member_follow WHERE followed_uid=?",
    )
    .bind(actor.uid)
    .fetch_one(db.executor())
    .await?;
    if page > (total as usize).div_ceil(PAGE_SIZE).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    let data = crate::db::query("SELECT r.uid AS target,r.dateline,m.username AS title,m.account_state FROM gonof_home_member_follow r JOIN gonof_common_member m ON m.uid=r.uid WHERE r.followed_uid=? ORDER BY r.dateline DESC,r.uid DESC LIMIT ? OFFSET ?")
        .bind(actor.uid)
        .bind(PAGE_SIZE as i64)
        .bind(((page - 1) * PAGE_SIZE) as i64)
        .fetch_all(db.executor())
        .await?;
    let mut rows = Vec::new();
    for row in data {
        let target = row.try_get("target")?;
        let deleted = db::text(&row, "account_state")? == "deleted";
        rows.push(RelationRow {
            target,
            title: if deleted {
                crate::i18n::current().common_deleted_user().into()
            } else {
                db::text(&row, "title")?
            },
            href: if deleted {
                String::new()
            } else {
                crate::urls::space(target)
            },
            available: true,
            dateline: row.try_get("dateline")?,
        });
    }
    Ok(RelationPage { rows, total })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FollowCounts {
    pub following: i64,
    pub followers: i64,
}

/// Following and follower counts on the profile page: two index-backed counts.
pub async fn follow_counts(db: &Database, uid: i64) -> anyhow::Result<FollowCounts> {
    let following: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_home_member_follow WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await?;
    let followers: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_member_follow WHERE followed_uid=?",
    )
    .bind(uid)
    .fetch_one(db.executor())
    .await?;
    Ok(FollowCounts {
        following,
        followers,
    })
}

/// The follow feed only covers new threads from the last this-many days, so the sorted set is bounded by the
/// follow limit and the number of days.
pub const FEED_DAYS: i64 = 30;

pub struct FeedItem {
    pub tid: i64,
    pub pid: i64,
    pub dateline: i64,
    pub subject: String,
    pub author_uid: i64,
    pub author: String,
    pub forum_name: String,
    pub forum_slug: String,
    pub replies: i64,
    pub category: i64,
}

pub struct FeedPage {
    pub rows: Vec<FeedItem>,
    /// Cursor for the next page: publish time and first-post PID of the last entry on this page.
    pub next: Option<(i64, i64)>,
}

/// Follow feed: new threads posted in the last [`FEED_DAYS`] days by followed members, newest first, 20 per page.
///
/// Only members whose personal space the viewer can see are included; threads are filtered by the viewer's read
/// permissions, withdrawn ones do not appear, and neither do blocked authors or hidden or blocked forums (the same
/// rules as cross-forum aggregation and the home page).
pub async fn feed(
    db: &Database,
    actor: Actor,
    before: Option<(i64, i64)>,
    now: i64,
    filters: &super::forum_filter::ForumFilters,
    forums: &[crate::organization::ForumRecord],
) -> Result<FeedPage> {
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    // Readable forums and forum names both come from the member's own forum cache (`forums`; thread rows carry no
    // forum copies).
    let fids: Vec<i64> = forums.iter().map(|forum| forum.fid).collect();
    let readable = content::thread_visible_sql(content::Readable {
        uid: Some(actor.uid),
        forums: &fids,
    });
    let excluded = super::forum_filter::excluded_sql("t.fid", &filters.timeline_excluded());
    let blocked = content::not_blocked(
        "firstpost.authorid",
        &content::blocked_authors(db, actor.uid)
            .await
            .map_err(ContentWriteError::Internal)?,
    );
    let cursor = if before.is_some() {
        " AND (firstpost.dateline<? OR (firstpost.dateline=? AND firstpost.pid<?))"
    } else {
        ""
    };
    // Join order and index are hard-coded: first the people the member follows (≤500), then each one's first posts by
    // the author index. When the thread table is small or a forum is large, the optimizer would switch to a full scan
    // from the forum or thread side, and rows read would no longer be bounded by the number of people followed.
    // Measured on both 5.7.44 and 8.4.11 as mf ref PRIMARY → firstpost ref gonof_post_author_created →
    // t eq_ref PRIMARY, with a sorted set holding only the followed members' new threads from the last 30 days. The
    // readable scope goes by readable forum ids without joining the forum table; blocks use the list the member read,
    // with no subquery.
    let sql = format!(
        "SELECT STRAIGHT_JOIN firstpost.pid,t.tid,t.fid,firstpost.dateline,t.subject,t.authorid,t.author,t.replies,t.category
        FROM gonof_home_member_follow mf
        JOIN gonof_common_member m ON m.uid=mf.followed_uid
        JOIN gonof_forum_post firstpost FORCE INDEX (gonof_post_author_created) ON firstpost.authorid=mf.followed_uid AND firstpost.first=1
        JOIN gonof_forum_thread t FORCE INDEX (PRIMARY) ON t.tid=firstpost.tid
        WHERE mf.uid=? AND firstpost.dateline>=? AND m.space_visibility IN ('public','members')
        AND {readable} AND t.deleted_at=0 AND firstpost.deleted_at=0{excluded}{blocked}{cursor}
        ORDER BY firstpost.dateline DESC,firstpost.pid DESC LIMIT ?"
    );
    let mut query = crate::db::query(&sql)
        .bind(actor.uid)
        .bind(now - FEED_DAYS * 86400);
    if let Some((dateline, pid)) = before {
        query = query.bind(dateline).bind(dateline).bind(pid);
    }
    let data = query
        .bind(PAGE_SIZE as i64 + 1)
        .fetch_all(db.executor())
        .await?;
    let more = data.len() > PAGE_SIZE;
    let mut rows = Vec::new();
    for row in data.into_iter().take(PAGE_SIZE) {
        rows.push(FeedItem {
            tid: row.try_get("tid")?,
            pid: row.try_get("pid")?,
            dateline: row.try_get("dateline")?,
            subject: db::text(&row, "subject")?,
            author_uid: row.try_get::<Option<i64>, _>("authorid")?.unwrap_or(0),
            author: db::text(&row, "author")?,
            forum_name: content::forum_name(forums, row.try_get("fid")?),
            forum_slug: content::forum_slug(forums, row.try_get("fid")?),
            replies: row.try_get("replies")?,
            category: row.try_get("category")?,
        });
    }
    let next = if more {
        rows.last().map(|item| (item.dateline, item.pid))
    } else {
        None
    };
    Ok(FeedPage { rows, next })
}
