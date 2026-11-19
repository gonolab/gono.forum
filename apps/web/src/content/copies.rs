//! The only write entry point for denormalized copies on thread rows and for content cache versions
//! (docs/architecture.md, "Data"; docs/queries.md, "Copy columns").
//!
//! Thread rows store copies of the author name, the last replier and the top-level forum id; lists display and locate
//! by them without joins. The forum name and URL, the inherited forum visibility and enabled state, and the author
//! avatar are not copied: the read path takes the forum from the in-process forum cache by `fid` and decides
//! readability there, and list avatars are served by uid. There are no triggers: only this module rewrites the
//! copies when their source rows change (pinned by `db::source_guards`).
//! Threads that are not listed have a negative `displayorder` (first post
//! hidden by moderation [`HIDDEN_ORDER`], reserved states [`RESERVED_ORDER`]); the per-forum thread count
//! `gonof_forum_thread_count` and the member count `gonof_common_member_count` are also only incremented and
//! decremented here, and the `root_invisible` of nested replies is rewritten along with their parent floor:
//!
//! - Posting a thread and the development fixtures use [`ThreadCopies`] to gather the copy values and write them into
//!   the INSERT; `family_fid` has no default, so leaving it out fails with 1364;
//! - Moving a thread [`move_thread`], re-parenting a forum [`forum_saved`], renames and account deletion
//!   [`member_changed`], hiding and restoring a first post [`sync_first_hidden`], and hiding and restoring a parent
//!   floor [`sync_root_invisible`] rewrite inside the original transaction.
//!
//! Cache versions have two layers: the thread version `cache_revision` is incremented on
//! every UPDATE of the thread row, and the thread page's floor and rendering caches are invalidated by it; the
//! site-wide version is only incremented via [`bump_site`] on structural events (title changes, hiding, withdrawal,
//! moves, forum changes, renames and account deletion, etc., but not the counts and last reply on the reply path;
//! changing an avatar touches neither the thread row nor the site-wide version). Keys of lists, sidebars and feeds also
//! carry the new-post watermark (`content::list_revision`), which changes the key for new threads and new replies, so
//! replies, likes and poll votes do not touch the site-wide version row.

use crate::db::Tx;
use anyhow::{Context, Result, ensure};

use crate::db::{self, Database};

/// Name stored for the author and last replier in lists when the account has been deleted (or the author row does not
/// exist). It is a marker, shown as `common-deleted-user` in the page's language ([`shown_name`]); the stored value is
/// the Simplified Chinese for "deleted user", kept as it is so that existing rows keep matching.
pub const DELETED_AUTHOR: &str = "\u{5df2}\u{6ce8}\u{9500}\u{7528}\u{6237}";

/// A name copy as shown: [`DELETED_AUTHOR`] is the stored marker of a deleted account and is shown in this request's
/// language (docs/architecture.md, "Languages": stored system text is translated when shown); other names are as
/// typed.
pub fn shown_name(name: String) -> String {
    if name == DELETED_AUTHOR {
        crate::i18n::current().common_deleted_user().to_owned()
    } else {
        name
    }
}

/// Forum visibility, ordered by strictness: inheritance takes the stricter of this level and its parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ForumVisibility {
    Public,
    Members,
    Private,
}

impl ForumVisibility {
    pub const ALL: [Self; 3] = [Self::Public, Self::Members, Self::Private];

    /// The value stored in `gonof_forum_forum.visibility`.
    pub fn value(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Members => "members",
            Self::Private => "private",
        }
    }

    /// Exact comparison: does not accept spellings that differ in case or trailing spaces (a comparison in SQL under
    /// a PAD SPACE collation would let them through).
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.value() == value)
    }
}

/// Inherited forum scope: visibility is the stricter of this level and its parent, and it is enabled only when both
/// levels are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForumScope {
    pub visibility: ForumVisibility,
    pub enabled: bool,
}

impl ForumScope {
    /// The scope of a forum row itself; an unknown visibility is an error and is never treated as public.
    pub fn own(visibility: &str, status: i64) -> Result<Self> {
        Ok(Self {
            visibility: ForumVisibility::parse(visibility)
                .with_context(|| format!("invalid forum visibility: {visibility:?}"))?,
            enabled: status == 1,
        })
    }

    pub fn inherit(self, parent: Option<Self>) -> Self {
        match parent {
            None => self,
            Some(parent) => Self {
                visibility: self.visibility.max(parent.visibility),
                enabled: self.enabled && parent.enabled,
            },
        }
    }
}

/// Columns to select when computing the inherited scope of a thread's forum from the forum table (the thread row
/// carries no forum copies): the statement joins [`FORUM_SCOPE_JOINS`] (thread alias `t`).
pub const FORUM_SCOPE_COLUMNS: &str = "f.visibility AS forum_visibility,f.status AS forum_status,pf.visibility AS parent_visibility,pf.status AS parent_status";

/// Companion of [`FORUM_SCOPE_COLUMNS`]: joins the thread's forum (`f`) and its parent forum (`pf`).
pub const FORUM_SCOPE_JOINS: &str =
    "JOIN gonof_forum_forum f ON f.fid=t.fid LEFT JOIN gonof_forum_forum pf ON pf.fid=f.parent_fid";

/// Computes the inherited scope from the columns selected by [`FORUM_SCOPE_COLUMNS`]: visibility is the stricter of
/// this level and its parent, and it is enabled only when both levels are enabled.
pub fn forum_scope(row: &crate::db::DbRow) -> Result<ForumScope> {
    let own = ForumScope::own(
        &db::text(row, "forum_visibility")?,
        row.try_get("forum_status")?,
    )?;
    let parent = match row.try_get::<Option<String>, _>("parent_visibility")? {
        Some(visibility) => Some(ForumScope::own(
            &visibility,
            row.try_get::<Option<i64>, _>("parent_status")?
                .context("the parent forum has no status")?,
        )?),
        None => None,
    };
    Ok(own.inherit(parent))
}

/// `displayorder` of a thread whose first post is hidden by moderation: lists
/// only take `displayorder >= 0`. Unhiding always sets it back to 0; the previous pin level is not kept.
pub const HIDDEN_ORDER: i64 = -1;

/// `displayorder` of the reserved states unpublished, private and read-permission-restricted (no write entry point
/// yet; migrations and check scripts use it to decide).
pub const RESERVED_ORDER: i64 = -2;

/// The thread row is in a reserved state (unpublished, private, read-permission-restricted): never listed, whether or
/// not the first post is hidden.
pub fn reserved(visibility: &str, publication_state: &str, readperm: i64) -> bool {
    publication_state != "published" || !matches!(visibility, "public" | "members") || readperm != 0
}

/// Forum copies on the thread row: only the top-level forum id (the forum name, URL and inherited scope come from
/// the in-process forum cache by fid).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumCopies {
    /// The thread row's `family_fid`: the parent forum's id, or the forum itself for a top-level forum. A parent forum
    /// page reads the threads of itself and its child forums in one range by it.
    pub family_fid: i64,
}

impl ForumCopies {
    /// Reads the forum's current parent. The caller has already locked the forum path (shared or exclusive), so the
    /// value does not change before commit.
    pub async fn load(db: &Database, tx: &mut Tx<'_>, fid: i64) -> Result<Self> {
        let parent: Option<i64> =
            db::query_scalar("SELECT parent_fid FROM gonof_forum_forum WHERE fid=?")
                .bind(fid)
                .fetch_optional(db.observe(&mut **tx))
                .await?
                .with_context(|| format!("forum {fid} does not exist"))?;
        Ok(Self {
            family_fid: parent.unwrap_or(fid),
        })
    }
}

/// Author copies on the thread row: only the username (list avatars are served by uid).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorCopies {
    pub username: String,
}

impl AuthorCopies {
    /// Reads the current values of the member row. The caller has already locked that member row.
    pub async fn load(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<Self> {
        let username: String =
            db::query_scalar("SELECT username FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_optional(db.observe(&mut **tx))
                .await?
                .with_context(|| format!("member {uid} does not exist"))?;
        Ok(Self { username })
    }
}

/// Copy values to write into a new thread row (shared by posting a thread and the development fixtures). A new thread
/// is not hidden, and `displayorder` is the pin level (0 when posting).
#[derive(Clone, Debug)]
pub struct ThreadCopies {
    pub author: AuthorCopies,
    pub forum: ForumCopies,
}

/// Name shown for "Last replier" in lists: always [`DELETED_AUTHOR`] when the account has been deleted (or the member
/// row does not exist), never the pseudonym generated on deletion. Replies, recomputing valid replies,
/// and renames/account deletion all get the name through here.
pub fn last_reply_name(username: Option<String>, account_state: Option<&str>) -> String {
    match username {
        Some(name) if account_state != Some("deleted") => name,
        _ => DELETED_AUTHOR.into(),
    }
}

/// The `last_reply_uid` copy of a reply by `authorid` whose account is in `account_state`: `None` when the reply has
/// no author or the account is deleted, the same cases [`last_reply_name`] shows as "Deleted user".
pub fn last_reply_uid(authorid: Option<i64>, account_state: Option<&str>) -> Option<i64> {
    authorid.filter(|_| account_state.is_some_and(|state| state != "deleted"))
}

/// Increments the site-wide content version. Only called on structural events, preferably among the last statements
/// of the transaction: the exclusive lock on this row is held until commit.
pub async fn bump_site(db: &Database, tx: &mut Tx<'_>) -> Result<()> {
    let result =
        db::query("UPDATE gonof_common_content_revision SET revision=revision+1 WHERE id=1")
            .execute(db.observe(&mut **tx))
            .await?;
    ensure!(
        result.rows_affected() == 1,
        "the site-wide content version row is missing"
    );
    Ok(())
}

/// Invalidates the thread page's floor and rendering caches when only a post row changed and there is no other reason
/// to touch the thread row (editing a reply that is not the last one).
pub async fn touch_thread(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<()> {
    db::query("UPDATE gonof_forum_thread SET cache_revision=cache_revision+1 WHERE tid=?")
        .bind(tid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Moves a thread: the same statement that changes `fid` writes the target forum's top-level forum id. The caller has
/// already exclusively locked the source and target forum paths in ascending FID order, locked the thread row and
/// checked the version, and increments the site-wide version before finishing.
pub async fn move_thread(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
    version: i64,
    target_fid: i64,
) -> Result<()> {
    let forum = ForumCopies::load(db, tx, target_fid).await?;
    let before = ListedThread::load(db, tx, tid).await?;
    let result = db::query(
        "UPDATE gonof_forum_thread SET fid=?,family_fid=?,version=version+1,cache_revision=cache_revision+1 WHERE tid=? AND version=?",
    )
    .bind(target_fid)
    .bind(forum.family_fid)
    .bind(tid)
    .bind(version)
    .execute(db.observe(&mut **tx))
    .await?;
    ensure!(
        result.rows_affected() == 1,
        "thread {tid} changed its version under the lock"
    );
    if before.listed() && before.fid != target_fid {
        count_thread(
            db,
            tx,
            (before.fid, &before.visibility, before.category),
            -1,
        )
        .await?;
        count_thread(db, tx, (target_fid, &before.visibility, before.category), 1).await?;
    }
    Ok(())
}

/// The columns of the thread row that determine listing and counts. The caller has already locked the thread row.
#[derive(Clone, Debug)]
pub struct ListedThread {
    pub fid: i64,
    pub authorid: Option<i64>,
    pub visibility: String,
    pub category: i64,
    pub displayorder: i64,
    pub digest: i64,
    pub publication_state: String,
    pub readperm: i64,
}

impl ListedThread {
    pub async fn load(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<Self> {
        let row = db::query(
            "SELECT fid,authorid,visibility,category,displayorder,digest,publication_state,readperm FROM gonof_forum_thread WHERE tid=?",
        )
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .with_context(|| format!("thread {tid} does not exist"))?;
        Ok(Self {
            fid: row.try_get("fid")?,
            authorid: row.try_get("authorid")?,
            visibility: db::text(&row, "visibility")?,
            category: row.try_get("category")?,
            displayorder: row.try_get("displayorder")?,
            digest: row.try_get("digest")?,
            publication_state: db::text(&row, "publication_state")?,
            readperm: row.try_get("readperm")?,
        })
    }

    /// Can be listed: `displayorder >= 0`.
    pub fn listed(&self) -> bool {
        self.displayorder >= 0
    }
}

/// Adjusts the per-forum thread count (`gonof_forum_thread_count`): listable threads are counted by (forum, thread
/// visibility, category), and a list total is one single-table `SUM`. Only posting a thread, moving, changing
/// visibility, and hiding and restoring the first post move a thread in or out of this count.
pub async fn count_thread(
    db: &Database,
    tx: &mut Tx<'_>,
    (fid, visibility, category): (i64, &str, i64),
    delta: i64,
) -> Result<()> {
    db::query(
        "INSERT INTO gonof_forum_thread_count(fid,visibility,category,threads) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE threads=threads+?",
    )
    .bind(fid)
    .bind(visibility)
    .bind(category)
    .bind(delta)
    .bind(delta)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// Adjusts the member count (`gonof_common_member_count`): the member's own threads
/// and replies that are not withdrawn or hidden, and the highlighted threads among them. Both the profile page and the
/// sidebar card read it. Posts without an author are not counted.
pub async fn count_member(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: Option<i64>,
    (threads, replies, digests): (i64, i64, i64),
) -> Result<()> {
    let Some(uid) = uid else {
        return Ok(());
    };
    if (threads, replies, digests) == (0, 0, 0) {
        return Ok(());
    }
    db::query(
        "INSERT INTO gonof_common_member_count(uid,threads,replies,digests) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE threads=threads+?,replies=replies+?,digests=digests+?",
    )
    .bind(uid)
    .bind(threads)
    .bind(replies)
    .bind(digests)
    .bind(threads)
    .bind(replies)
    .bind(digests)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// Whether a post counts towards the member count: not hidden and not withdrawn (erasure always comes with withdrawal).
pub fn counted_post(invisible: i64, deleted_at: i64) -> bool {
    invisible == 0 && deleted_at == 0
}

/// Adjusts the author's member count when a post changes from "counted" to "not counted" (or back): a first post counts
/// as a thread (and as a highlight if highlighted), anything else as a reply.
pub async fn post_counted_changed(
    db: &Database,
    tx: &mut Tx<'_>,
    (authorid, first, digest): (Option<i64>, bool, i64),
    delta: i64,
) -> Result<()> {
    let counts = if first {
        (delta, 0, if digest > 0 { delta } else { 0 })
    } else {
        (0, delta, 0)
    };
    count_member(db, tx, authorid, counts).await
}

/// Fields on the forum row that affect thread lists and thread pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumFields {
    pub name: String,
    pub slug: String,
    pub visibility: String,
    pub status: i64,
    pub parent_fid: Option<i64>,
}

/// After saving a forum: only increments the site-wide version when these fields really changed (changing the order,
/// description or icon does not), and when the parent forum changed, also rewrites the top-level forum id of this
/// forum's threads. Name, URL, visibility and enabled state are not on thread rows (the read path decides from the
/// in-process forum cache), so thread rows are not rewritten for them. Returns whether anything changed. The caller has
/// already exclusively locked this forum and the (new) parent forum.
pub async fn forum_saved(
    db: &Database,
    tx: &mut Tx<'_>,
    fid: i64,
    before: &ForumFields,
    after: &ForumFields,
) -> Result<bool> {
    if before == after {
        return Ok(false);
    }
    if before.parent_fid != after.parent_fid {
        resync_forum(db, tx, fid).await?;
    } else {
        bump_site(db, tx).await?;
    }
    Ok(true)
}

/// Rewrites the top-level forum id of all threads under a forum according to its current parent (via the `fid`
/// index), and finally increments the site-wide version. Threads of child forums are not affected: a forum that has
/// child forums cannot be moved under another forum (forums have at most two levels). The caller has already
/// exclusively locked that forum.
pub async fn resync_forum(db: &Database, tx: &mut Tx<'_>, fid: i64) -> Result<()> {
    let forum = ForumCopies::load(db, tx, fid).await?;
    db::query(
        "UPDATE gonof_forum_thread SET family_fid=?,cache_revision=cache_revision+1 WHERE fid=?",
    )
    .bind(forum.family_fid)
    .bind(fid)
    .execute(db.observe(&mut **tx))
    .await?;
    bump_site(db, tx).await
}

/// A member's username or account state changed: rewrites the author copy on all of the member's threads (`authorid`
/// index), then the last replier of threads whose last reply is the member's (writing [`DELETED_AUTHOR`] after account
/// deletion), and finally increments the site-wide version. Not needed when only the avatar changes: list avatars are
/// served by uid.
///
/// The caller has already locked that member row. The member's own replies also lock the member's row first, and
/// replies by others to the member's threads lock both parties in ascending UID order, so the old name cannot be
/// written during the rewrite.
pub async fn member_changed(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    let row = db::query("SELECT username,account_state FROM gonof_common_member WHERE uid=?")
        .bind(uid)
        .fetch_one(db.observe(&mut **tx))
        .await?;
    let username: String = row.try_get("username")?;
    let state = db::text(&row, "account_state")?;
    // After account deletion the author copy on thread rows is written directly as "Deleted user", so lists need not
    // join the member table to check the account state.
    let author = if state == "deleted" {
        DELETED_AUTHOR.to_owned()
    } else {
        username.clone()
    };
    db::query(
        "UPDATE gonof_forum_thread SET author=?,cache_revision=cache_revision+1 WHERE authorid=?",
    )
    .bind(&author)
    .bind(uid)
    .execute(db.observe(&mut **tx))
    .await?;
    let shown = last_reply_name(Some(username), Some(&state));
    // A deleted account is no longer the last replier by uid either: the copy becomes NULL with the name.
    let shown_uid = last_reply_uid(Some(uid), Some(&state));
    // First read with a lock the threads whose last replier is this member, on the `last_reply_uid` index, then update
    // by thread id (not a multi-table UPDATE, which SQLite writes differently): once locked, other replies cannot
    // change the last reply of these threads, and updating by thread id cannot hit the wrong person.
    let tids: Vec<i64> =
        db::query_scalar("SELECT tid FROM gonof_forum_thread WHERE last_reply_uid=? FOR UPDATE")
            .bind(uid)
            .fetch_all(db.observe(&mut **tx))
            .await?;
    update_keys(
        db,
        tx,
        "UPDATE gonof_forum_thread SET last_reply_author=?,last_reply_uid=?,cache_revision=cache_revision+1 WHERE tid IN (",
        |query| query.bind(shown.clone()).bind(shown_uid),
        &tids,
    )
    .await?;
    bump_site(db, tx).await
}

/// The first post's hidden state changed: rewrites the thread row's `displayorder` from the first post's current
/// `invisible` — [`HIDDEN_ORDER`] when hidden, back to 0 when unhidden (the pin level
/// is not kept), while threads in a reserved state keep [`RESERVED_ORDER`]; adjusts the per-forum thread count when
/// a thread enters or leaves the list. The caller has already locked the thread row and the first post, and
/// increments the site-wide version before finishing.
pub async fn sync_first_hidden(db: &Database, tx: &mut Tx<'_>, tid: i64) -> Result<()> {
    let invisible: i64 =
        db::query_scalar("SELECT invisible FROM gonof_forum_post WHERE first_tid=?")
            .bind(tid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    let thread = ListedThread::load(db, tx, tid).await?;
    let target = if invisible != 0 {
        HIDDEN_ORDER
    } else if reserved(
        &thread.visibility,
        &thread.publication_state,
        thread.readperm,
    ) {
        RESERVED_ORDER
    } else if matches!(thread.displayorder, HIDDEN_ORDER | RESERVED_ORDER) {
        // Only these two negative values written by this function are reset; other negatives are reserved for
        // moderation and are left alone.
        0
    } else {
        thread.displayorder
    };
    if target == thread.displayorder {
        return Ok(());
    }
    db::query(
        "UPDATE gonof_forum_thread SET displayorder=?,cache_revision=cache_revision+1 WHERE tid=?",
    )
    .bind(target)
    .bind(tid)
    .execute(db.observe(&mut **tx))
    .await?;
    if thread.listed() != (target >= 0) {
        let delta = if target >= 0 { 1 } else { -1 };
        count_thread(
            db,
            tx,
            (thread.fid, &thread.visibility, thread.category),
            delta,
        )
        .await?;
    }
    Ok(())
}

/// The moderation hidden state of a main floor changed: the `root_invisible` of its nested replies follows, so
/// deciding whether a nested reply is visible does not look up the parent floor.
/// The caller has already locked the main floor. Returns how many nested replies became live replies (positive) or
/// stopped being live (negative), for `content::reply_visibility_changed`: those neither hidden nor withdrawn
/// themselves, rewritten by a statement of their own whose affected rows are that number. Only rows whose copy
/// really changes match (`root_invisible<>?`), since the MySQL connection reports matched rather than changed rows.
pub async fn sync_root_invisible(
    db: &Database,
    tx: &mut Tx<'_>,
    root_pid: i64,
    invisible: i64,
) -> Result<i64> {
    let flipped = db::query(
        "UPDATE gonof_forum_post SET root_invisible=? WHERE root_pid=? AND root_invisible<>? AND invisible=0 AND deleted_at=0",
    )
    .bind(invisible)
    .bind(root_pid)
    .bind(invisible)
    .execute(db.observe(&mut **tx))
    .await?
    .rows_affected();
    db::query(
        "UPDATE gonof_forum_post SET root_invisible=? WHERE root_pid=? AND root_invisible<>?",
    )
    .bind(invisible)
    .bind(root_pid)
    .bind(invisible)
    .execute(db.observe(&mut **tx))
    .await?;
    let flipped = i64::try_from(flipped)?;
    Ok(if invisible == 0 { flipped } else { -flipped })
}

/// First look up the keys to change and then update by key (not a multi-table UPDATE, which SQLite writes
/// differently; split up, both backends use the same statement). `sql` ends with `IN (`, with at
/// most 500 keys per statement; `set` binds the parameters before the keys (the values in `SET`). The caller looks up
/// the keys first in the same transaction.
pub(crate) async fn update_keys(
    db: &Database,
    tx: &mut Tx<'_>,
    sql: &str,
    set: impl for<'q> Fn(db::Query<'q>) -> db::Query<'q>,
    keys: &[i64],
) -> Result<()> {
    for chunk in keys.chunks(500) {
        let statement = format!("{sql}{})", vec!["?"; chunk.len()].join(","));
        let mut query = set(db::query(&statement));
        for key in chunk {
            query = query.bind(*key);
        }
        query.execute(db.observe(&mut **tx)).await?;
    }
    Ok(())
}

/// Like [`update_keys`], but each key has its own value to write: (key, value) pairs are grouped by value, one
/// statement per group. Counts and hidden values take few distinct values, so there are far fewer statements than rows.
pub(crate) async fn update_by_value(
    db: &Database,
    tx: &mut Tx<'_>,
    sql: &str,
    rows: Vec<(i64, Vec<i64>)>,
) -> Result<()> {
    let mut groups: std::collections::BTreeMap<Vec<i64>, Vec<i64>> = Default::default();
    for (key, values) in rows {
        groups.entry(values).or_default().push(key);
    }
    for (values, keys) in groups {
        update_keys(
            db,
            tx,
            sql,
            |mut query| {
                for value in &values {
                    query = query.bind(*value);
                }
                query
            },
            &keys,
        )
        .await?;
    }
    Ok(())
}

/// After threads and posts were written in bulk bypassing the business entry points (development fixture import, test
/// data), recomputes with the rules the write paths follow: the `displayorder` of unlisted threads, the
/// `root_invisible` of nested replies, the per-forum thread counts and the member counts. Finally increments the
/// site-wide version.
pub async fn rebuild_listing(db: &Database, tx: &mut Tx<'_>) -> Result<()> {
    // Threads whose first post is hidden are not listed; after unhiding they go back to 0. Look up
    // the keys first and then update by key (one statement for both backends).
    for (keys, displayorder) in [
        (
            "SELECT t.tid FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid = t.tid WHERE p.invisible <> 0 AND t.displayorder <> -1",
            "UPDATE gonof_forum_thread SET displayorder = -1, cache_revision = cache_revision + 1 WHERE tid IN (",
        ),
        (
            "SELECT t.tid FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid = t.tid WHERE p.invisible = 0 AND t.displayorder = -1",
            "UPDATE gonof_forum_thread SET displayorder = 0, cache_revision = cache_revision + 1 WHERE tid IN (",
        ),
    ] {
        let tids: Vec<i64> = db::query_scalar(keys)
            .fetch_all(db.observe(&mut **tx))
            .await?;
        update_keys(db, tx, displayorder, |query| query, &tids).await?;
    }
    // The parent floor copy of nested replies: first fetch (floor, the parent floor's hidden value), then update
    // grouped by value.
    let parents = db::query(
        "SELECT p.pid, parent.invisible FROM gonof_forum_post p JOIN gonof_forum_post parent ON parent.pid = p.root_pid WHERE p.root_invisible <> parent.invisible",
    )
    .fetch_all(db.observe(&mut **tx))
    .await?;
    let parents = parents
        .iter()
        .map(|row| Ok((row.try_get(0)?, vec![row.try_get(1)?])))
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    update_by_value(
        db,
        tx,
        "UPDATE gonof_forum_post SET root_invisible = ? WHERE pid IN (",
        parents,
    )
    .await?;
    for query in [
        // Reserved states (unpublished, private, read-permission-restricted) are not listed.
        db::query(
            "UPDATE gonof_forum_thread SET displayorder = -2, cache_revision = cache_revision + 1 WHERE displayorder >= 0 AND (publication_state <> 'published' OR visibility NOT IN ('public','members') OR readperm <> 0)",
        ),
        db::query(
            "UPDATE gonof_forum_thread SET displayorder = 0, cache_revision = cache_revision + 1 WHERE displayorder = -2 AND publication_state = 'published' AND visibility IN ('public','members') AND readperm = 0",
        ),
        db::query(
            "UPDATE gonof_forum_post SET root_invisible = 0 WHERE root_pid IS NULL AND root_invisible <> 0",
        ),
        db::query("DELETE FROM gonof_forum_thread_count"),
        db::query(
            "INSERT INTO gonof_forum_thread_count(fid,visibility,category,threads) SELECT fid,visibility,category,COUNT(*) FROM gonof_forum_thread WHERE displayorder >= 0 GROUP BY fid,visibility,category",
        ),
        db::query("DELETE FROM gonof_common_member_count"),
        db::query(
            "INSERT INTO gonof_common_member_count(uid,threads,replies,digests) SELECT p.authorid,SUM(p.first = 1),SUM(p.first = 0),SUM(p.first = 1 AND t.digest > 0) FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid WHERE p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0 GROUP BY p.authorid",
        ),
    ] {
        query.execute(db.observe(&mut **tx)).await?;
    }
    bump_site(db, tx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(visibility: ForumVisibility, enabled: bool) -> ForumScope {
        ForumScope {
            visibility,
            enabled,
        }
    }

    #[test]
    fn visibility_parses_exactly() {
        for kind in ForumVisibility::ALL {
            assert_eq!(ForumVisibility::parse(kind.value()), Some(kind));
        }
        for bad in ["", "Public", "public ", " members", "invalid"] {
            assert_eq!(ForumVisibility::parse(bad), None, "{bad:?}");
        }
        assert!(
            ForumScope::own("invalid", 1).is_err(),
            "an unknown visibility must not count as public"
        );
    }

    #[test]
    fn scope_takes_the_stricter_of_forum_and_parent() {
        use ForumVisibility::*;
        for own in ForumVisibility::ALL {
            assert_eq!(scope(own, true).inherit(None), scope(own, true));
            for parent in ForumVisibility::ALL {
                for (own_on, parent_on) in
                    [(true, true), (true, false), (false, true), (false, false)]
                {
                    let inherited = scope(own, own_on).inherit(Some(scope(parent, parent_on)));
                    let expected = if own == Private || parent == Private {
                        Private
                    } else if own == Members || parent == Members {
                        Members
                    } else {
                        Public
                    };
                    assert_eq!(inherited.visibility, expected, "{own:?} under {parent:?}");
                    assert_eq!(inherited.enabled, own_on && parent_on);
                }
            }
        }
        assert!(!ForumScope::own("public", 0).unwrap().enabled);
        assert!(
            !ForumScope::own("public", 2).unwrap().enabled,
            "only 1 means enabled"
        );
    }

    #[test]
    fn deleted_accounts_show_one_name() {
        assert_eq!(
            last_reply_name(Some("Alice".into()), Some("active")),
            "Alice"
        );
        assert_eq!(
            last_reply_name(Some("Alice".into()), Some("deleting")),
            "Alice"
        );
        assert_eq!(
            last_reply_name(Some("deleted-7-abc".into()), Some("deleted")),
            DELETED_AUTHOR
        );
        assert_eq!(last_reply_name(None, None), DELETED_AUTHOR);
    }

    #[test]
    fn the_last_replier_uid_is_dropped_with_the_name() {
        assert_eq!(last_reply_uid(Some(7), Some("active")), Some(7));
        assert_eq!(last_reply_uid(Some(7), Some("deleting")), Some(7));
        assert_eq!(last_reply_uid(Some(7), Some("deleted")), None);
        // No member row: the reply has no author, like `last_reply_name` showing "Deleted user".
        assert_eq!(last_reply_uid(Some(7), None), None);
        assert_eq!(last_reply_uid(None, None), None);
    }
}
