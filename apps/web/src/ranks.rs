//! Leaderboards.
//!
//! Four boards (growth, threads, replies, likes received) × three periods (24 hours, 30 days, all time). A
//! background task recomputes them every `settings.ranks.refresh_seconds` (re-read each round, [`Schedule`]), and the
//! places of each (board, period) are replaced wholesale into `gonof_rank_snapshot` in one transaction; reading a
//! board only fetches these ≤200 rows by primary key, regardless of the Redis switch.
//!
//! Threads, replies and likes received count only public content visible to anonymous visitors, with the same
//! conditions as the activity index; the growth board ranks by growth value.
//! Who may appear on a board is decided twice from the member's current state: a refresh keeps only members someone
//! may see, until both anonymous visitors and signed-in members have [`SNAPSHOT_SIZE`] places, so members with
//! private profiles cannot crowd out the ones after them; reading filters again, so a privacy change or a freeze
//! takes effect immediately without waiting for the next refresh.
use crate::db::Tx;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Result;

use crate::db::Database;

/// Places a snapshot keeps for each audience: the first 100 members anonymous visitors may see and the first 100
/// signed-in members may see (a snapshot therefore has at most 200 rows). The page shows the top
/// `settings.display.ranks_shown` (10–100), and the rest leaves headroom for filtering at read time.
pub const SNAPSHOT_SIZE: i64 = 100;

/// Candidates the first aggregation of a refresh asks for; when members nobody may see leave the snapshot short, the
/// aggregation is asked again for four times as many.
const FIRST_CANDIDATES: i64 = 4 * SNAPSHOT_SIZE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Board {
    Growth,
    Threads,
    Replies,
    Likes,
}

impl Board {
    pub const ALL: [Self; 4] = [Self::Growth, Self::Threads, Self::Replies, Self::Likes];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Growth => "growth",
            Self::Threads => "threads",
            Self::Replies => "replies",
            Self::Likes => "likes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Growth => crate::i18n::current().profile_growth(),
            Self::Threads => crate::i18n::current().site_ranks_board_threads(),
            Self::Replies => crate::i18n::current().site_ranks_board_replies(),
            Self::Likes => crate::i18n::current().site_ranks_board_likes(),
        }
    }

    /// The unit after the score.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Growth => crate::i18n::current().site_ranks_unit_growth(),
            Self::Threads => crate::i18n::current().site_ranks_unit_threads(),
            Self::Replies => crate::i18n::current().site_ranks_unit_replies(),
            Self::Likes => crate::i18n::current().site_ranks_unit_likes(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|board| board.slug() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Day,
    Month,
    All,
}

impl Period {
    pub const ALL: [Self; 3] = [Self::Day, Self::Month, Self::All];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Day => "24h",
            Self::Month => "30d",
            Self::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => crate::i18n::current().site_ranks_period_day(),
            Self::Month => crate::i18n::current().site_ranks_period_month(),
            Self::All => crate::i18n::current().site_ranks_period_all(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|period| period.slug() == value)
    }

    /// Start of the rolling window; the all-time period has no start.
    fn since(self, now: i64) -> Option<i64> {
        match self {
            Self::Day => Some(now - 86_400),
            Self::Month => Some(now - 30 * 86_400),
            Self::All => None,
        }
    }
}

/// Computes a board's top 100 from current data: (uid, score), score descending, ties by uid ascending.
///
/// The 5.7 optimizer always drives the aggregation over posts and likes from the forum table, which has only a few
/// rows, and never picks the time index by itself; with a time window, `FORCE INDEX` is spelled out and
/// `STRAIGHT_JOIN` fixes the post or like table as the driving table (measured on a 5.7.44 scratch database: the
/// 24-hour replies board went from 310,000 rows to 25,000).
pub async fn compute(
    db: &Database,
    board: Board,
    period: Period,
    now: i64,
) -> Result<Vec<(i64, i64)>> {
    candidates(db, board, period, now, SNAPSHOT_SIZE).await
}

/// The first `limit` members of a board before anyone is filtered out, in board order.
async fn candidates(
    db: &Database,
    board: Board,
    period: Period,
    now: i64,
    limit: i64,
) -> Result<Vec<(i64, i64)>> {
    let since = period.since(now);
    // The readable scope uses the forum IDs readable by anonymous visitors (one read of the forum table; the thread
    // row holds no forum copies), and post visibility looks at the post's own and its parent post's copies; no join
    // with the forum table or first post, and no subquery.
    let anonymous = crate::content::anonymous_forums(db).await?;
    let public = crate::content::public_threads(&anonymous);
    let post = crate::content::post_visible("p");
    let joins = "JOIN gonof_forum_thread t ON t.tid = p.tid";
    let (sql, binds): (String, Vec<i64>) = match board {
        Board::Growth => match since {
            Some(since) => (
                format!(
                    "SELECT l.uid, CAST(SUM(l.delta) AS SIGNED) AS score
                     FROM gonof_common_growth_ledger l FORCE INDEX (gonof_growth_window)
                     WHERE l.dateline >= ? GROUP BY l.uid HAVING score > 0
                     ORDER BY score DESC, l.uid LIMIT {limit}"
                ),
                vec![since],
            ),
            None => (
                format!(
                    "SELECT uid, growth AS score FROM gonof_common_growth_account
                     WHERE growth > 0 ORDER BY growth DESC, uid LIMIT {limit}"
                ),
                vec![],
            ),
        },
        Board::Threads | Board::Replies => {
            let first = i64::from(board == Board::Threads);
            match since {
                Some(since) => (
                    format!(
                        "SELECT STRAIGHT_JOIN p.authorid AS uid, COUNT(*) AS score
                         FROM gonof_forum_post p FORCE INDEX (gonof_post_dateline) {joins}
                         WHERE p.dateline >= ? AND p.first = ? AND p.authorid IS NOT NULL
                           AND {public} AND {post}
                         GROUP BY p.authorid ORDER BY score DESC, p.authorid LIMIT {limit}"
                    ),
                    vec![since, first],
                ),
                None => (
                    format!(
                        "SELECT p.authorid AS uid, COUNT(*) AS score
                         FROM gonof_forum_post p {joins}
                         WHERE p.first = ? AND p.authorid IS NOT NULL AND {public} AND {post}
                         GROUP BY p.authorid ORDER BY score DESC, p.authorid LIMIT {limit}"
                    ),
                    vec![first],
                ),
            }
        }
        Board::Likes => match since {
            Some(since) => (
                format!(
                    "SELECT STRAIGHT_JOIN p.authorid AS uid, COUNT(*) AS score
                     FROM gonof_forum_post_reaction r FORCE INDEX (gonof_reaction_dateline)
                     JOIN gonof_forum_post p ON p.pid = r.pid {joins}
                     WHERE r.dateline >= ? AND r.uid <> p.authorid AND {public} AND {post}
                     GROUP BY p.authorid ORDER BY score DESC, p.authorid LIMIT {limit}"
                ),
                vec![since],
            ),
            None => (
                format!(
                    "SELECT p.authorid AS uid, COUNT(*) AS score
                     FROM gonof_forum_post_reaction r
                     JOIN gonof_forum_post p ON p.pid = r.pid {joins}
                     WHERE r.uid <> p.authorid AND {public} AND {post}
                     GROUP BY p.authorid ORDER BY score DESC, p.authorid LIMIT {limit}"
                ),
                vec![],
            ),
        },
    };
    let mut query = crate::db::query(&sql);
    for value in binds {
        query = query.bind(value);
    }
    query
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("uid")?, row.try_get("score")?)))
        .collect()
}

/// Refreshes all 12 boards and returns how many got a new snapshot. Boards refreshed within the last `min_age` seconds
/// are skipped (with multiple processes, another process has already computed them). A board that fails does not stop
/// the others: each is tried, and the round reports an error naming how many failed, so the caller retries soon
/// instead of a full interval later ([`Schedule`]).
pub async fn refresh(db: &Database, now: i64, min_age: i64) -> Result<usize> {
    let mut refreshed = 0;
    let mut failed = Vec::new();
    for board in Board::ALL {
        for period in Period::ALL {
            match refresh_board(db, board, period, now, min_age).await {
                Ok(true) => refreshed += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        board = board.slug(),
                        period = period.slug(),
                        error = %format!("{error:#}"),
                        "leaderboard refresh failed; the other boards continue"
                    );
                    failed.push(format!("{}/{}", board.slug(), period.slug()));
                }
            }
        }
    }
    anyhow::ensure!(
        failed.is_empty(),
        "{} of 12 leaderboards were not refreshed ({}); {refreshed} were",
        failed.len(),
        failed.join(", ")
    );
    Ok(refreshed)
}

/// The refresh interval of the background task: a round runs once `settings.ranks.refresh_seconds` have passed since
/// the last round that refreshed every board. A round with a failed board is not recorded, so the next tick tries again
/// (boards that succeeded are fresh and are skipped).
#[derive(Debug, Default)]
pub struct Schedule {
    last: AtomicI64,
}

impl Schedule {
    /// One tick of the background task at `now`, with the interval read this round.
    pub async fn tick(&self, db: &Database, refresh_seconds: i64, now: i64) -> Result<()> {
        if now.saturating_sub(self.last.load(Ordering::Relaxed)) < refresh_seconds {
            return Ok(());
        }
        refresh(db, now, refresh_seconds / 2).await?;
        self.last.store(now, Ordering::Relaxed);
        Ok(())
    }
}

async fn computed_at(
    db: &Database,
    executor: Option<&mut Tx<'_>>,
    board: Board,
    period: Period,
) -> Result<Option<i64>> {
    // Kept per board in `gonof_rank_board`, so a board that came out empty still has its time.
    let sql = "SELECT computed_at FROM gonof_rank_board WHERE board=? AND period=?";
    Ok(match executor {
        Some(tx) => {
            crate::db::query_scalar(&format!("{sql} FOR UPDATE"))
                .bind(board.slug())
                .bind(period.slug())
                .fetch_optional(db.observe(&mut **tx))
                .await?
        }
        None => {
            crate::db::query_scalar(sql)
                .bind(board.slug())
                .bind(period.slug())
                .fetch_optional(db.executor())
                .await?
        }
    })
}

fn fresh(computed: Option<i64>, now: i64, min_age: i64) -> bool {
    computed.is_some_and(|at| at <= now && now - at < min_age)
}

/// Refreshes one board and returns whether a new snapshot was written: [`prepare`] then [`Prepared::store`].
pub async fn refresh_board(
    db: &Database,
    board: Board,
    period: Period,
    now: i64,
    min_age: i64,
) -> Result<bool> {
    match prepare(db, board, period, now, min_age).await? {
        Some(prepared) => prepared.store(db).await,
        None => Ok(false),
    }
}

/// A board computed at `now`, not yet stored.
#[derive(Clone, Debug)]
pub struct Prepared {
    board: Board,
    period: Period,
    now: i64,
    /// The board's computation time when this refresh read it, before aggregating.
    seen: Option<i64>,
    rows: Vec<(i64, i64)>,
}

/// Computes one board unless it is fresh. Aggregation runs outside any transaction (consistent read, no locks).
pub async fn prepare(
    db: &Database,
    board: Board,
    period: Period,
    now: i64,
    min_age: i64,
) -> Result<Option<Prepared>> {
    let seen = computed_at(db, None, board, period).await?;
    if fresh(seen, now, min_age) {
        return Ok(None);
    }
    let rows = places(db, board, period, now).await?;
    Ok(Some(Prepared {
        board,
        period,
        now,
        seen,
        rows,
    }))
}

impl Prepared {
    /// Replaces the snapshot in a short transaction and returns whether it did. It first locks the board's row in
    /// `gonof_rank_board`, so a consistent read of the board sees either all old rows or all new ones, and gives up
    /// when the board was replaced after this refresh read it: another worker stored it meanwhile, possibly one that
    /// started later and so holds newer figures (its time is then later than this refresh's `now`). A time in the
    /// future that was already there before (the clock went back) is replaced as usual.
    pub async fn store(&self, db: &Database) -> Result<bool> {
        crate::activities::retry_transaction(|| self.replace(db)).await
    }

    async fn replace(&self, db: &Database) -> Result<bool> {
        let (board, period, now) = (self.board, self.period, self.now);
        let mut tx = db.begin_write().await?;
        if computed_at(db, Some(&mut tx), board, period).await? != self.seen {
            tx.commit().await?;
            return Ok(false);
        }
        crate::db::query("DELETE FROM gonof_rank_snapshot WHERE board=? AND period=?")
            .bind(board.slug())
            .bind(period.slug())
            .execute(db.observe(&mut *tx))
            .await?;
        if !self.rows.is_empty() {
            let values = vec!["(?,?,?,?,?,?)"; self.rows.len()].join(",");
            let sql = format!(
                "INSERT INTO gonof_rank_snapshot(board,period,position,uid,score,computed_at) VALUES {values}"
            );
            let mut query = crate::db::query(&sql);
            for (index, (uid, score)) in self.rows.iter().enumerate() {
                query = query
                    .bind(board.slug())
                    .bind(period.slug())
                    .bind(i64::try_from(index)? + 1)
                    .bind(*uid)
                    .bind(*score)
                    .bind(now);
            }
            query.execute(db.observe(&mut *tx)).await?;
        }
        crate::db::query("INSERT INTO gonof_rank_board(board,period,computed_at) VALUES(?,?,?) ON DUPLICATE KEY UPDATE computed_at=VALUES(computed_at)")
            .bind(board.slug())
            .bind(period.slug())
            .bind(now)
            .execute(db.observe(&mut *tx))
            .await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// Who may see a member on a board, from the member's current state (the rules [`read_listed`] applies).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Audience {
    /// Frozen, deleted or being deleted, or a profile space of "Only me".
    Nobody,
    /// A profile space of "Logged-in members".
    SignedIn,
    Everyone,
}

impl Audience {
    fn of(account_state: &str, freeze: i64, space: &str) -> Self {
        if account_state != "active" || freeze != 0 {
            return Self::Nobody;
        }
        match space {
            "public" => Self::Everyone,
            "members" => Self::SignedIn,
            _ => Self::Nobody,
        }
    }
}

async fn audiences(db: &Database, uids: &[i64]) -> Result<HashMap<i64, Audience>> {
    let mut found = HashMap::new();
    for chunk in uids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT uid,account_state,freeze,space_visibility FROM gonof_common_member WHERE uid IN ({placeholders})"
        );
        let mut query = crate::db::query(&sql);
        for uid in chunk {
            query = query.bind(*uid);
        }
        for row in query.fetch_all(db.executor()).await? {
            found.insert(
                row.try_get("uid")?,
                Audience::of(
                    &crate::db::text(&row, "account_state")?,
                    row.try_get("freeze")?,
                    &crate::db::text(&row, "space_visibility")?,
                ),
            );
        }
    }
    Ok(found)
}

/// The rows a snapshot keeps, chosen from candidates in board order.
#[derive(Debug, Default)]
struct Places {
    rows: Vec<(i64, i64)>,
    everyone: i64,
    signed_in: i64,
}

impl Places {
    fn full(&self) -> bool {
        self.everyone >= SNAPSHOT_SIZE && self.signed_in >= SNAPSHOT_SIZE
    }

    /// Keeps a candidate while an audience that may see it still has places left.
    fn offer(&mut self, row: (i64, i64), audience: Audience) {
        match audience {
            Audience::Nobody => {}
            Audience::SignedIn => {
                if self.signed_in < SNAPSHOT_SIZE {
                    self.rows.push(row);
                    self.signed_in += 1;
                }
            }
            Audience::Everyone => {
                if self.everyone < SNAPSHOT_SIZE || self.signed_in < SNAPSHOT_SIZE {
                    self.rows.push(row);
                    self.everyone += 1;
                    self.signed_in += 1;
                }
            }
        }
    }
}

/// The places of a board's snapshot: candidates in board order, leaving out members nobody may see, until both
/// audiences have [`SNAPSHOT_SIZE`] places or the candidates run out. When the first candidates do not fill them, the
/// aggregation is asked for four times as many.
async fn places(db: &Database, board: Board, period: Period, now: i64) -> Result<Vec<(i64, i64)>> {
    let mut limit = FIRST_CANDIDATES;
    loop {
        let rows = candidates(db, board, period, now, limit).await?;
        let uids: Vec<i64> = rows.iter().map(|(uid, _)| *uid).collect();
        let audiences = audiences(db, &uids).await?;
        let mut places = Places::default();
        for row in &rows {
            places.offer(
                *row,
                audiences.get(&row.0).copied().unwrap_or(Audience::Nobody),
            );
            if places.full() {
                break;
            }
        }
        if places.full() || i64::try_from(rows.len())? < limit {
            return Ok(places.rows);
        }
        limit = limit.saturating_mul(4);
    }
}

/// One row on the page.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Standing {
    /// Place after filtering, starting from 1; people filtered out take no place.
    pub rank: usize,
    pub uid: i64,
    pub username: String,
    pub avatar: i64,
    pub avatar_aid: Option<i64>,
    pub score: i64,
    pub growth: i64,
}

/// The current snapshot of one board; an empty `computed_at` means it has not been computed yet.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub standings: Vec<Standing>,
    pub computed_at: Option<i64>,
}

/// Reads a board and filters it by the members' current state: frozen, deleted, deleting members and
/// those whose profile space is "Only me" are left off; "Logged-in members" ones are shown only to logged-in visitors;
/// people the visitor has blocked are removed from the board they see. Places move up accordingly.
///
/// `viewer` must be a member who has already passed authentication.
pub async fn read(
    db: &Database,
    board: Board,
    period: Period,
    viewer: Option<i64>,
) -> Result<Snapshot> {
    let mut snapshot = read_listed(db, board, period, viewer.is_some()).await?;
    if let Some(viewer) = viewer {
        let uids: BTreeSet<i64> = snapshot
            .standings
            .iter()
            .map(|standing| standing.uid)
            .collect();
        let blocked = blocked_among(db, viewer, &uids).await?;
        snapshot.without(&blocked);
    }
    Ok(snapshot)
}

impl Snapshot {
    /// Removes these members and closes up the places.
    pub fn without(&mut self, uids: &HashSet<i64>) {
        if uids.is_empty() {
            return;
        }
        self.standings
            .retain(|standing| !uids.contains(&standing.uid));
        for (index, standing) in self.standings.iter_mut().enumerate() {
            standing.rank = index + 1;
        }
    }
}

/// [`read`] without the viewer's blocks: the board as every anonymous visitor (`signed_in` false) or every signed-in
/// member sees it before their own blocks, so one copy can serve all of them (the `members.rank` data source caches
/// it and removes the viewer's blocks in code).
pub async fn read_listed(
    db: &Database,
    board: Board,
    period: Period,
    signed_in: bool,
) -> Result<Snapshot> {
    let rows = crate::db::query(
        "SELECT s.uid,s.score,m.username,m.avatar,m.avatar_aid,m.account_state,m.freeze,m.space_visibility,g.growth
         FROM gonof_rank_snapshot s JOIN gonof_common_member m ON m.uid=s.uid
         LEFT JOIN gonof_common_growth_account g ON g.uid=s.uid
         WHERE s.board=? AND s.period=? ORDER BY s.position",
    )
    .bind(board.slug())
    .bind(period.slug())
    .fetch_all(db.executor())
    .await?;
    let computed_at = computed_at(db, None, board, period).await?;
    let mut standings = Vec::new();
    for row in rows {
        let uid: i64 = row.try_get("uid")?;
        let listed = match Audience::of(
            &crate::db::text(&row, "account_state")?,
            row.try_get("freeze")?,
            &crate::db::text(&row, "space_visibility")?,
        ) {
            Audience::Everyone => true,
            Audience::SignedIn => signed_in,
            Audience::Nobody => false,
        };
        if !listed {
            continue;
        }
        standings.push(Standing {
            rank: standings.len() + 1,
            uid,
            username: crate::db::text(&row, "username")?,
            avatar: row.try_get("avatar")?,
            avatar_aid: row.try_get("avatar_aid")?,
            score: row.try_get("score")?,
            growth: row.try_get::<Option<i64>, _>("growth")?.unwrap_or(0),
        });
    }
    Ok(Snapshot {
        standings,
        computed_at,
    })
}

async fn blocked_among(db: &Database, viewer: i64, uids: &BTreeSet<i64>) -> Result<HashSet<i64>> {
    if uids.is_empty() {
        return Ok(HashSet::new());
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql = format!(
        "SELECT blocked_uid FROM gonof_home_member_block WHERE uid=? AND blocked_uid IN ({placeholders})"
    );
    let mut query = crate::db::query_scalar(&sql).bind(viewer);
    for uid in uids {
        query = query.bind(*uid);
    }
    Ok(query.fetch_all(db.executor()).await?.into_iter().collect())
}

/// On account deletion, removes this member's rows from the snapshots; the next refresh would exclude them anyway,
/// this makes them disappear immediately.
pub async fn forget_member(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    crate::db::query("DELETE FROM gonof_rank_snapshot WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip() {
        for board in Board::ALL {
            assert_eq!(Board::parse(board.slug()), Some(board));
        }
        for period in Period::ALL {
            assert_eq!(Period::parse(period.slug()), Some(period));
        }
        assert_eq!(Board::parse("credits"), None);
        assert_eq!(Period::parse("7d"), None);
    }

    #[test]
    fn freshness_respects_min_age_and_clock_skew() {
        assert!(!fresh(None, 100, 30));
        assert!(fresh(Some(90), 100, 30));
        assert!(!fresh(Some(60), 100, 30));
        assert!(!fresh(Some(90), 100, 0));
        // A snapshot time in the future (clock rollback) is recomputed as usual.
        assert!(!fresh(Some(200), 100, 30));
    }
}
