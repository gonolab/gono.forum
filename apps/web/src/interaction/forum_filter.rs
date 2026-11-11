//! Personal forum filters: hiding and blocking (`gonof_home_forum_filter`).
//!
//! Hiding only takes effect where content is aggregated across forums (home page, sidebar, search, the site-wide part
//! of the personal feed); entering the forum and opening threads work as usual. Blocking additionally stops direct
//! access, personal spaces, favorites and follow placeholders (followed threads and followed forums), notifications,
//! the posting forum picker, and the child forums aggregated on the parent forum page.
//! A parent forum's setting carries over to all its child forums (including ones created later); a child forum takes
//! the stronger level of parent and child.
//!
//! Only affects what the member sees; the anonymous view, the shared cache and other people's views are unaffected.
use crate::db::Tx;
use std::collections::BTreeMap;

use super::*;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum FilterMode {
    Hide,
    Block,
}

impl FilterMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Block => "block",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "hide" => Some(Self::Hide),
            "block" => Some(Self::Block),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hide => crate::i18n::current().social_filter_mode_hide(),
            Self::Block => crate::i18n::current().social_filter_mode_block(),
        }
    }
}

/// One member's forum filter settings, expanded with parent-child carry-over over the forum tree readable in this
/// request.
#[derive(Clone, Debug, Default)]
pub struct ForumFilters {
    direct: BTreeMap<i64, FilterMode>,
    effective: BTreeMap<i64, FilterMode>,
    /// When the effective level comes from the parent forum (the parent's setting is stronger than its own), records
    /// the parent forum.
    inherited: BTreeMap<i64, i64>,
    /// Multi-domain sites: forums outside this site's scope that this visitor can read. Like blocking they enter no
    /// aggregation, but they are not the member's own setting, so [`ForumFilters::is_blocked`] and the settings page do
    /// not count them. Anonymous visitors have them too.
    scope_excluded: Vec<i64>,
}

impl ForumFilters {
    /// `tree` is (fid, parent_fid); forums not in the tree (which the member cannot read) keep their own setting as is.
    pub fn new(
        direct: BTreeMap<i64, FilterMode>,
        tree: impl IntoIterator<Item = (i64, Option<i64>)>,
    ) -> Self {
        let mut effective = direct.clone();
        let mut inherited = BTreeMap::new();
        for (fid, parent) in tree {
            let Some(parent) = parent else { continue };
            let Some(&from_parent) = direct.get(&parent) else {
                continue;
            };
            if direct.get(&fid).is_none_or(|own| *own < from_parent) {
                effective.insert(fid, from_parent);
                inherited.insert(fid, parent);
            }
        }
        Self {
            direct,
            effective,
            inherited,
            scope_excluded: Vec::new(),
        }
    }

    /// Adds the forums outside this site's scope (computed by the page frame from the multi-domain site scope,
    /// ascending).
    pub fn with_scope(mut self, excluded: Vec<i64>) -> Self {
        self.scope_excluded = excluded;
        self
    }

    /// Reads all of the member's settings (a primary-key range read, no more rows than there are forums), then expands
    /// them over the forum tree.
    pub async fn load(
        db: &Database,
        uid: i64,
        tree: impl IntoIterator<Item = (i64, Option<i64>)>,
    ) -> anyhow::Result<Self> {
        Ok(Self::new(Self::load_direct(db, uid).await?, tree))
    }

    pub(crate) async fn load_direct(
        db: &Database,
        uid: i64,
    ) -> anyhow::Result<BTreeMap<i64, FilterMode>> {
        let rows =
            crate::db::query("SELECT fid,filter_mode FROM gonof_home_forum_filter WHERE uid=?")
                .bind(uid)
                .fetch_all(db.executor())
                .await?;
        let mut direct = BTreeMap::new();
        for row in rows {
            let mode = db::text(&row, "filter_mode")?;
            let mode = FilterMode::parse(&mode)
                .ok_or_else(|| anyhow::anyhow!("invalid forum filter value: {mode}"))?;
            direct.insert(row.try_get::<i64, _>("fid")?, mode);
        }
        Ok(direct)
    }

    pub fn is_empty(&self) -> bool {
        self.direct.is_empty()
    }

    /// The level the member set directly on this forum.
    pub fn direct(&self, fid: i64) -> Option<FilterMode> {
        self.direct.get(&fid).copied()
    }

    /// The effective level after carrying over from the parent forum.
    pub fn effective(&self, fid: i64) -> Option<FilterMode> {
        self.effective.get(&fid).copied()
    }

    /// Returns the parent forum when the effective level comes from it.
    pub fn inherited_from(&self, fid: i64) -> Option<i64> {
        self.inherited.get(&fid).copied()
    }

    pub fn is_blocked(&self, fid: i64) -> bool {
        self.effective(fid) == Some(FilterMode::Block)
    }

    /// For query rows that carry their parent forum: does not depend on the forum tree in the page frame; true when
    /// this forum or its parent forum is blocked.
    pub fn blocks(&self, fid: i64, parent_fid: Option<i64>) -> bool {
        self.is_blocked(fid)
            || parent_fid.is_some_and(|parent| self.direct(parent) == Some(FilterMode::Block))
    }

    /// Forums to exclude when aggregating across forums: hidden and blocked ones, plus forums outside this site's
    /// scope.
    pub fn timeline_excluded(&self) -> Vec<i64> {
        self.merged(self.effective.keys().copied())
    }

    /// Only blocked forums: used by exits such as personal spaces where content is "not visible even when reached
    /// directly"; forums outside this site's scope are excluded as well.
    pub fn blocked(&self) -> Vec<i64> {
        self.merged(
            self.effective
                .iter()
                .filter(|(_, mode)| **mode == FilterMode::Block)
                .map(|(fid, _)| *fid),
        )
    }

    fn merged(&self, own: impl Iterator<Item = i64>) -> Vec<i64> {
        let mut fids: Vec<i64> = own.chain(self.scope_excluded.iter().copied()).collect();
        fids.sort_unstable();
        fids.dedup();
        fids
    }
}

/// List condition: returns an empty string when the exclusion set is empty, so the SQL text is exactly the same as
/// without filtering.
///
/// Forum ids come from the member's settings and the forum table and are all integers, so they are written straight
/// into the SQL, the same way `unblocked_sql` writes uids.
pub fn excluded_sql(column: &str, fids: &[i64]) -> String {
    if fids.is_empty() {
        return String::new();
    }
    let list = fids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(" AND {column} NOT IN ({list})")
}

/// Sets the target level for one forum; `None` clears it. Only affects the member and works as usual during read-only
/// periods (like following and blocking members).
pub async fn set(
    db: &Database,
    cache: &crate::cache::Cache,
    actor: Actor,
    fid: i64,
    desired: Option<FilterMode>,
    key: &str,
) -> Result<()> {
    if fid <= 0 || !identity::valid_request_key(key) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().social_error_filter_target(),
        ));
    }
    let desired_name = desired.map_or("none", FilterMode::as_str);
    let hash = identity::token_hash(
        &serde_json::json!({"relation":"forum_filter","target":fid,"desired":desired_name})
            .to_string(),
    );
    let mut tx = db.begin_write().await?;
    lock_actor(db, &mut tx, actor, false).await?;
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
        crate::cache::member::invalidate(cache, actor.uid).await?;
        return Ok(());
    }
    // A missing forum is a 404 right here; setting (not clearing) also requires the member to be able to read it
    // right now.
    crate::organization::share_forum_path(db, &mut tx, fid).await?;
    if desired.is_some() {
        let visible = crate::organization::forum_readable_sql("f", Some(actor.uid));
        let readable: i64 = crate::db::query_scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_forum f WHERE f.fid=? AND {visible}"
        ))
        .bind(fid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        if readable != 1 {
            return Err(ContentWriteError::NotFound);
        }
    }
    // The member row lock serializes the same member's settings, so this read sees previously committed results.
    let current: Option<String> = crate::db::query_scalar(
        "SELECT filter_mode FROM gonof_home_forum_filter WHERE uid=? AND fid=?",
    )
    .bind(actor.uid)
    .bind(fid)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    let timestamp = identity::now();
    let changed = match (current.as_deref().and_then(FilterMode::parse), desired) {
        (None, None) => false,
        (Some(_), None) => {
            crate::db::query("DELETE FROM gonof_home_forum_filter WHERE uid=? AND fid=?")
                .bind(actor.uid)
                .bind(fid)
                .execute(db.observe(&mut *tx))
                .await?;
            true
        }
        (Some(now), Some(want)) if now == want => false,
        (Some(_), Some(want)) => {
            crate::db::query(
                "UPDATE gonof_home_forum_filter SET filter_mode=?,dateline=? WHERE uid=? AND fid=?",
            )
            .bind(want.as_str())
            .bind(timestamp)
            .bind(actor.uid)
            .bind(fid)
            .execute(db.observe(&mut *tx))
            .await?;
            true
        }
        (None, Some(want)) => {
            crate::db::query(
                "INSERT INTO gonof_home_forum_filter(uid,fid,filter_mode,dateline) VALUES(?,?,?,?)",
            )
            .bind(actor.uid)
            .bind(fid)
            .bind(want.as_str())
            .bind(timestamp)
            .execute(db.observe(&mut *tx))
            .await?;
            true
        }
    };
    crate::identity::rules::insert_interaction_request(
        db, &mut tx, actor.uid, key, &hash, timestamp,
    )
    .await?;
    if changed {
        crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'interaction.forum_filter',?,'web',?)")
            .bind(actor.uid)
            .bind(actor.uid)
            .bind(serde_json::json!({"target":fid,"desired":desired_name}).to_string())
            .bind(timestamp)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    tx.commit().await?;
    crate::cache::member::invalidate(cache, actor.uid).await?;
    Ok(())
}

/// One row of the management list: a forum the member set directly.
pub struct FilterRow {
    pub fid: i64,
    pub mode: FilterMode,
    pub dateline: i64,
    pub name: String,
    pub slug: String,
}

/// All forums the member set directly, newest setting first. No more rows than there are forums, so one page lists
/// them all.
pub async fn list(db: &Database, actor: Actor) -> Result<Vec<FilterRow>> {
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    let rows = crate::db::query(
        "SELECT ff.fid,ff.filter_mode,ff.dateline,f.name,f.slug FROM gonof_home_forum_filter ff
         JOIN gonof_forum_forum f ON f.fid=ff.fid WHERE ff.uid=? ORDER BY ff.dateline DESC,ff.fid DESC",
    )
    .bind(actor.uid)
    .fetch_all(db.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            let mode = db::text(&row, "filter_mode")?;
            Ok(FilterRow {
                fid: row.try_get("fid")?,
                mode: FilterMode::parse(&mode).ok_or(ContentWriteError::Internal(
                    anyhow::anyhow!("invalid forum filter value: {mode}"),
                ))?,
                dateline: row.try_get("dateline")?,
                name: db::text(&row, "name")?,
                slug: db::text(&row, "slug")?,
            })
        })
        .collect()
}

/// Checked when delivering notifications: whether the recipient blocked this forum or its parent forum.
pub async fn blocks_forum_locked(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    fid: i64,
    parent_fid: Option<i64>,
) -> anyhow::Result<bool> {
    let parent = parent_fid.unwrap_or(fid);
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_forum_filter WHERE uid=? AND fid IN (?,?) AND filter_mode='block'",
    )
    .bind(uid)
    .bind(fid)
    .bind(parent)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(direct: &[(i64, FilterMode)]) -> ForumFilters {
        // 1 and 2 are parent forums, 11 and 12 are child forums of 1, 21 is a child forum of 2.
        ForumFilters::new(
            direct.iter().copied().collect(),
            [
                (1, None),
                (2, None),
                (11, Some(1)),
                (12, Some(1)),
                (21, Some(2)),
            ],
        )
    }

    #[test]
    fn parent_setting_covers_children_and_stronger_level_wins() {
        let f = filters(&[(1, FilterMode::Hide), (12, FilterMode::Block)]);
        assert_eq!(f.effective(11), Some(FilterMode::Hide));
        assert_eq!(f.inherited_from(11), Some(1));
        // The child forum's own setting is stronger, so it applies.
        assert_eq!(f.effective(12), Some(FilterMode::Block));
        assert_eq!(f.inherited_from(12), None);
        assert_eq!(f.effective(2), None);
        assert_eq!(f.timeline_excluded(), vec![1, 11, 12]);
        assert_eq!(f.blocked(), vec![12]);

        let f = filters(&[(2, FilterMode::Block), (21, FilterMode::Hide)]);
        assert_eq!(f.effective(21), Some(FilterMode::Block));
        assert_eq!(f.inherited_from(21), Some(2));
        assert_eq!(f.direct(21), Some(FilterMode::Hide));
        assert!(f.is_blocked(21));
    }

    #[test]
    fn excluded_sql_is_empty_without_settings() {
        assert_eq!(excluded_sql("t.fid", &[]), "");
        assert_eq!(excluded_sql("t.fid", &[3, 7]), " AND t.fid NOT IN (3,7)");
        assert!(ForumFilters::default().is_empty());
    }
}
