//! Entries of public feeds: recently published public threads, site-wide or for a single forum.
//!
//! Only threads that anonymous visitors can see are included: the caller supplies the forums from the forum cache
//! (enabled and public, and so are their parents), and the thread is public with `displayorder >= 0` (a first post
//! hidden by moderation, unpublished, or with a read permission requirement is negative). Threads
//! withdrawn by their author are also excluded; erasure always comes with withdrawal (the only erase statement writes
//! both `deleted_at` and `erased_at`, and a source scan pins `erased_at` to being written only there), so
//! `erased_at` need not be checked separately.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::ListScope;
use crate::db::{Database, DbRow, text};

// The number of entries per feed is the admin setting `settings.feed.items` (10–100,
// "Global settings › Search & performance"); the handler passes it in and it is encoded into the feed cache key
// (docs/settings-and-caching.md, "The Redis cache").

/// Summary length, the same as the thread page's share description.
pub const SUMMARY_CHARS: usize = 160;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedItem {
    pub tid: i64,
    pub title: String,
    /// Deleted authors are shown as "Deleted user".
    pub author: String,
    /// The forum: the forum name and URL are not cached; the feed route takes them from the forum cache by `fid`
    /// (the thread row carries no forum copies).
    pub fid: i64,
    #[serde(skip)]
    pub forum_name: String,
    #[serde(skip)]
    pub forum_slug: String,
    pub published: i64,
    /// The later of the publish time and the first post's last edit time.
    pub updated: i64,
    /// The first [`SUMMARY_CHARS`] characters of the rendered first post as plain text, without spoilers,
    /// attachments, quotes or mention syntax.
    pub summary: String,
}

/// Pin levels only range from 0 to 3 (the pin action in `content/edits.rs`); negative means not listed (migration
/// 0009). Sorting by publish time descending across pin levels, with `displayorder` ahead of `dateline` in the
/// `*_created` indexes, would need a range condition and a full filesort. So it is split into two segments: normal
/// threads `= 0` take the first N in index order; pinned threads `> 0` are only a handful, and only they get
/// filesorted. The code then merges the two segments and takes the first N (two queries, not a UNION).
///
/// `segment` is the scope's index and forum condition ([`segment`]); `filter` holds the extra conditions on readable
/// forums, thread visibility and blocking. The author comes from the copy on the thread row, written as "Deleted user"
/// on account deletion, without joining the member table; the first post is only for the body and edit time.
fn sql(segment: &Segment, pinned: bool, filter: &str, limit: usize) -> String {
    let (index, forum) = segment;
    let scope = forum
        .map(|(column, _)| format!("{column} = ? AND "))
        .unwrap_or_default();
    let order = if pinned {
        "t.displayorder > 0"
    } else {
        "t.displayorder = 0"
    };
    format!(
        "SELECT t.tid, t.fid, t.subject, t.dateline, t.author, firstpost.message, firstpost.edited_at
        FROM gonof_forum_thread t FORCE INDEX ({index})
        JOIN gonof_forum_post firstpost ON firstpost.first_tid = t.tid
        WHERE {scope}{order} AND t.deleted_at = 0 AND firstpost.deleted_at = 0{filter}
        ORDER BY t.dateline DESC, t.tid DESC LIMIT {limit}"
    )
}

/// The scope's index and forum condition (column and forum id).
type Segment = (&'static str, Option<(&'static str, i64)>);

/// Feed scope: site-wide uses `gonof_thread_global_created`; a top-level forum's feed includes its child forums
/// and uses `gonof_thread_family_created` by `family_fid`; a child forum uses
/// `gonof_thread_forum_created` by `fid`.
fn segment(scope: &ListScope) -> Segment {
    match scope {
        ListScope::All => ("gonof_thread_global_created", None),
        ListScope::Family(fid) => ("gonof_thread_family_created", Some(("t.family_fid", *fid))),
        ListScope::Forum(fid) => ("gonof_thread_forum_created", Some(("t.fid", *fid))),
    }
}

/// Readable forums and visibility: `forums` are the forums the reader can see (as for lists); with `public` only public
/// threads are included.
fn filter(forums: &[i64], public: bool) -> String {
    let mut sql = format!(
        " AND t.fid IN ({})",
        forums
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    if public {
        sql.push_str(" AND t.visibility = 'public'");
    }
    sql
}

/// Runs each of the two segments (normal, pinned) once, decodes, merges and takes the first `limit`.
async fn fetch(
    db: &Database,
    scope: &ListScope,
    filter: &str,
    limit: usize,
) -> Result<Vec<FeedItem>> {
    let rows = segments(db, scope, filter, limit, || async {}).await?;
    finish(db, merge(rows, limit)).await
}

/// The two segments, read in one read-only snapshot: a thread pinned or unpinned between the two statements is then
/// in exactly one of them, as of the snapshot, instead of in both or in neither. `between` runs after the first
/// statement (a test changes a pin there).
async fn segments<F, Fut>(
    db: &Database,
    scope: &ListScope,
    filter: &str,
    limit: usize,
    between: F,
) -> Result<Vec<Row>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let segment = segment(scope);
    let mut tx = db.begin_read_only().await?;
    let mut rows = Vec::new();
    let mut between = Some(between);
    for pinned in [false, true] {
        let sql = sql(&segment, pinned, filter, limit);
        let mut query = crate::db::query(&sql);
        if let Some((_, fid)) = segment.1 {
            query = query.bind(fid);
        }
        for row in query.fetch_all(db.observe(&mut *tx)).await? {
            rows.push(decode(row)?);
        }
        if let Some(between) = between.take() {
            between().await;
        }
    }
    tx.commit().await?;
    Ok(rows)
}

struct Row {
    tid: i64,
    fid: i64,
    title: String,
    author: String,
    published: i64,
    edited_at: i64,
    message: String,
}

fn decode(row: DbRow) -> Result<Row> {
    Ok(Row {
        tid: row.try_get("tid")?,
        fid: row.try_get("fid")?,
        title: row.try_get("subject")?,
        author: row.try_get("author")?,
        published: row.try_get("dateline")?,
        edited_at: row.try_get("edited_at")?,
        message: text(&row, "message")?,
    })
}

/// Both segments are already sorted by publish time descending; after merging, take the first `limit`. A thread is
/// kept once even if both segments returned it, so it cannot push another one out.
fn merge(mut rows: Vec<Row>, limit: usize) -> Vec<Row> {
    rows.sort_by_key(|row| std::cmp::Reverse((row.published, row.tid)));
    rows.dedup_by_key(|row| row.tid);
    rows.truncate(limit);
    rows
}

/// `scope` is the scope (site-wide, a top-level forum with its child forums, a child forum); `forums` are the forums
/// anonymous visitors can read within this site's scope. The caller first confirms the forum is visible to anonymous
/// visitors; the conditions here also only let public threads through.
pub async fn items(
    db: &Database,
    scope: &ListScope,
    forums: &[i64],
    limit: usize,
) -> Result<Vec<FeedItem>> {
    if forums.is_empty() {
        return Ok(Vec::new());
    }
    fetch(db, scope, &filter(forums, true), limit).await
}

/// The reader of a personal feed: the token owner and their filters.
pub struct MemberScope<'a> {
    pub uid: i64,
    /// The owner turned on "Include members-only threads".
    pub include_members: bool,
    /// Forums the owner can see: readable from the owner's point of view and within this site's scope, minus hidden
    /// and blocked forums (for a single-forum feed, the ones in its family that are not blocked).
    pub forums: &'a [i64],
    /// Members the owner has blocked.
    pub blocked_uids: &'a [i64],
}

/// Entries of a personal feed: the same two segments (normal, pinned) and count as the public feed, additionally
/// removing members the owner blocked and excluded forums; when member threads are included, visibility is no longer
/// filtered. Not put in the shared cache.
pub async fn member_items(
    db: &Database,
    scope: &ListScope,
    member: &MemberScope<'_>,
    limit: usize,
) -> Result<Vec<FeedItem>> {
    if member.forums.is_empty() {
        return Ok(Vec::new());
    }
    let mut filter = filter(member.forums, !member.include_members);
    if !member.blocked_uids.is_empty() {
        filter.push_str(&format!(
            " AND (t.authorid IS NULL OR t.authorid NOT IN ({}))",
            member
                .blocked_uids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    fetch(db, scope, &filter, limit).await
}

async fn finish(db: &Database, rows: Vec<Row>) -> Result<Vec<FeedItem>> {
    let sources: Vec<&str> = rows.iter().map(|row| row.message.as_str()).collect();
    let names = crate::markdown::member_names(db, &sources)
        .await
        .context("failed to read the members mentioned in feed summaries")?;
    Ok(rows
        .into_iter()
        .map(|row| FeedItem {
            summary: crate::markdown::excerpt(&row.message, &names, SUMMARY_CHARS),
            updated: row.published.max(row.edited_at),
            tid: row.tid,
            title: row.title,
            // The marker of a deleted author, in the feed's language (the cache key holds it).
            author: super::copies::shown_name(row.author),
            fid: row.fid,
            forum_name: String::new(),
            forum_slug: String::new(),
            published: row.published,
        })
        .collect())
}

/// The feed is anonymous public content, so entries are cached by list version (site-wide version plus the new-post
/// watermark); settings such as the site name and URL are not cached and are assembled each time. With Redis
/// disabled, `get_or_fill_json` goes straight to the source.
pub async fn cached_items(
    db: &Database,
    cache: &crate::cache::TimedCache,
    scope: &ListScope,
    forums: &[i64],
    limit: usize,
) -> Result<Vec<FeedItem>> {
    if !cache.cache().enabled() {
        return items(db, scope, forums, limit).await;
    }
    let (revision, watermark) = super::list_revision(db).await?;
    // The entry count is encoded into the key: changing `settings.feed.items` naturally changes the key, and old
    // entries expire with their lifetime.
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::FEED_ITEMS,
        revision,
        &(watermark, scope, forums, limit, crate::i18n::current()),
    )?;
    cache
        .get_or_fill_json(&key, || items(db, scope, forums, limit))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tid: i64, published: i64) -> Row {
        Row {
            tid,
            fid: 1,
            title: String::new(),
            author: String::new(),
            published,
            edited_at: 0,
            message: String::new(),
        }
    }

    #[test]
    fn merge_orders_both_segments_by_publication_and_keeps_the_limit() {
        // 30 normal entries (times 100..=129), 2 pinned: one newest, one oldest.
        let mut rows: Vec<Row> = (0..30).map(|n| row(1000 + n, 100 + n)).collect();
        rows.push(row(1, 500));
        rows.push(row(2, 1));
        let merged = merge(rows, 30);
        assert_eq!(merged.len(), 30);
        assert_eq!(
            merged[0].tid, 1,
            "the newest pinned thread comes first by publication time"
        );
        assert_eq!(merged[1].tid, 1029);
        assert!(
            merged.iter().all(|r| r.tid != 2),
            "the oldest pinned thread is pushed out"
        );
        assert_eq!(merged.last().unwrap().tid, 1001);
        // Same time: by tid descending.
        let tie = merge(vec![row(5, 7), row(9, 7)], 30);
        assert_eq!(tie.iter().map(|r| r.tid).collect::<Vec<_>>(), [9, 5]);
    }

    #[test]
    fn merge_keeps_a_thread_both_segments_returned_once() {
        let rows = vec![row(3, 30), row(2, 20), row(1, 10), row(3, 30)];
        let merged = merge(rows, 3);
        assert_eq!(merged.iter().map(|r| r.tid).collect::<Vec<_>>(), [3, 2, 1]);
    }

    /// A thread unpinned between the two statements is still in the feed (as the pinned thread the snapshot saw),
    /// and one pinned between them is in it once.
    #[tokio::test]
    async fn both_segments_read_one_snapshot() {
        let fixture = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &fixture.db;
        super::super::seed_fixture(db).await.unwrap();
        let forums = super::super::anonymous_forums(db).await.unwrap();
        let filter = filter(&forums, true);
        let tids = |rows: &[Row]| rows.iter().map(|row| row.tid).collect::<Vec<_>>();
        let listed = tids(
            &segments(db, &ListScope::All, &filter, 100, || async {})
                .await
                .unwrap(),
        );
        let (unpinned, pinned) = (listed[0], listed[1]);
        let set = |tid: i64, order: i64| async move {
            crate::db::query("UPDATE gonof_forum_thread SET displayorder=? WHERE tid=?")
                .bind(order)
                .bind(tid)
                .execute(db.executor())
                .await
                .unwrap();
        };
        set(unpinned, 1).await;
        let rows = segments(db, &ListScope::All, &filter, 100, || async {
            set(unpinned, 0).await;
            set(pinned, 1).await;
        })
        .await
        .unwrap();
        let seen = tids(&rows);
        for tid in [unpinned, pinned] {
            assert_eq!(
                seen.iter().filter(|seen| **seen == tid).count(),
                1,
                "{tid} in {seen:?}"
            );
        }
        fixture.cleanup().await.unwrap();
    }

    #[test]
    fn queries_stay_on_the_created_indexes() {
        for (scope, index, column) in [
            (ListScope::All, "gonof_thread_global_created", None),
            (
                ListScope::Family(3),
                "gonof_thread_family_created",
                Some("t.family_fid = ?"),
            ),
            (
                ListScope::Forum(4),
                "gonof_thread_forum_created",
                Some("t.fid = ?"),
            ),
        ] {
            for pinned in [false, true] {
                let sql = sql(&segment(&scope), pinned, &filter(&[1, 2], true), 30);
                assert!(sql.contains(&format!("FORCE INDEX ({index})")));
                assert_eq!(column.is_some_and(|c| sql.contains(c)), column.is_some());
                assert!(sql.contains("t.fid IN (1,2) AND t.visibility = 'public'"));
                assert!(
                    !sql.contains("gonof_common_member"),
                    "no join with the member table"
                );
                assert!(sql.contains("LIMIT 30"));
            }
        }
    }
}
