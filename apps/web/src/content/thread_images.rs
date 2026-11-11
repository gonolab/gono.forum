//! Real thumbnails for thread lists.
//!
//! - For each thread, `gonof_forum_thread_image` stores the first [`MAX_THREAD_IMAGES`] static images of the first
//!   post body in order of appearance: only image syntax counts, not images inside spoilers
//!   ([`crate::markdown::image_attachments`]); each must be bound to the first post, processed, and JPEG or PNG (a GIF
//!   gets no thumbnail and the next image is taken instead).
//! - Posting a thread and editing the first post rewrite the whole set in the same write transaction ([`rewrite`]);
//!   the attachment rows are already locked by the binding. Editing the first post advances the site-wide content
//!   version, so the anonymous list cache changes key with it. There is no backfill: a thread gets its rows only
//!   through these two write paths.
//! - A list fetches everything for the thread ids on the page in one `IN` query ([`attach`]), including the attachment
//!   width and height for page layout; how many are shown is cut at render time by the display setting
//!   `settings.display.list_thumbnails`, and the cache always holds the full 5.
//! - When the first post is withdrawn, hidden or erased the rows stay; reads filter by thread and first post state
//!   (not shown in lists, `/thumb` returns 404).
use crate::db::{self, Database, Tx};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The most images stored per thread (the upper bound of the count setting).
pub const MAX_THREAD_IMAGES: usize = 5;

/// One thumbnail in a list: attachment id and original width and height (for layout, scaled proportionally to
/// 90 px high).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Thumbnail {
    pub aid: i64,
    pub width: i64,
    pub height: i64,
}

/// Attachments that can be thumbnails: ready, passed the check, static images (a GIF may be animated, so it gets no
/// thumbnail).
pub fn thumbnail_type(mime_type: &str) -> bool {
    matches!(mime_type, "image/jpeg" | "image/png")
}

/// Attachments in the first post body that can be thumbnails, in order of appearance, at most [`MAX_THREAD_IMAGES`].
/// `pid` is the first post: only attachments bound to it are taken.
async fn candidates<'c, E>(executor: E, pid: i64, body: &str) -> Result<Vec<i64>>
where
    E: db::Executor<'c>,
{
    let ordered = crate::markdown::image_attachments(body);
    if ordered.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT d.aid,d.mime_type,d.lifecycle,d.scan_status FROM gonof_forum_attachment_reference r JOIN gonof_forum_attachment_0 d ON d.aid=r.aid WHERE r.pid=? AND r.aid IN ({})",
        vec!["?"; ordered.len()].join(",")
    );
    let mut query = db::query(&sql).bind(pid);
    for aid in &ordered {
        query = query.bind(*aid);
    }
    let rows = query.fetch_all(executor).await?;
    let mut usable = std::collections::HashSet::new();
    for row in rows {
        if db::text(&row, "lifecycle")? == "ready"
            && db::text(&row, "scan_status")? == "clean"
            && thumbnail_type(&db::text(&row, "mime_type")?)
        {
            usable.insert(row.try_get::<i64, _>("aid")?);
        }
    }
    Ok(ordered
        .into_iter()
        .filter(|aid| usable.contains(aid))
        .take(MAX_THREAD_IMAGES)
        .collect())
}

/// Rewrites the whole thumbnail set of a thread from the first post body. Called in the write transaction of posting
/// a thread or editing the first post, after the first post's attachment references are written.
pub async fn rewrite(
    database: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    pid: i64,
    body: &str,
) -> Result<()> {
    let images = candidates(database.observe(&mut **tx), pid, body).await?;
    db::query("DELETE FROM gonof_forum_thread_image WHERE tid=?")
        .bind(tid)
        .execute(database.observe(&mut **tx))
        .await?;
    for (position, aid) in images.into_iter().enumerate() {
        db::query("INSERT INTO gonof_forum_thread_image(tid,position,aid) VALUES(?,?,?)")
            .bind(tid)
            .bind(i64::try_from(position)?)
            .bind(aid)
            .execute(database.observe(&mut **tx))
            .await?;
    }
    Ok(())
}

/// Adds thumbnails to a page of threads: one `IN` query fetches them all by thread id and position, with the
/// original width and height. Withdrawn threads get none (the list only shows "Thread withdrawn").
pub async fn attach(database: &Database, threads: &mut [super::Thread]) -> Result<()> {
    let tids: Vec<i64> = threads
        .iter()
        .filter(|thread| !thread.withdrawn && !thread.erased)
        .map(|thread| i64::try_from(thread.id))
        .collect::<std::result::Result<_, _>>()?;
    if tids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "SELECT ti.tid,ti.aid,d.width,d.height FROM gonof_forum_thread_image ti JOIN gonof_forum_attachment_0 d ON d.aid=ti.aid WHERE ti.tid IN ({}) ORDER BY ti.tid,ti.position",
        vec!["?"; tids.len()].join(",")
    );
    let mut query = db::query(&sql);
    for tid in &tids {
        query = query.bind(*tid);
    }
    let rows = query.fetch_all(database.executor()).await?;
    for row in rows {
        let tid: i64 = row.try_get("tid")?;
        let (Some(width), Some(height)) = (
            row.try_get::<Option<i64>, _>("width")?,
            row.try_get::<Option<i64>, _>("height")?,
        ) else {
            continue;
        };
        if let Some(thread) = threads
            .iter_mut()
            .find(|thread| i64::try_from(thread.id).ok() == Some(tid))
        {
            thread.thumbnails.push(Thumbnail {
                aid: row.try_get("aid")?,
                width,
                height,
            });
        }
    }
    Ok(())
}
