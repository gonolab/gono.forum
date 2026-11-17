//! The statements behind the thread-shaped data sources (docs/data.md; docs/queries.md, "Data sources"). They
//! follow the list rules: one single-table statement on a list index, the visitor's boards and blocks written into
//! it as in [`super::ListViewer::filter`], capped, and no total.
use anyhow::Result;

use super::{
    Arg, ListQuery, ListScope, ListViewer, Thread, bind_rows, decode_list_thread, hot_topics,
    list_revision, list_rows, list_select, refresh_thread_times, thread_images,
};
use crate::db::Database;
use crate::urls::Sort;

/// How a `threads` source sorts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum SourceSort {
    Latest,
    Created,
    /// Threads with a reply since `since`, by reply count (`gonof_thread_global_active`).
    Hot {
        since: i64,
    },
    Featured,
    Pinned,
}

/// One `threads` request.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct SourceList<'a> {
    pub scope: &'a ListScope,
    pub sort: SourceSort,
    /// Category 1 to 5; only with `Latest` and `Created`.
    pub category: Option<i64>,
    pub limit: i64,
    /// Read the list thumbnails too (one more statement; only when the site shows thumbnails).
    pub thumbnails: bool,
}

/// The first `limit` threads of a source list. Anonymous lists go through the Redis cache when it is on, keyed by
/// the list version (and for hot threads a list-lifetime segment, since their window moves); members always read
/// the database, like list pages.
pub async fn threads(
    db: &Database,
    cache: &crate::cache::TimedCache,
    list: SourceList<'_>,
    viewer: ListViewer<'_>,
    now: i64,
) -> Result<Vec<Thread>> {
    if viewer.uid.is_some() || !cache.cache().enabled() {
        return read(db, list, &viewer).await;
    }
    let (revision, watermark) = list_revision(db).await?;
    let segment = match list.sort {
        SourceSort::Hot { .. } => now.div_euclid(
            i64::try_from(cache.lifetimes().list)
                .unwrap_or(i64::MAX)
                .max(1),
        ),
        _ => 0,
    };
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::DATA_THREADS,
        revision,
        &(
            watermark,
            segment,
            crate::clock::current(),
            list,
            viewer.forums,
        ),
    )?;
    let mut threads = cache
        .get_or_fill_json(&key, || read(db, list, &viewer))
        .await?;
    refresh_thread_times(&mut threads)?;
    Ok(threads)
}

async fn read(db: &Database, list: SourceList<'_>, viewer: &ListViewer<'_>) -> Result<Vec<Thread>> {
    if viewer.sees_nothing(list.scope) || list.limit <= 0 {
        return Ok(Vec::new());
    }
    let mut threads = match list.sort {
        SourceSort::Hot { since } => hot_topics(db, viewer, list.scope, list.limit, since).await?,
        sort => {
            let sort = match sort {
                SourceSort::Created => Sort::Created,
                SourceSort::Featured => Sort::Featured,
                SourceSort::Pinned => Sort::Pinned,
                _ => Sort::Latest,
            };
            let mut args = Vec::new();
            let mut filter = String::from(list.scope.condition(&mut args));
            filter.push_str(if sort == Sort::Pinned {
                " AND t.displayorder > 0"
            } else {
                " AND t.displayorder >= 0"
            });
            if sort == Sort::Featured {
                filter.push_str(" AND t.digest = 1");
            }
            let category = list
                .category
                .filter(|_| matches!(sort, Sort::Latest | Sort::Created));
            if let Some(category) = category {
                filter.push_str(" AND t.category = ?");
                args.push(Arg::Int(category));
            }
            filter.push_str(&viewer.filter(list.scope));
            let query = ListQuery {
                scope: list.scope,
                sort,
                category: category.is_some(),
            };
            list_rows(
                db,
                &query,
                (&filter, &args),
                usize::try_from(list.limit).unwrap_or(0),
                0,
            )
            .await?
        }
    };
    if list.thumbnails {
        thread_images::attach(db, &mut threads).await?;
    }
    Ok(threads)
}

/// The latest listed thread of each board (docs/data.md, `boards`, `last_post`), read without a new index: for a
/// top-level board on `gonof_thread_family_latest` (its own threads and its child boards'), for a child board on
/// `gonof_thread_forum_latest`, one statement per pin level 3 to 0, each an index seek to the newest row of that
/// level; the newest of the four wins in code, as the feed merges its two statements. `fids` are the boards this
/// view may read (written as `t.fid IN (…)`, evaluated in the index); `public` adds `t.visibility = 'public'`.
pub async fn board_last_posts(
    db: &Database,
    boards: &[(i64, bool)],
    fids: &[i64],
    public: bool,
) -> Result<Vec<(i64, Thread)>> {
    if fids.is_empty() {
        return Ok(Vec::new());
    }
    let readable = fids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut found = Vec::new();
    for &(fid, top) in boards {
        let (index, column) = if top {
            ("gonof_thread_family_latest", "family_fid")
        } else {
            ("gonof_thread_forum_latest", "fid")
        };
        let sql = format!(
            "{} WHERE t.{column} = ? AND t.displayorder = ?{} AND t.fid IN ({readable}) ORDER BY t.lastpost DESC,t.tid DESC LIMIT 1",
            list_select(index),
            if public {
                " AND t.visibility = 'public'"
            } else {
                ""
            },
        );
        let mut newest: Option<Thread> = None;
        for level in [3, 2, 1, 0] {
            let args = [Arg::Int(fid), Arg::Int(level)];
            let row = bind_rows(crate::db::query(&sql), &args)
                .fetch_optional(db.executor())
                .await?;
            if let Some(row) = row {
                let thread = decode_list_thread(row)?;
                if newest
                    .as_ref()
                    .is_none_or(|seen| (thread.updated, thread.id) > (seen.updated, seen.id))
                {
                    newest = Some(thread);
                }
            }
        }
        if let Some(thread) = newest {
            found.push((fid, thread));
        }
    }
    Ok(found)
}

/// The head of a thread the visitor may read, for `thread.outline` outside its own page: `(maxposition,
/// cache_revision)`, by primary key under [`super::thread_visible_sql`].
pub async fn outline_head(
    db: &Database,
    tid: i64,
    readable: super::Readable<'_>,
) -> Result<Option<(i64, i64)>> {
    let sql = format!(
        "SELECT t.maxposition,t.cache_revision FROM gonof_forum_thread t WHERE t.tid = ? AND {}",
        super::thread_visible_sql(readable)
    );
    let row = crate::db::query(&sql)
        .bind(tid)
        .fetch_optional(db.executor())
        .await?;
    row.map(|row| Ok((row.try_get("maxposition")?, row.try_get("cache_revision")?)))
        .transpose()
}

/// Floors read in full up to this many; past it the outline samples.
pub const OUTLINE_FULL: i64 = 5_000;

/// The floors behind a thread's outline: `(position, dateline)` in floor order, on the post primary key
/// `(tid, position)`. Hidden and withdrawn floors are included: they keep their number, and the page shows their
/// placeholders at their times. Up to [`OUTLINE_FULL`] floors every one is read; past it the positions in
/// `samples`, so the read stays bounded.
pub async fn outline_floors(
    db: &Database,
    tid: i64,
    max_position: i64,
    samples: &[i64],
) -> Result<Vec<(i64, i64)>> {
    let rows = if max_position <= OUTLINE_FULL {
        crate::db::query(
            "SELECT position,dateline FROM gonof_forum_post FORCE INDEX (PRIMARY) WHERE tid = ? AND position <= ? ORDER BY position",
        )
        .bind(tid)
        .bind(max_position)
        .fetch_all(db.executor())
        .await?
    } else {
        let positions = samples
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        crate::db::query(&format!(
            "SELECT position,dateline FROM gonof_forum_post FORCE INDEX (PRIMARY) WHERE tid = ? AND position IN ({positions}) ORDER BY position"
        ))
        .bind(tid)
        .fetch_all(db.executor())
        .await?
    };
    rows.into_iter()
        .map(|row| Ok((row.try_get("position")?, row.try_get("dateline")?)))
        .collect()
}
