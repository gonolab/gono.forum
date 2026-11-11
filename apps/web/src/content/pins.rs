//! Cross-forum level 2 and level 3 pins (docs/queries.md, "Sidebars, the pinned block and feeds").
//!
//! - Level 1 pins stay in their own forum; **level 2 pins show on the forum pages of the same parent forum** (the
//!   parent forum and all its child forums); **level 3 pins show on every forum page of the whole site**. The home
//!   page stays as it is (the list already puts threads first by pin level), and search results do not merge them.
//! - Shown as a separate block above the list only on page 1 of a forum page; not counted in the total and not
//!   taking up per-page slots, so neither the list SQL nor the count SQL changes.
//! - Threads the page's own list already contains do not enter the block (a forum page leaves out its own
//!   forum's); inside the block they are ordered by `displayorder DESC, lastpost DESC, tid DESC`, with no limit on
//!   the number of entries.
//!
//! The site-wide pin index (holding only `tid, fid, displayorder`) lives in the process cache
//! ([`crate::cache::Block::Pins`]) and is used to pick the applicable thread ids; **an empty set sends no query**,
//! otherwise the rows are fetched once by primary key, and the readable scope, the thread status, the members and
//! forums the viewer has blocked, and the forums the viewer has hidden are all re-checked in that SQL; the pin
//! level and the forum are also re-judged against the current values on the row. So a stale index can only make a
//! new pin appear late; it cannot expose a thread whose pin was removed, or that was hidden or moved away.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{ListScope, ListViewer, Thread, decode_list_thread, list_select};
use crate::db::Database;

/// One entry of the site-wide pin index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Pin {
    pub tid: i64,
    pub fid: i64,
    /// Pin level: 2 or 3.
    pub level: i64,
}

/// Loads the index of site-wide level 2 and level 3 pins: `gonof_thread_global_latest` starts with `displayorder`,
/// so only the index rows of pinned threads are read, without going back to the table.
/// The visibility of forums and threads is judged again for the viewer when the rows are fetched.
pub async fn index(db: &Database) -> Result<Vec<Pin>> {
    crate::db::query(
        "SELECT t.tid,t.fid,t.displayorder FROM gonof_forum_thread t FORCE INDEX (gonof_thread_global_latest)
         WHERE t.displayorder IN (2,3)",
    )
    .fetch_all(db.executor())
    .await?
    .into_iter()
    .map(|row| {
        Ok(Pin {
            tid: row.try_get("tid")?,
            fid: row.try_get("fid")?,
            level: row.try_get("displayorder")?,
        })
    })
    .collect()
}

/// The scope merged into this page: `home` is the family (the parent forum and all its child forums; a child
/// forum page takes its parent's family), `listed` is the forums the page's own list already covers (on a forum
/// page, that forum itself).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Scope {
    pub home: Vec<i64>,
    pub listed: Vec<i64>,
}

impl Scope {
    /// Computes the scope of the page for `fid` from the forum list (`(fid, parent_fid)`). Forums have only two
    /// levels. `listed` is the forums the page's list covers: a child forum page is just itself, a parent forum
    /// page is itself and its child forums.
    pub fn for_forum(
        fid: i64,
        parent: Option<i64>,
        forums: &[(i64, Option<i64>)],
        listed: &[i64],
    ) -> Self {
        let root = parent.unwrap_or(fid);
        let mut home: Vec<i64> = std::iter::once(root)
            .chain(
                forums
                    .iter()
                    .filter(|(_, parent)| *parent == Some(root))
                    .map(|(child, _)| *child),
            )
            .collect();
        home.sort_unstable();
        home.dedup();
        let mut listed = listed.to_vec();
        if listed.is_empty() {
            listed.push(fid);
        }
        listed.sort_unstable();
        Self { home, listed }
    }

    /// Picks the applicable thread ids from the index: all of level 3, and the level 2 ones whose forum belongs to
    /// the family; drops those the page's list already has. Sorted by tid ascending.
    pub fn applicable(&self, index: &[Pin]) -> Vec<i64> {
        let mut tids: Vec<i64> = index
            .iter()
            .filter(|pin| !self.listed.contains(&pin.fid))
            .filter(|pin| pin.level == 3 || (pin.level == 2 && self.home.contains(&pin.fid)))
            .map(|pin| pin.tid)
            .collect();
        tids.sort_unstable();
        tids.dedup();
        tids
    }
}

/// Fetches the rows of the pin block by primary key and re-checks in the same SQL: the pin level and the forum (any
/// forum for level 3, the family for level 2), not in a forum the page's list already covers, forums this viewer
/// can see and thread visibility, members the viewer has blocked (`viewer`). Sends no SQL when `tids` is empty.
pub async fn rows(
    db: &Database,
    tids: &[i64],
    scope: &Scope,
    viewer: ListViewer<'_>,
) -> Result<Vec<Thread>> {
    if tids.is_empty() || scope.home.is_empty() || viewer.forums.is_empty() {
        return Ok(Vec::new());
    }
    let list = |ids: &[i64]| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let sql = format!(
        "{} WHERE t.tid IN ({}) AND (t.displayorder = 3 OR (t.displayorder = 2 AND t.fid IN ({}))) AND t.fid NOT IN ({}){} ORDER BY t.displayorder DESC,t.lastpost DESC,t.tid DESC",
        list_select("PRIMARY"),
        list(tids),
        list(&scope.home),
        list(&scope.listed),
        viewer.filter(&ListScope::All),
    );
    crate::db::query(&sql)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(decode_list_thread)
        .collect()
}

/// For anonymous visitors the pin block goes into Redis together with the anonymous list: pinning, unpinning,
/// hiding, moving and changing a forum's scope all bump the site-wide content version (`copies::bump_site`), and
/// the key carries the list version, so no new invalidation point is needed. When Redis
/// is off it queries directly, without an extra database read to get the version.
pub async fn cached_rows(
    db: &Database,
    cache: &crate::cache::TimedCache,
    tids: &[i64],
    scope: &Scope,
    forums: &[i64],
) -> Result<Vec<Thread>> {
    if tids.is_empty() {
        return Ok(Vec::new());
    }
    let anonymous = ListViewer {
        forums,
        ..ListViewer::default()
    };
    if !cache.cache().enabled() {
        return rows(db, tids, scope, anonymous).await;
    }
    let (revision, watermark) = super::list_revision(db).await?;
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::FORUM_PINS,
        revision,
        &(
            watermark,
            tids,
            scope,
            forums,
            // The cached value contains times already formatted for the site time zone and time style.
            crate::clock::current(),
        ),
    )?;
    cache
        .get_or_fill_json(&key, || rows(db, tids, scope, anonymous))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_follow_the_forum_family_and_skip_what_the_list_already_has() {
        // 1 is a parent forum, 2 and 3 are its child forums; 4 is another top-level forum, 5 is a child forum of 4.
        let forums = [
            (1, None),
            (2, Some(1)),
            (3, Some(1)),
            (4, None),
            (5, Some(4)),
        ];
        let index = [
            Pin {
                tid: 10,
                fid: 2,
                level: 2,
            },
            Pin {
                tid: 11,
                fid: 5,
                level: 2,
            },
            Pin {
                tid: 12,
                fid: 4,
                level: 3,
            },
            Pin {
                tid: 13,
                fid: 1,
                level: 3,
            },
            Pin {
                tid: 14,
                fid: 1,
                level: 2,
            },
        ];
        // The list of a parent forum page rolls up its child forums: the family's pins are all in the list
        // already, so only other families' level 3 pins are left in the block.
        let parent = Scope::for_forum(1, None, &forums, &[1, 2, 3]);
        assert_eq!(parent.home, vec![1, 2, 3]);
        assert_eq!(parent.listed, vec![1, 2, 3]);
        assert_eq!(parent.applicable(&index), vec![12]);
        // When the list covers only the parent forum itself (no roll-up scope passed), level 2 pins of child forums
        // enter the block, while other families' level 2 pins (11) do not.
        let alone = Scope::for_forum(1, None, &forums, &[]);
        assert_eq!(alone.listed, vec![1]);
        assert_eq!(alone.applicable(&index), vec![10, 12]);
        let child = Scope::for_forum(3, Some(1), &forums, &[3]);
        assert_eq!(child.home, vec![1, 2, 3]);
        // Child forum page: all level 2 pins of the same family enter (the parent forum's 14, the sibling's 10), and
        // level 3 pins from the whole site enter.
        assert_eq!(child.applicable(&index), vec![10, 12, 13, 14]);
        let other = Scope::for_forum(5, Some(4), &forums, &[5]);
        assert_eq!(
            other.applicable(&index),
            vec![12, 13],
            "this forum's level 2 pins are in the list"
        );
        assert!(
            Scope::for_forum(4, None, &forums, &[4, 5])
                .applicable(&[])
                .is_empty()
        );
    }
}
