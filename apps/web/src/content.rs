//! Public content in the database and the page models that reuse the existing templates.
use crate::db::DbRow;
use crate::db::Tx;
pub mod antispam;
pub mod api_keys;
pub mod campaigns;
pub mod cards;
pub mod copies;
pub mod dataset;
pub mod edits;
pub mod feed_tokens;
pub mod feeds;
pub mod home;
pub mod members;
pub mod pins;
pub mod profiles;
pub mod sample;
mod seed;
pub mod sources;
pub mod thread_images;
pub mod views;
pub mod writes;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    db::{Database, text as decode_text},
    urls::Sort,
};

pub use seed::{
    ImportedMember, import_dataset, import_fixture, import_sample, sample_fits, seed_fixture,
    seed_forums,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Thread {
    pub id: u64,
    /// Only for the development fixture (the `slug` in `tests/fixtures/forum.json`): the URL of the thread's forum.
    /// Threads read from the database do not have it (the thread row carries no forum copies); the forum is taken
    /// from the forum cache by `fid`.
    #[serde(rename = "slug", default, skip_serializing)]
    pub fixture_forum: String,
    pub title: String,
    pub author: String,
    pub avatar: usize,
    #[serde(default)]
    pub avatar_aid: i64,
    #[serde(default)]
    pub last_reply_author: String,
    /// The last live reply; the list title "Jump to the latest reply" (reading preference) links to its permalink.
    #[serde(default)]
    pub last_reply_pid: Option<i64>,
    /// The last replier's uid (`last_reply_uid`): `None` without a reply or for a deleted account.
    #[serde(default)]
    pub last_reply_uid: Option<i64>,
    /// The first 255 characters of the last reply's Markdown source (`last_reply_summary`); data sources show it as
    /// plain text.
    #[serde(default)]
    pub last_reply_summary: String,
    /// The pin level, 0 to 3 (`displayorder` of a listed thread).
    #[serde(default)]
    pub pin: i64,
    /// The highest floor number (`maxposition`).
    #[serde(default)]
    pub max_position: i64,
    pub created: u64,
    pub updated: u64,
    pub time: String,
    pub created_time: String,
    pub views: usize,
    pub pinned: bool,
    pub featured: bool,
    pub body: Vec<String>,
    pub posts: Vec<Post>,
    #[serde(default)]
    pub reply_count: usize,
    #[serde(default)]
    pub root_replies: usize,
    #[serde(default)]
    pub category: i64,
    #[serde(default)]
    pub cache_revision: i64,
    #[serde(default)]
    pub first_pid: i64,
    #[serde(default)]
    pub fid: i64,
    #[serde(default)]
    pub authorid: Option<i64>,
    #[serde(default)]
    pub author_deleted: bool,
    /// The author's account is deleted, also known from a list row's author copy (the "Deleted user" marker that
    /// account deletion writes): list avatars, profile links and records show no uid for such an author. Unlike
    /// `author_deleted`, it is known on list rows too.
    #[serde(default)]
    pub author_gone: bool,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub first_version: i64,
    #[serde(default)]
    pub withdrawn: bool,
    #[serde(default)]
    pub erased: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub edited_at: i64,
    /// The thread or its forum (after inheritance) is members-only. List rows only know the thread's own visibility;
    /// the detail model adds the forum's, which the router fills from the forum cache.
    #[serde(default)]
    pub members_only: bool,
    /// Counters (docs/queries.md, "Counter columns"): favorites and the tip count are read by lists too; the tip
    /// total and the first post's downvotes only by the detail model, always 0 in list rows.
    #[serde(default)]
    pub favorites: i64,
    #[serde(default)]
    pub tips: i64,
    #[serde(default)]
    pub tip_total: i64,
    #[serde(default)]
    pub downvotes: i64,
    /// The original poster is frozen or not in normal status: the thread page then shows no tip button (the tip
    /// transaction checks again). Only set in the detail model.
    #[serde(default)]
    pub author_unavailable: bool,
    /// List thumbnails (`gonof_forum_thread_image`): static images in the first post in order of appearance, at most
    /// [`thread_images::MAX_THREAD_IMAGES`]; how many are shown is cut by the display settings. Only set in thread
    /// lists and goes into the anonymous cache with the list; editing the first post advances the site-wide version,
    /// so the cache key changes with it.
    #[serde(default)]
    pub thumbnails: Vec<thread_images::Thumbnail>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Post {
    pub pid: u64,
    pub position: usize,
    #[serde(default)]
    pub root_position: usize,
    #[serde(default)]
    pub sub_position: usize,
    #[serde(default)]
    pub sub_replies_count: usize,
    #[serde(default)]
    pub root_floor: usize,
    #[serde(default)]
    pub parent_label: String,
    pub root_pid: Option<u64>,
    pub reply_to_pid: Option<u64>,
    pub author: String,
    pub avatar: usize,
    #[serde(default)]
    pub avatar_aid: i64,
    pub body: String,
    pub time: String,
    pub dateline: u64,
    #[serde(default)]
    pub authorid: Option<i64>,
    #[serde(default)]
    pub author_deleted: bool,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub withdrawn: bool,
    #[serde(default)]
    pub erased: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub edited_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadPage {
    pub threads: Vec<Thread>,
    pub total: usize,
    /// Read from MySQL when the current request builds the list key; the home page's newest member reuses this
    /// read. The version must not come from Redis.
    #[serde(skip)]
    pub(crate) site_revision: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Overview {
    pub hot: Vec<Thread>,
    pub latest: Vec<Thread>,
    pub total: usize,
    pub replies: usize,
    pub contributors: usize,
}

/// Imported explicitly only by the development CLI and tests; Web startup and queries never read this file.
pub fn fixtures() -> Vec<Thread> {
    let mut threads: Vec<Thread> =
        serde_json::from_str(include_str!("../../../tests/fixtures/forum.json"))
            .expect("the versioned synthetic data must be valid");
    for thread in &mut threads {
        thread.reply_count = thread.posts.len();
        let mut root_ordinals = std::collections::HashMap::new();
        let mut sub_counts = std::collections::HashMap::new();
        let mut roots = 0;
        for post in &mut thread.posts {
            if let Some(root_pid) = post.root_pid {
                let count = sub_counts.entry(root_pid).or_insert(0);
                *count += 1;
                post.sub_position = *count;
            } else {
                roots += 1;
                post.root_position = roots;
                root_ordinals.insert(post.pid, (roots, post.position));
            }
        }
        thread.root_replies = roots;
        for post in &mut thread.posts {
            if let Some(root) = post.root_pid {
                (post.root_position, post.root_floor) = root_ordinals[&root];
            } else {
                post.sub_replies_count = *sub_counts.get(&post.pid).unwrap_or(&0);
            }
        }
    }
    threads
}

// status is the original bit mask and closed says whether replies are still allowed; neither is turned into a
// public visibility enum. Forum, thread and first-post permissions use the same condition in every public query.
pub(crate) const PUBLIC_THREAD: &str = "f.status = 1 AND f.visibility = 'public'
    AND t.displayorder >= 0 AND t.readperm = 0 AND t.visibility = 'public'
    AND t.publication_state = 'published' AND firstpost.invisible = 0
    AND (f.parent_fid IS NULL OR EXISTS(SELECT 1 FROM gonof_forum_forum ancestor WHERE ancestor.fid=f.parent_fid AND ancestor.status=1 AND ancestor.visibility='public'))";

/// A Some UID has already passed account authentication; membership is not inferred from a parameter.
pub fn readable_thread_sql(viewer_uid: Option<i64>) -> String {
    let visibility = if viewer_uid.is_some() {
        "IN ('public','members')"
    } else {
        "='public'"
    };
    format!(
        "{} AND t.displayorder>=0 AND t.readperm=0 AND t.visibility {visibility} AND t.publication_state='published' AND firstpost.invisible=0",
        crate::organization::forum_readable_sql("f", viewer_uid)
    )
}
pub use crate::interaction::forum_filter::excluded_sql;

/// Which forums this visitor can read (a forum's enabled state and inherited visibility come from the process forum
/// cache; the thread row carries no forum copies).
#[derive(Clone, Copy, Debug, Default)]
pub struct Readable<'a> {
    /// A member who passed account authentication; `None` is anonymous and sees only public threads. Member
    /// identity is never inferred from parameters.
    pub uid: Option<i64>,
    /// Forum ids this visitor can read: pages take the forum cache from the page frame (ignoring the multi-domain
    /// site scope); entry points without a page frame read the forum table through [`anonymous_forums`] or
    /// `organization::forums`. Disabled, private and unreadable members-only forums, and child forums whose
    /// parent forum is unreadable, are not included. When empty, nothing can be read.
    pub forums: &'a [i64],
}

/// The thread readability condition on read paths: this visitor can read the thread's forum
/// ([`Readable::forums`]), the thread can be listed (threads whose first post is hidden by moderation,
/// unpublished, private or read-restricted have a negative `displayorder`), and the thread's own
/// visibility matches. The forum table is not joined. `PUBLIC_THREAD` and `readable_thread_sql` above are kept only
/// for statements that recheck under a lock on write paths. Forum ids come from the cache or the forum table and
/// are integers, written straight into the SQL (as in [`excluded_sql`]).
pub fn thread_visible_sql(readable: Readable<'_>) -> String {
    format!(
        "t.fid IN ({}) AND t.displayorder >= 0 AND t.visibility {}",
        fid_list(readable.forums),
        if readable.uid.is_some() {
            "IN ('public','members')"
        } else {
            "= 'public'"
        }
    )
}

/// Same as [`thread_visible_sql`], but forum readability is judged from the forum table already joined in the
/// statement (alias `forum`, parent forum included): for statements that only have a database handle and already
/// join the forum table (the per-row "can it be opened" flag in favorite, follow and notification lists, rechecks
/// under a lock in write transactions).
pub fn thread_visible_by_forum_sql(forum: &str, viewer_uid: Option<i64>) -> String {
    format!(
        "{} AND t.displayorder >= 0 AND t.visibility {}",
        crate::organization::forum_readable_sql(forum, viewer_uid),
        if viewer_uid.is_some() {
            "IN ('public','members')"
        } else {
            "= 'public'"
        }
    )
}

/// Forum name by `fid` from the forum cache; empty for forums the reader cannot read (not in the cache).
pub fn forum_name(forums: &[crate::organization::ForumRecord], fid: i64) -> String {
    forums
        .iter()
        .find(|forum| forum.fid == fid)
        .map(|forum| forum.name.clone())
        .unwrap_or_default()
}

/// Same as [`forum_name`], for the forum URL.
pub fn forum_slug(forums: &[crate::organization::ForumRecord], fid: i64) -> String {
    forums
        .iter()
        .find(|forum| forum.fid == fid)
        .map(|forum| forum.slug.clone())
        .unwrap_or_default()
}

/// Threads anonymous visitors can read (the anonymous form of [`thread_visible_sql`]); `forums` are the forum ids
/// anonymous visitors can read.
pub(crate) fn public_threads(forums: &[i64]) -> String {
    thread_visible_sql(Readable { uid: None, forums })
}

/// Forum ids inside `IN (…)`; an empty list writes the nonexistent forum id 0, so the condition is always false.
fn fid_list(fids: &[i64]) -> String {
    if fids.is_empty() {
        "0".into()
    } else {
        join_ids(fids)
    }
}

/// The post visibility condition on read paths: the post itself is not hidden, withdrawn or erased, and a nested
/// reply's parent post is not hidden (`root_invisible`, so the parent post is not looked up). Withdrawing the first
/// post and withdrawing the thread are always written in the same transaction, so the first post's own `deleted_at`
/// is enough.
pub(crate) fn post_visible(alias: &str) -> String {
    format!(
        "{alias}.invisible = 0 AND {alias}.deleted_at = 0 AND {alias}.erased_at = 0 AND {alias}.root_invisible = 0"
    )
}

/// Block condition (read paths): the author id is not in the reader's block list ([`blocked_authors`], read
/// once per page). An empty string when the list is empty.
pub(crate) fn not_blocked(column: &str, blocked: &[i64]) -> String {
    if blocked.is_empty() {
        return String::new();
    }
    format!(
        " AND ({column} IS NULL OR {column} NOT IN ({}))",
        join_ids(blocked)
    )
}

/// List condition: keep only forums in `fids` (the multi-domain site scope); returns an empty string when empty,
/// so the SQL text is the same as without a scope.
pub fn scope_sql(column: &str, fids: &[i64]) -> String {
    if fids.is_empty() {
        return String::new();
    }
    let list = fids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(" AND {column} IN ({list})")
}

pub fn unblocked_sql(viewer_uid: Option<i64>, author: &str) -> String {
    viewer_uid.map_or_else(|| "1=1".into(), |uid| format!("NOT EXISTS(SELECT 1 FROM gonof_home_member_block blocked WHERE blocked.uid={uid} AND blocked.blocked_uid={author})"))
}

const THREAD_JOINS: &str = "FROM gonof_forum_thread t
    JOIN gonof_forum_post firstpost ON firstpost.first_tid = t.tid
    LEFT JOIN gonof_common_member author ON author.uid = t.authorid";

/// Withdrawal only masks the post itself; only moderation-hiding a parent post blocks its child replies.
/// Withdrawing the first post also checks the thread status.
pub(crate) fn public_post_body(alias: &str) -> String {
    format!(
        "{alias}.invisible = 0 AND {alias}.deleted_at = 0 AND {alias}.erased_at=0
        AND ({alias}.first = 0 OR (t.deleted_at = 0 AND firstpost.deleted_at = 0))"
    )
}

pub(crate) fn public_post(alias: &str) -> String {
    format!(
        "{} AND ({alias}.root_pid IS NULL OR EXISTS (
            SELECT 1 FROM gonof_forum_post parent
            WHERE parent.pid = {alias}.root_pid AND parent.tid = {alias}.tid
              AND parent.first = 0 AND parent.root_pid IS NULL
              AND parent.invisible = 0))",
        public_post_body(alias)
    )
}

fn thread_select() -> String {
    format!(
        "SELECT t.tid, t.fid, t.authorid, t.version, t.closed, t.category, t.root_replies, t.cache_revision,
            firstpost.version AS first_version, firstpost.edited_at, firstpost.erased_at,
            t.deleted_at, firstpost.deleted_at AS first_deleted_at,
            t.subject,
            author.username AS author, author.account_state AS author_state, author.avatar, author.avatar_aid, author.freeze AS author_freeze,
            t.dateline, t.lastpost, t.last_reply_author, t.last_reply_pid, t.last_reply_uid, t.last_reply_summary,
            t.views, t.displayorder, t.digest, firstpost.message, t.maxposition,
            firstpost.pid AS first_pid, firstpost.downvotes AS first_downvotes, t.favorites, t.tips, t.tip_total,
            t.replies AS reply_count,t.visibility
        {THREAD_JOINS}"
    )
}

/// Public forum fields (name, URL, description, posting guide).
#[derive(Clone, Debug)]
pub struct Forum {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub guidance: String,
}

pub async fn forums(db: &Database) -> Result<Vec<Forum>> {
    forums_for_viewer(db, None).await
}
pub async fn forums_for_viewer(db: &Database, viewer_uid: Option<i64>) -> Result<Vec<Forum>> {
    Ok(crate::organization::forums(db, viewer_uid)
        .await?
        .into_iter()
        .map(|f| Forum {
            name: f.name,
            slug: f.slug,
            description: f.description,
            guidance: f.guidance,
        })
        .collect())
}

/// Threads anonymous visitors can see: `displayorder >= 0` (negative means not listed: the first post is hidden by
/// moderation, or a reserved state) and the thread is public. Forum-level readability is not here: lists exclude by
/// the forum ids the caller gives ([`ListViewer::forums`]), and reading a single thread adds `t.fid IN (readable
/// forums)` ([`thread_visible_sql`]). Feeds and forum statistics use it too.
pub const LIST_PUBLIC: &str = "t.displayorder >= 0 AND t.visibility = 'public'";

/// At most this many list pages: the total is counted up
/// to at most `MAX_LIST_PAGES × page size`, and later pages are 404. Deep pagination therefore has a bounded read.
pub const MAX_LIST_PAGES: usize = 1000;

/// The forum scope of lists, the sidebar and feeds. Which forums in the scope this visitor can see is given
/// separately by [`ListViewer::forums`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListScope {
    /// Site-wide: the home page, search, and the user center's sidebar (narrowed by `forums` to the multi-domain
    /// site scope).
    All,
    /// A top-level forum together with its child forums (the thread row's `family_fid`): the top-level forum page.
    Family(i64),
    /// A single forum without its child forums: the child forum page.
    Forum(i64),
}

impl ListScope {
    /// The scope of a forum page: a top-level forum with its child forums; a child forum just itself.
    pub fn of_forum(fid: i64, parent_fid: Option<i64>) -> Self {
        match parent_fid {
            None => Self::Family(fid),
            Some(_) => Self::Forum(fid),
        }
    }

    /// The scope segment of the list index names (with a category there is a separate set `*_category_*`).
    fn index(&self, category: bool) -> &'static str {
        match (self, category) {
            (Self::All, false) => "global",
            (Self::All, true) => "category",
            (Self::Family(_), false) => "family",
            (Self::Family(_), true) => "family_category",
            (Self::Forum(_), false) => "forum",
            (Self::Forum(_), true) => "forum_category",
        }
    }

    /// The scope's equality condition (the index's leading column) and its parameters.
    fn condition(&self, args: &mut Vec<Arg>) -> &'static str {
        match self {
            Self::All => "",
            Self::Family(fid) => {
                args.push(Arg::Int(*fid));
                " AND t.family_fid = ?"
            }
            Self::Forum(fid) => {
                args.push(Arg::Int(*fid));
                " AND t.fid = ?"
            }
        }
    }
}

/// This visitor's list filter: which forums they can see and whom they have blocked.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListViewer<'a> {
    /// A member who passed account authentication; `None` is anonymous and sees only public threads. Member
    /// identity is never inferred from parameters.
    pub uid: Option<i64>,
    /// Forum ids this visitor can see in this scope: the router computes them in code from the process forum cache,
    /// already without disabled, private and
    /// anonymous-unreadable members-only forums, forums outside the multi-domain site scope, and forums the member
    /// hid or blocked. When the scope is a top-level forum, only that family's forums are included (the total is
    /// read from the counter table by this list). When empty, nothing is listed.
    pub forums: &'a [i64],
    /// Members this member has blocked ([`blocked_authors`]); their threads are not listed.
    pub blocked_uids: &'a [i64],
}

impl ListViewer<'_> {
    /// This visitor's filter condition: anonymous visitors see only public threads; forum id, thread visibility and
    /// author id are all at the end of the list indexes, so they are judged from the index without a table lookup.
    /// Both the forum and author lists are integers read from the database and are written straight into the SQL
    /// (as in [`excluded_sql`]). When `scope` is a single forum, the forum is not restricted again.
    fn filter(&self, scope: &ListScope) -> String {
        let mut sql = String::new();
        if self.uid.is_none() {
            sql.push_str(" AND t.visibility = 'public'");
        }
        if !matches!(scope, ListScope::Forum(_)) {
            sql.push_str(&format!(" AND t.fid IN ({})", join_ids(self.forums)));
        }
        if !self.blocked_uids.is_empty() {
            // The author id is nullable: threads without an author are not affected by blocks.
            sql.push_str(&format!(
                " AND (t.authorid IS NULL OR t.authorid NOT IN ({}))",
                join_ids(self.blocked_uids)
            ));
        }
        sql
    }

    /// No forum in the scope is visible: no SQL is sent and the result is simply empty.
    fn sees_nothing(&self, scope: &ListScope) -> bool {
        self.forums.is_empty() && !matches!(scope, ListScope::Forum(_))
    }
}

fn join_ids(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Members this member has blocked, by member id ascending. Read once per page and shared by lists, counts, the
/// sidebar, the pinned block and the personal space.
pub async fn blocked_authors(db: &Database, uid: i64) -> Result<Vec<i64>> {
    Ok(crate::db::query_scalar(
        "SELECT blocked_uid FROM gonof_home_member_block WHERE uid=? ORDER BY blocked_uid",
    )
    .bind(uid)
    .fetch_all(db.executor())
    .await?)
}

/// Placeholder parameters of a list SQL, collected in the order they appear in the SQL.
#[derive(Clone, Debug)]
enum Arg {
    Int(i64),
    Text(String),
}

fn bind_rows<'q>(mut query: crate::db::Query<'q>, args: &'q [Arg]) -> crate::db::Query<'q> {
    for arg in args {
        query = match arg {
            Arg::Int(value) => query.bind(*value),
            Arg::Text(value) => query.bind(value.as_str()),
        };
    }
    query
}

fn bind_scalar<'q>(
    mut query: crate::db::QueryScalar<'q, i64>,
    args: &'q [Arg],
) -> crate::db::QueryScalar<'q, i64> {
    for arg in args {
        query = match arg {
            Arg::Int(value) => query.bind(*value),
            Arg::Text(value) => query.bind(value.as_str()),
        };
    }
    query
}

/// Parameters of the sidebar's hot and latest cards, from the admin area's "Appearance › Display"
/// (`settings.display.hot_days`, `hot_count`, `latest_count`). The hot-days cap of
/// 30 and the count cap of 20 are hard limits already validated when decoding, so the queries stay bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SidebarParams {
    pub hot_days: i64,
    pub hot_count: i64,
    pub latest_count: i64,
    /// Site time zone: this week's contributors count from Monday 00:00 in the site time zone.
    pub offset_minutes: i64,
}

impl SidebarParams {
    pub fn from_settings(settings: &crate::runtime::SiteSettings) -> Self {
        Self {
            hot_days: settings.display.hot_days,
            hot_count: settings.display.hot_count,
            latest_count: settings.display.latest_count,
            offset_minutes: settings.time.offset_minutes,
        }
    }

    /// Sidebar hot threads only consider threads with a new reply within this period.
    fn hot_since(self, now: i64) -> i64 {
        now.saturating_sub(self.hot_days.saturating_mul(86_400))
    }
}

/// Sidebar hot threads: threads with a new reply in the last N days, by reply count descending, with `tid` as the
/// final sort key. A single-table query: `gonof_thread_global_active` first takes those days' range by
/// `lastpost`; pin level, reply count, visibility, top-level forum id, forum id and author id are all in the index
/// and filtered in place; then it sorts by reply count. The read grows only with the number of threads active in
/// that period. A forum page's right column filters by scope.
async fn hot_topics(
    db: &Database,
    viewer: &ListViewer<'_>,
    scope: &ListScope,
    count: i64,
    since: i64,
) -> Result<Vec<Thread>> {
    if viewer.sees_nothing(scope) {
        return Ok(Vec::new());
    }
    let mut args = vec![Arg::Int(since)];
    let scope_sql = scope.condition(&mut args);
    let sql = format!(
        "{} WHERE t.lastpost >= ? AND t.displayorder >= 0 AND t.replies > 0{scope_sql}{} ORDER BY t.replies DESC,t.tid DESC LIMIT {count}",
        list_select("gonof_thread_global_active"),
        viewer.filter(scope)
    );
    bind_rows(crate::db::query(&sql), &args)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(decode_list_thread)
        .collect()
}

/// Columns of a list row: only the thread row is read, no other table is joined. The author copy of a deleted
/// account was already written as "Deleted user" at deletion; forum name and URL come from the process forum cache
/// by `fid`, and avatars get a URL by uid.
const LIST_COLUMNS: &str =
    "t.tid,t.fid,t.authorid,t.version,t.closed,t.category,t.root_replies,t.cache_revision,
        t.deleted_at,t.subject,t.author,
        t.dateline,t.lastpost,t.last_reply_author,t.last_reply_pid,t.last_reply_uid,t.last_reply_summary,
        t.views,t.displayorder,t.digest,t.maxposition,t.favorites,t.tips,t.visibility,
        t.replies AS reply_count";

/// Take whole rows straight from the index (the pinned block takes them by primary key).
fn list_select(index: &str) -> String {
    format!("SELECT {LIST_COLUMNS} FROM gonof_forum_thread t FORCE INDEX ({index})")
}

pub async fn list_threads(
    db: &Database,
    slug: &str,
    sort: Sort,
    search: Option<&str>,
    page: usize,
    page_size: usize,
) -> Result<ThreadPage> {
    list_threads_category(db, slug, sort, search, None, page, page_size).await
}

pub async fn list_threads_category(
    db: &Database,
    slug: &str,
    sort: Sort,
    search: Option<&str>,
    category: Option<i64>,
    page: usize,
    page_size: usize,
) -> Result<ThreadPage> {
    list_threads_for_viewer(db, slug, sort, search, category, page, page_size, None, &[]).await
}

/// One page by forum URL (for tests and tools; pages call [`list_scope_for_viewer`] with a [`ListScope`]): an
/// empty `slug` takes the forums this visitor can read site-wide, otherwise only that forum itself without child
/// forums; an unknown URL gives an empty page. The signed-in view also reads the members this member has blocked.
/// `excluded_fids` are the forums the member hid or blocked (child forums expanded), non-empty only in the signed-in
/// view.
#[allow(clippy::too_many_arguments)]
pub async fn list_threads_for_viewer(
    db: &Database,
    slug: &str,
    sort: Sort,
    search: Option<&str>,
    category: Option<i64>,
    page: usize,
    page_size: usize,
    viewer_uid: Option<i64>,
    excluded_fids: &[i64],
) -> Result<ThreadPage> {
    let readable = crate::organization::forums(db, viewer_uid).await?;
    let scope = if slug.is_empty() {
        ListScope::All
    } else {
        match readable.iter().find(|forum| forum.slug == slug) {
            Some(forum) => ListScope::Forum(forum.fid),
            None => {
                return Ok(ThreadPage {
                    threads: vec![],
                    total: 0,
                    site_revision: None,
                });
            }
        }
    };
    let forums: Vec<i64> = readable
        .iter()
        .map(|forum| forum.fid)
        .filter(|fid| !excluded_fids.contains(fid))
        .collect();
    let blocked = match viewer_uid {
        Some(uid) => blocked_authors(db, uid).await?,
        None => Vec::new(),
    };
    let viewer = ListViewer {
        uid: viewer_uid,
        forums: &forums,
        blocked_uids: &blocked,
    };
    list_scope_for_viewer(db, &scope, sort, search, category, page, page_size, viewer).await
}

/// One page of threads and the total. The list is a single-table query that picks
/// the index by scope, category and sort: the scope's forum id and category are equalities, `displayorder >= 0` is
/// the range right after them, the sort is read along the index, and this visitor's forums, visibility and blocks
/// are filtered in place at the end of the index.
///
/// The total is only used to compute the last page and counts up to at most [`MAX_LIST_PAGES`] pages: latest
/// replies, latest threads and categories read the forum thread counter table,
/// summed over the forums this visitor can see, minus the threads of blocked members; the pinned and highlighted
/// sets are small and counted straight from the index; search cannot be counted that way and stays a capped count.
/// A page number past the last page returns an empty page, and the page gives a 404. When a count read here is 0 (an
/// exact figure, unlike the counter table's), the list statement is not run at all: an empty search would otherwise
/// match the pattern against the candidate titles a second time.
#[allow(clippy::too_many_arguments)]
pub async fn list_scope_for_viewer(
    db: &Database,
    scope: &ListScope,
    sort: Sort,
    search: Option<&str>,
    category: Option<i64>,
    page: usize,
    page_size: usize,
    viewer: ListViewer<'_>,
) -> Result<ThreadPage> {
    ensure!(
        page > 0 && page_size > 0 && page_size <= 100,
        "invalid page or page size"
    );
    ensure!(
        category.is_none_or(|n| (0..=5).contains(&n)),
        "invalid thread category"
    );
    let empty = || ThreadPage {
        threads: vec![],
        total: 0,
        site_revision: None,
    };
    if viewer.sees_nothing(scope) {
        return Ok(empty());
    }
    // Conditions and parameters after the scope condition, collected in the order they appear in the SQL.
    let mut args = Vec::new();
    let scope_sql = scope.condition(&mut args);
    let mut filter = String::from(scope_sql);
    filter.push_str(if sort == Sort::Pinned {
        " AND t.displayorder > 0"
    } else {
        " AND t.displayorder >= 0"
    });
    if sort == Sort::Featured {
        // Highlight is only 0 or 1 (moderation only writes these two values; others are reserved states): written
        // as an equality, so `gonof_thread_featured` is read in index order.
        filter.push_str(" AND t.digest = 1");
    }
    if let Some(category) = category {
        filter.push_str(" AND t.category = ?");
        args.push(Arg::Int(category));
    }
    let search = search.filter(|v| !v.is_empty()).map(search_pattern);
    if let Some(pattern) = &search {
        // Both sides go through LOWER, MySQL's mapping on both backends (`db::lower`); Rust's to_lowercase differs
        // from it on letters such as Σ, İ and ẞ.
        filter.push_str(" AND t.deleted_at = 0 AND LOWER(t.subject) LIKE LOWER(?) ESCAPE '!'");
        args.push(Arg::Text(pattern.clone()));
    }
    filter.push_str(&viewer.filter(scope));
    let list = ListQuery {
        scope,
        sort,
        category: category.is_some(),
    };
    let cap = MAX_LIST_PAGES
        .checked_mul(page_size)
        .context("page limit out of range")?;
    // Whether `total` was counted here from the rows themselves (rather than read from the counter table).
    let counted = search.is_some() || matches!(sort, Sort::Pinned | Sort::Featured);
    let total = if search.is_some() {
        // Search has no counter to read: a capped count, reading at most as many rows as the cap.
        let mut count_args = args.clone();
        count_args.push(Arg::Int(i64::try_from(cap)?));
        let sql = format!(
            "SELECT COUNT(*) FROM (SELECT 1 FROM gonof_forum_thread t FORCE INDEX ({}) WHERE 1=1{filter} LIMIT ?) capped",
            list.index()
        );
        bind_scalar(crate::db::query_scalar::<i64>(&sql), &count_args)
            .fetch_one(db.executor())
            .await?
    } else if matches!(sort, Sort::Pinned | Sort::Featured) {
        // Only a few threads are pinned or highlighted: count them straight from their own index ranges.
        let sql = format!(
            "SELECT COUNT(*) FROM gonof_forum_thread t FORCE INDEX ({}) WHERE 1=1{filter}",
            list.index()
        );
        bind_scalar(crate::db::query_scalar::<i64>(&sql), &args)
            .fetch_one(db.executor())
            .await?
    } else {
        counted_threads(db, scope, category, viewer).await?
    };
    let total = usize::try_from(total.max(0))?.min(cap);
    if (counted && total == 0) || page > total.div_ceil(page_size).max(1) {
        return Ok(ThreadPage { total, ..empty() });
    }
    let offset = (page - 1)
        .checked_mul(page_size)
        .context("page offset out of range")?;
    let mut threads = list_rows(db, &list, (&filter, &args), page_size, offset).await?;
    thread_images::attach(db, &mut threads).await?;
    Ok(ThreadPage {
        threads,
        total,
        site_revision: None,
    })
}

/// Number of listable threads: sum the slots of the forum thread counter table this visitor can see; when the member
/// has blocked others, also count those members' threads in the same scope by author id and subtract them. Both are
/// single-table queries.
async fn counted_threads(
    db: &Database,
    scope: &ListScope,
    category: Option<i64>,
    viewer: ListViewer<'_>,
) -> Result<i64> {
    let forums = match scope {
        ListScope::Forum(fid) => vec![*fid],
        _ => viewer.forums.to_vec(),
    };
    let mut sql = format!(
        "SELECT threads FROM gonof_forum_thread_count WHERE fid IN ({})",
        join_ids(&forums)
    );
    if viewer.uid.is_none() {
        sql.push_str(" AND visibility = 'public'");
    }
    if category.is_some() {
        sql.push_str(" AND category = ?");
    }
    let mut query = crate::db::query_scalar::<i64>(&sql);
    if let Some(category) = category {
        query = query.bind(category);
    }
    let mut total: i64 = query.fetch_all(db.executor()).await?.into_iter().sum();
    if !viewer.blocked_uids.is_empty() {
        let mut sql = format!(
            "SELECT COUNT(*) FROM gonof_forum_thread t FORCE INDEX (authorid) WHERE t.authorid IN ({}) AND t.displayorder >= 0 AND t.fid IN ({})",
            join_ids(viewer.blocked_uids),
            join_ids(&forums)
        );
        if viewer.uid.is_none() {
            sql.push_str(" AND t.visibility = 'public'");
        }
        if category.is_some() {
            sql.push_str(" AND t.category = ?");
        }
        let mut query = crate::db::query_scalar::<i64>(&sql);
        if let Some(category) = category {
            query = query.bind(category);
        }
        total -= query.fetch_one(db.executor()).await?;
    }
    Ok(total)
}

/// Which index a list query uses and which column it sorts by.
struct ListQuery<'a> {
    scope: &'a ListScope,
    sort: Sort,
    category: bool,
}

impl ListQuery<'_> {
    /// Highlights have their own index led by `digest`: `digest = 1` as an equality, then read in order of pin
    /// level and last reply time.
    fn index(&self) -> String {
        if self.sort == Sort::Featured {
            return "gonof_thread_featured".into();
        }
        format!(
            "gonof_thread_{}_{}",
            self.scope.index(self.category),
            if self.sort == Sort::Created {
                "created"
            } else {
                "latest"
            }
        )
    }

    /// Sort time column: latest threads by post time, all others by last reply time.
    fn field(&self) -> &'static str {
        if self.sort == Sort::Created {
            "dateline"
        } else {
            "lastpost"
        }
    }
}

/// One page of threads by pin level, time and tid descending: a single-table query, not split into segments and not
/// nested. `filter` and `args` are the conditions and parameters after `WHERE`.
async fn list_rows(
    db: &Database,
    list: &ListQuery<'_>,
    (filter, args): (&str, &[Arg]),
    limit: usize,
    offset: usize,
) -> Result<Vec<Thread>> {
    let field = list.field();
    let sql = format!(
        "{} WHERE 1=1{filter} ORDER BY t.displayorder DESC,t.{field} DESC,t.tid DESC LIMIT ? OFFSET ?",
        list_select(&list.index())
    );
    let mut args = args.to_vec();
    args.push(Arg::Int(i64::try_from(limit)?));
    args.push(Arg::Int(i64::try_from(offset)?));
    bind_rows(crate::db::query(&sql), &args)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(decode_list_thread)
        .collect()
}

/// The sidebar's "Latest threads": the first N by post time, just without counting the total. `scope` is the scope:
/// the home page and search are site-wide; a forum page is that forum (a parent forum includes its child forums).
async fn latest_topics(
    db: &Database,
    viewer: &ListViewer<'_>,
    count: i64,
    scope: &ListScope,
) -> Result<Vec<Thread>> {
    if viewer.sees_nothing(scope) {
        return Ok(Vec::new());
    }
    let mut args = Vec::new();
    let mut filter = String::from(scope.condition(&mut args));
    filter.push_str(" AND t.displayorder >= 0");
    filter.push_str(&viewer.filter(scope));
    list_rows(
        db,
        &ListQuery {
            scope,
            sort: Sort::Created,
            category: false,
        },
        (&filter, &args),
        usize::try_from(count).unwrap_or(0),
        0,
    )
    .await
}

/// Only reads the thread and its first post; replies must request the current page explicitly. Uses the anonymous
/// visitor's readable scope (reads the forum table once); for the admin area and tests.
pub async fn thread(db: &Database, id: u64) -> Result<Option<Thread>> {
    let forums = anonymous_forums(db).await?;
    thread_for_viewer(
        db,
        id,
        Readable {
            uid: None,
            forums: &forums,
        },
    )
    .await
}
pub async fn thread_for_viewer(
    db: &Database,
    id: u64,
    readable: Readable<'_>,
) -> Result<Option<Thread>> {
    let Ok(id) = i64::try_from(id) else {
        return Ok(None);
    };
    let sql = format!(
        "{} WHERE {} AND t.tid = ?",
        thread_select(),
        thread_visible_sql(readable)
    );
    crate::db::query(&sql)
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .map(decode_thread)
        .transpose()
}

fn post_select(index: &str) -> String {
    format!("SELECT p.pid,p.position,p.root_pid,p.reply_to_pid,p.dateline,p.authorid,
        p.root_position,parent.root_position AS parent_root_position,
        p.sub_position,p.sub_replies_count,parent.position AS root_floor,
        target.position AS target_position,target_member.username AS target_author,target_member.account_state AS target_author_state,
        p.version,p.edited_at,p.deleted_at,p.erased_at,p.invisible,parent.invisible AS parent_invisible,p.message,
        member.username AS author,member.account_state AS author_state,member.avatar,member.avatar_aid
        FROM gonof_forum_post p FORCE INDEX ({index})
        LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid
        LEFT JOIN gonof_forum_post target ON target.pid=p.reply_to_pid
        LEFT JOIN gonof_common_member target_member ON target_member.uid=target.authorid
        LEFT JOIN gonof_common_member member ON member.uid=p.authorid")
}

pub async fn load_replies(
    db: &Database,
    thread: &mut Thread,
    page: usize,
    flat: bool,
    sub_root: Option<u64>,
    sub_page: usize,
    paging: crate::urls::Paging,
) -> Result<()> {
    ensure!(page > 0 && sub_page > 0, "invalid page number");
    let per_page = i64::try_from(paging.replies)?;
    let start = i64::try_from(
        (page - 1)
            .checked_mul(paging.replies)
            .context("page number out of range")?,
    )? + if flat { 2 } else { 1 };
    let end = start
        .checked_add(per_page)
        .context("page number out of range")?;
    let (index, column) = if flat {
        ("PRIMARY", "position")
    } else {
        ("gonof_post_root_position", "root_position")
    };
    let sql = format!(
        "{} WHERE p.tid=? AND p.{column}>=? AND p.{column}<? ORDER BY p.{column}",
        post_select(index)
    );
    thread.posts = crate::db::query(&sql)
        .bind(i64::try_from(thread.id)?)
        .bind(start)
        .bind(end)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(decode_post)
        .collect::<Result<_>>()?;
    if !flat && !thread.posts.is_empty() {
        // MySQL 5.7 has no window functions: each top-level post takes a slice of its nested replies; the floor
        // number `sub_position` is contiguous within a top-level post, so "the first page" is
        // `sub_position < 1 + page size`. The first pages of all top-level posts are written as one
        // `root_pid IN (…)`, and the top-level post whose nested replies are being paged gets its own segment; all
        // are read as ranges on the `(root_pid, sub_position)` index, in one query without UNION.
        let per_page = i64::try_from(paging.sub_replies)?;
        let mut first_pages = Vec::new();
        let mut turned = None;
        for post in thread
            .posts
            .iter()
            .filter(|post| post.sub_replies_count > 0)
        {
            let root = i64::try_from(post.pid)?;
            match sub_root.filter(|pid| *pid == post.pid && sub_page > 1) {
                Some(_) => {
                    // A page whose first position cannot even be written holds no nested reply: nothing is read for
                    // it, and the thread page's page count check answers 404 instead of a database error.
                    let range = (sub_page - 1)
                        .checked_mul(paging.sub_replies)
                        .and_then(|skipped| i64::try_from(skipped).ok())
                        .and_then(|skipped| skipped.checked_add(1))
                        .and_then(|start| Some((start, start.checked_add(per_page)?)));
                    if let Some((start, end)) = range {
                        turned = Some((root, start, end));
                    }
                }
                None => first_pages.push(root),
            }
        }
        let mut ranges = Vec::new();
        if !first_pages.is_empty() {
            ranges.push(format!(
                "(p.root_pid IN ({}) AND p.sub_position>=1 AND p.sub_position<?)",
                vec!["?"; first_pages.len()].join(",")
            ));
        }
        if turned.is_some() {
            ranges.push("(p.root_pid=? AND p.sub_position>=? AND p.sub_position<?)".into());
        }
        if ranges.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "{} WHERE p.tid=? AND ({}) ORDER BY p.root_pid,p.sub_position",
            post_select("gonof_post_sub_position"),
            ranges.join(" OR ")
        );
        let mut query = crate::db::query(&sql).bind(i64::try_from(thread.id)?);
        if !first_pages.is_empty() {
            for root in &first_pages {
                query = query.bind(*root);
            }
            query = query.bind(1 + per_page);
        }
        if let Some((root, start, end)) = turned {
            query = query.bind(root).bind(start).bind(end);
        }
        let children = query
            .fetch_all(db.executor())
            .await?
            .into_iter()
            .map(decode_post)
            .collect::<Result<Vec<_>>>()?;
        thread.posts.extend(children);
    }
    Ok(())
}

/// Only reads the target's and the parent post's position fields; does not load the thread's other replies or
/// bodies.
pub async fn post_location(
    db: &Database,
    tid: u64,
    pid: i64,
    flat: bool,
    paging: crate::urls::Paging,
) -> Result<Option<String>> {
    let forums = anonymous_forums(db).await?;
    let readable = Readable {
        uid: None,
        forums: &forums,
    };
    post_location_for_viewer(db, tid, pid, flat, readable, paging).await
}
pub async fn post_location_for_viewer(
    db: &Database,
    tid: u64,
    pid: i64,
    flat: bool,
    readable: Readable<'_>,
    paging: crate::urls::Paging,
) -> Result<Option<String>> {
    let visibility = thread_visible_sql(readable);
    let sql = format!(
        "SELECT p.position,p.root_pid,p.root_position,parent.root_position AS parent_root_position,
        p.sub_position FROM gonof_forum_post p
        JOIN gonof_forum_thread t ON t.tid=p.tid
        LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid
        WHERE p.pid=? AND p.tid=? AND {visibility}"
    );
    let Some(row) = crate::db::query(&sql)
        .bind(pid)
        .bind(i64::try_from(tid)?)
        .fetch_optional(db.executor())
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(crate::urls::position_location(
        tid,
        usize::try_from(row.try_get::<i64, _>("position")?)?,
        row.try_get::<Option<i64>, _>("root_pid")?.map(|v| v as u64),
        root_position(&row)?,
        usize::try_from(row.try_get::<Option<i64>, _>("sub_position")?.unwrap_or(0))?,
        flat,
        paging,
    )))
}

pub async fn sidebar(db: &Database, params: SidebarParams) -> Result<Overview> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    sidebar_at(db, now, params).await
}

/// This week's contributors: distinct authors of public live posts from Monday 00:00 in the site time zone up to
/// now. The explicit time entry point keeps database behavior tests independent of the run date.
pub async fn sidebar_at(db: &Database, now: u64, params: SidebarParams) -> Result<Overview> {
    let forums = anonymous_forums(db).await?;
    let anonymous = ListViewer {
        forums: &forums,
        ..ListViewer::default()
    };
    let (hot, latest) =
        sidebar_topics(db, i64::try_from(now)?, params, &anonymous, &ListScope::All).await?;
    let statistics = statistics_at(db, now, params.offset_minutes).await?;
    Ok(Overview {
        hot,
        latest,
        total: statistics.total,
        replies: statistics.replies,
        contributors: statistics.contributors,
    })
}

/// All forum ids anonymous visitors can read (ignoring the multi-domain site scope). Pages take them from the
/// page frame's forum cache; this is for tests and entry points without a page frame.
pub async fn anonymous_forums(db: &Database) -> Result<Vec<i64>> {
    readable_forums(db, None).await
}

/// Same as [`anonymous_forums`], read for a member (`Some` has passed account authentication): reads the forum
/// table once. For tests, the member API, background tasks and other entry points without a web page frame, to
/// fill [`Readable::forums`].
pub async fn readable_forums(db: &Database, viewer_uid: Option<i64>) -> Result<Vec<i64>> {
    Ok(crate::organization::forums(db, viewer_uid)
        .await?
        .into_iter()
        .map(|forum| forum.fid)
        .collect())
}

/// The sidebar's hot and latest cards. `scope` is the scope: the home page and search are site-wide (or the
/// multi-domain site scope); a forum page is that forum (a parent forum includes its child forums).
async fn sidebar_topics(
    db: &Database,
    now: i64,
    params: SidebarParams,
    viewer: &ListViewer<'_>,
    scope: &ListScope,
) -> Result<(Vec<Thread>, Vec<Thread>)> {
    let hot = hot_topics(db, viewer, scope, params.hot_count, params.hot_since(now)).await?;
    Ok((
        hot,
        latest_topics(db, viewer, params.latest_count, scope).await?,
    ))
}

/// The sidebar's three site-wide statistics.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Statistics {
    pub total: usize,
    pub replies: usize,
    pub contributors: usize,
}

/// All three aggregates scan the public posts, so they can only be recomputed rarely; see
/// [`cached_statistics`]. This week's contributors count from Monday 00:00 in the site time zone
/// (`offset_minutes`).
pub async fn statistics_at(db: &Database, now: u64, offset_minutes: i64) -> Result<Statistics> {
    statistics_in(db, now, offset_minutes, &[]).await
}

/// Same as [`statistics_at`], counting only forums in `fids` (the multi-domain site scope); site-wide when empty.
/// Forum ids come from the process forum cache and are integers, written straight into the SQL (as in
/// [`excluded_sql`]).
pub async fn statistics_in(
    db: &Database,
    now: u64,
    offset_minutes: i64,
    fids: &[i64],
) -> Result<Statistics> {
    // Read the anonymously readable forums from the forum table once (statistics have their own cache layer), then
    // intersect with the scope.
    let anonymous = anonymous_forums(db).await?;
    let public = public_threads(&anonymous);
    let scope = scope_sql("t.fid", fids);
    let reply_sql = format!(
        "SELECT COUNT(*) FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid
         WHERE {public}{scope} AND p.first = 0 AND {}",
        post_visible("p")
    );
    let replies = usize::try_from(
        crate::db::query_scalar::<i64>(&reply_sql)
            .fetch_one(db.executor())
            .await?,
    )?;
    let monday = crate::clock::week_start(i64::try_from(now)?, offset_minutes);
    let contributors_sql = &format!(
        "SELECT COUNT(DISTINCT p.authorid) FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid
         WHERE {public}{scope} AND {} AND p.dateline >= ? AND p.dateline <= ?",
        post_visible("p")
    );
    let contributors = usize::try_from(
        crate::db::query_scalar::<i64>(contributors_sql)
            .bind(monday)
            .bind(i64::try_from(now)?)
            .fetch_one(db.executor())
            .await?,
    )?;
    let total = usize::try_from(
        crate::db::query_scalar::<i64>(&format!(
            "SELECT COUNT(*) FROM gonof_forum_thread t WHERE {public}{scope} AND t.deleted_at = 0"
        ))
        .fetch_one(db.executor())
        .await?,
    )?;
    Ok(Statistics {
        total,
        replies,
        contributors,
    })
}

/// Statistics of one forum (or a parent forum together with its child forums).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ForumCount {
    /// Thread count: same definition as "Threads" in the site-wide statistics (threads withdrawn by the author do not
    /// count).
    pub threads: i64,
    /// Posts today: threads and replies (nested replies included) posted since 00:00 today in the site time zone.
    pub today: i64,
}

impl std::ops::AddAssign for ForumCount {
    fn add_assign(&mut self, other: Self) {
        self.threads += other.threads;
        self.today += other.today;
    }
}

/// Forum statistics: thread count and posts today grouped by forum URL (grouped by `fid`, mapped to URLs through
/// the forum cache), **counting only anonymously readable public content**, one copy shared by all
/// visitors. Members-only, disabled and admin-only forums have no counts.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ForumStatistics {
    pub forums: std::collections::BTreeMap<String, ForumCount>,
}

impl ForumStatistics {
    /// Total over several forums: a parent forum passes its own URL and those of all its child forums (a parent
    /// forum's count includes its child forums).
    pub fn total<'a>(&self, slugs: impl IntoIterator<Item = &'a str>) -> ForumCount {
        let mut total = ForumCount::default();
        for slug in slugs {
            if let Some(count) = self.forums.get(slug) {
                total += *count;
            }
        }
        total
    }
}

/// Computes forum statistics on the spot with two aggregates grouped by forum:
///
/// - Thread count uses `gonof_thread_forum_latest`: the list scope equality and `displayorder >= 0` are filtered
///   in the index; excluding author withdrawals needs a table lookup for `deleted_at`; then it groups by the forum
///   URL copy (there are few forums, so the temporary table is small). The cost grows linearly with the total
///   number of public threads.
/// - Posts today use a range on `gonof_post_dateline`, reading only today's posts, then join the thread row by
///   primary key to check readability; same definition as the public posts of the site-wide statistics (posts
///   hidden by moderation, withdrawn or erased do not count, nor nested replies whose parent post is hidden).
///
/// Both are cached only by time segment ([`layered_forum_statistics`]) and do not follow the content version.
pub async fn forum_statistics_at(
    db: &Database,
    now: i64,
    offset_minutes: i64,
) -> Result<ForumStatistics> {
    // Group by fid, then map to URLs through the same forum cache data (anonymously readable
    // forums): the thread row has no forum URL.
    let readable = crate::organization::forums(db, None).await?;
    let fids: Vec<i64> = readable.iter().map(|forum| forum.fid).collect();
    let slug_of = |fid: i64| {
        readable
            .iter()
            .find(|forum| forum.fid == fid)
            .map(|forum| forum.slug.clone())
    };
    let public = public_threads(&fids);
    let mut forums = std::collections::BTreeMap::<String, ForumCount>::new();
    let threads = crate::db::query(&format!(
        "SELECT t.fid,COUNT(*) AS threads FROM gonof_forum_thread t FORCE INDEX (gonof_thread_forum_latest)
         WHERE {public} AND t.deleted_at = 0 GROUP BY t.fid"
    ))
    .fetch_all(db.executor())
    .await?;
    for row in threads {
        if let Some(slug) = slug_of(row.try_get("fid")?) {
            forums.entry(slug).or_default().threads = row.try_get("threads")?;
        }
    }
    let today = crate::db::query(&format!(
        "SELECT t.fid,COUNT(*) AS posts FROM gonof_forum_post p FORCE INDEX (gonof_post_dateline)
         JOIN gonof_forum_thread t ON t.tid = p.tid
         WHERE p.dateline >= ? AND p.dateline <= ? AND {public} AND {}
         GROUP BY t.fid",
        post_visible("p")
    ))
    .bind(crate::clock::day_start(now, offset_minutes))
    .bind(now)
    .fetch_all(db.executor())
    .await?;
    for row in today {
        if let Some(slug) = slug_of(row.try_get("fid")?) {
            forums.entry(slug).or_default().today = row.try_get("posts")?;
        }
    }
    Ok(ForumStatistics { forums })
}

/// The Redis layer of forum statistics: segmented by `settings.cache.forum_statistics_seconds`, with the site day
/// number and time zone offset in the key (passing midnight or changing the time zone naturally changes the key).
/// With Redis disabled, reads always miss and writes are always dropped.
pub async fn cached_forum_statistics(
    db: &Database,
    cache: &crate::cache::TimedCache,
    now: i64,
    offset_minutes: i64,
) -> Result<ForumStatistics> {
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::FORUM_STATISTICS,
        0,
        &(
            cache.lifetimes().forum_statistics_bucket(now),
            crate::clock::day_number(now, offset_minutes),
            offset_minutes,
        ),
    )?;
    cache
        .get_or_fill_json(&key, || forum_statistics_at(db, now, offset_minutes))
        .await
}

/// Forum statistics: process cache → Redis (when enabled) → computed. The admin forum
/// list and `/forums` share this one copy.
pub async fn layered_forum_statistics(
    db: &Database,
    global: &crate::cache::GlobalCache,
    cache: &crate::cache::TimedCache,
    now: i64,
    offset_minutes: i64,
) -> Result<ForumStatistics> {
    global
        .forum_statistics(
            (
                cache.lifetimes().forum_statistics_bucket(now),
                crate::clock::day_number(now, offset_minutes),
                offset_minutes,
            ),
            || cached_forum_statistics(db, cache, now, offset_minutes),
        )
        .await
}

/// Community statistics by scope: process cache → Redis (when enabled) → computed, segmented like the site-wide
/// statistics; when `fids` is empty this is just [`layered_statistics`].
pub async fn layered_statistics_in(
    db: &Database,
    global: &crate::cache::GlobalCache,
    cache: &crate::cache::TimedCache,
    (now, offset_minutes): (i64, i64),
    fids: &[i64],
) -> Result<Statistics> {
    if fids.is_empty() {
        return layered_statistics(db, global, cache, now, offset_minutes).await;
    }
    let segment = statistics_segment(cache.lifetimes(), now, offset_minutes);
    global
        .scoped_statistics(fids, segment, || async {
            let key = crate::cache::CacheKey::new(
                &crate::cache::registry::SCOPED_STATISTICS,
                0,
                &(segment, fids),
            )?;
            cache
                .get_or_fill_json(&key, || async {
                    statistics_in(db, u64::try_from(now)?, offset_minutes, fids).await
                })
                .await
        })
        .await
}

/// Site-wide statistics: process cache → Redis (when enabled) → computed. Segmented like
/// [`cached_statistics`].
pub async fn layered_statistics(
    db: &Database,
    global: &crate::cache::GlobalCache,
    cache: &crate::cache::TimedCache,
    now: i64,
    offset_minutes: i64,
) -> Result<Statistics> {
    global
        .statistics(
            statistics_segment(cache.lifetimes(), now, offset_minutes),
            || cached_statistics(db, cache, now, offset_minutes),
        )
        .await
}

/// The segment site and scoped statistics are cached by: the time segment (`settings.cache.statistics_seconds`) and
/// the start of this week in the site time zone, which also moves with the time zone. The bucket alone is counted in
/// UTC, so a long segment that straddles local Monday 00:00 would keep last week's contributors.
fn statistics_segment(
    lifetimes: crate::cache::Lifetimes,
    now: i64,
    offset_minutes: i64,
) -> (i64, i64) {
    (
        lifetimes.statistics_bucket(now),
        crate::clock::week_start(now, offset_minutes),
    )
}

fn search_pattern(value: &str) -> String {
    format!(
        "%{}%",
        value
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

fn display_time(seconds: u64) -> Result<String> {
    let timestamp = i64::try_from(seconds).context("time out of range")?;
    let text = crate::clock::short_datetime(timestamp, &crate::clock::current());
    ensure!(!text.is_empty(), "time out of range");
    Ok(text)
}

/// After reading from the cache, rewrite the post times by this request's definition. The copy in the cached value
/// was formatted when it was loaded, so with relative times on it would lag by up to one cache TTL
/// (docs/settings-and-caching.md, "The Redis cache"); like thread list rows (`router::list_row`) it is computed per
/// request.
fn refresh_post_times(posts: &mut [Post]) -> Result<()> {
    for post in posts {
        post.time = display_time(post.dateline)?;
    }
    Ok(())
}

/// Same as [`refresh_post_times`], for the threads in the sidebar cache.
fn refresh_thread_times(threads: &mut [Thread]) -> Result<()> {
    for thread in threads {
        thread.time = display_time(thread.updated)?;
        thread.created_time = display_time(thread.created)?;
    }
    Ok(())
}

fn decode_thread(row: DbRow) -> Result<Thread> {
    decode_thread_row(row, true)
}

fn decode_list_thread(row: DbRow) -> Result<Thread> {
    decode_thread_row(row, false)
}

fn decode_thread_row(row: DbRow, with_opening: bool) -> Result<Thread> {
    let created = u64::try_from(row.try_get::<i64, _>("dateline")?)?;
    let updated = u64::try_from(row.try_get::<i64, _>("lastpost")?)?;
    let (first_version, edited_at, first_pid, first_deleted_at) = if with_opening {
        (
            row.try_get("first_version")?,
            row.try_get("edited_at")?,
            row.try_get("first_pid")?,
            row.try_get::<i64, _>("first_deleted_at")?,
        )
    } else {
        (0, 0, 0, 0)
    };
    // The thread itself is members-only; whether the forum (including inheritance) is members-only is filled in by
    // the router from the forum cache.
    let members_only = decode_text(&row, "visibility")? == "members";
    // Lists only read the author copy on the thread row, already written as "Deleted user" at deletion; the thread
    // detail joins the member table and judges by account status.
    let author_deleted = with_opening
        && row.try_get::<Option<String>, _>("author_state")?.as_deref() == Some("deleted");
    let erased = with_opening && row.try_get::<i64, _>("erased_at")? > 0;
    let withdrawn = row.try_get::<i64, _>("deleted_at")? > 0 || first_deleted_at > 0;
    // Public models and caches only receive the masked title and opening body.
    let title = if erased {
        crate::i18n::current().common_erased_title().into()
    } else if withdrawn {
        crate::i18n::current().common_withdrawn_title().into()
    } else {
        row.try_get("subject")?
    };
    let body = if with_opening && !withdrawn {
        decode_text(&row, "message")?
    } else {
        String::new()
    };
    let author_copy: Option<String> = row.try_get("author")?;
    let author_gone = author_deleted || author_copy.as_deref() == Some(copies::DELETED_AUTHOR);
    Ok(Thread {
        id: u64::try_from(row.try_get::<i64, _>("tid")?)?,
        fixture_forum: String::new(),
        title,
        author: if author_deleted {
            crate::i18n::current().common_deleted_user().into()
        } else {
            author_copy
                .map(copies::shown_name)
                .unwrap_or_else(|| crate::i18n::current().common_deleted_user().into())
        },
        // List avatars get a URL by uid; only the thread detail reads the current avatar
        // from the member table.
        avatar: if author_deleted || !with_opening {
            0
        } else {
            usize::try_from(row.try_get::<Option<i64>, _>("avatar")?.unwrap_or(0))?
        },
        avatar_aid: if author_deleted || !with_opening {
            0
        } else {
            row.try_get::<Option<i64>, _>("avatar_aid")?.unwrap_or(0)
        },
        last_reply_author: copies::shown_name(row.try_get("last_reply_author")?),
        last_reply_pid: row.try_get("last_reply_pid")?,
        last_reply_uid: row.try_get("last_reply_uid")?,
        // Withdrawn and erased threads keep no excerpt in public models, like their title and body.
        last_reply_summary: if withdrawn || erased {
            String::new()
        } else {
            decode_text(&row, "last_reply_summary")?
        },
        pin: row.try_get::<i64, _>("displayorder")?.clamp(0, 3),
        max_position: row.try_get("maxposition")?,
        created,
        updated,
        time: display_time(updated)?,
        created_time: display_time(created)?,
        views: usize::try_from(row.try_get::<i64, _>("views")?)?,
        pinned: row.try_get::<i64, _>("displayorder")? > 0,
        featured: row.try_get::<i64, _>("digest")? > 0,
        body: vec![body],
        posts: Vec::new(),
        reply_count: usize::try_from(row.try_get::<i64, _>("reply_count")?)?,
        root_replies: usize::try_from(row.try_get::<i64, _>("root_replies")?)?,
        category: row.try_get("category")?,
        cache_revision: row.try_get("cache_revision")?,
        first_pid,
        fid: row.try_get("fid")?,
        authorid: row.try_get("authorid")?,
        author_deleted,
        author_gone,
        version: row.try_get("version")?,
        first_version,
        erased,
        blocked: false,
        withdrawn,
        closed: row.try_get::<i64, _>("closed")? != 0,
        edited_at,
        members_only,
        favorites: row.try_get("favorites")?,
        tips: row.try_get("tips")?,
        tip_total: if with_opening {
            row.try_get("tip_total")?
        } else {
            0
        },
        downvotes: if with_opening {
            row.try_get("first_downvotes")?
        } else {
            0
        },
        author_unavailable: with_opening
            && (row.try_get::<Option<i64>, _>("author_freeze")?.unwrap_or(1) != 0
                || row.try_get::<Option<String>, _>("author_state")?.as_deref() != Some("active")),
        thumbnails: Vec::new(),
    })
}

fn root_position(row: &DbRow) -> Result<usize> {
    Ok(usize::try_from(
        row.try_get::<Option<i64>, _>("root_position")?
            .or(row.try_get::<Option<i64>, _>("parent_root_position")?)
            .unwrap_or(0),
    )?)
}

fn decode_post(row: DbRow) -> Result<Post> {
    let dateline = u64::try_from(row.try_get::<i64, _>("dateline")?)?;
    let author_deleted =
        row.try_get::<Option<String>, _>("author_state")?.as_deref() == Some("deleted");
    let erased = row.try_get::<i64, _>("erased_at")? > 0;
    let withdrawn = row.try_get::<i64, _>("deleted_at")? > 0;
    let hidden = row.try_get::<i64, _>("invisible")? != 0
        || row
            .try_get::<Option<i64>, _>("parent_invisible")?
            .unwrap_or(0)
            != 0;
    let parent_label = match row.try_get::<Option<i64>, _>("target_position")? {
        Some(position) => {
            let author = if row
                .try_get::<Option<String>, _>("target_author_state")?
                .as_deref()
                == Some("deleted")
            {
                crate::i18n::current().common_deleted_user().into()
            } else {
                row.try_get::<Option<String>, _>("target_author")?
                    .map(copies::shown_name)
                    .unwrap_or_else(|| crate::i18n::current().common_deleted_user().into())
            };
            crate::i18n::current().topic_reply_to(author, position.to_string())
        }
        None => String::new(),
    };
    let body = if hidden || withdrawn {
        String::new()
    } else {
        decode_text(&row, "message")?
    };
    Ok(Post {
        pid: u64::try_from(row.try_get::<i64, _>("pid")?)?,
        position: usize::try_from(row.try_get::<i64, _>("position")?)?,
        root_position: root_position(&row)?,
        sub_position: usize::try_from(row.try_get::<Option<i64>, _>("sub_position")?.unwrap_or(0))?,
        sub_replies_count: usize::try_from(row.try_get::<i64, _>("sub_replies_count")?)?,
        root_floor: usize::try_from(row.try_get::<Option<i64>, _>("root_floor")?.unwrap_or(0))?,
        parent_label,
        root_pid: row
            .try_get::<Option<i64>, _>("root_pid")?
            .map(u64::try_from)
            .transpose()?,
        reply_to_pid: row
            .try_get::<Option<i64>, _>("reply_to_pid")?
            .map(u64::try_from)
            .transpose()?,
        author: if author_deleted {
            crate::i18n::current().common_deleted_user().into()
        } else {
            row.try_get::<Option<String>, _>("author")?
                .map(copies::shown_name)
                .unwrap_or_else(|| crate::i18n::current().common_deleted_user().into())
        },
        avatar: if author_deleted {
            0
        } else {
            usize::try_from(row.try_get::<Option<i64>, _>("avatar")?.unwrap_or(0))?
        },
        avatar_aid: if author_deleted {
            0
        } else {
            row.try_get::<Option<i64>, _>("avatar_aid")?.unwrap_or(0)
        },
        body,
        time: display_time(dateline)?,
        dateline,
        authorid: row.try_get("authorid")?,
        author_deleted,
        version: row.try_get("version")?,
        withdrawn,
        erased,
        blocked: false,
        hidden,
        edited_at: row.try_get("edited_at")?,
    })
}

pub struct PublicMember {
    pub uid: i64,
    pub username: String,
    pub avatar: i64,
    pub avatar_aid: i64,
    pub bio: String,
    pub regdate: i64,
    pub space_visibility: crate::identity::SpaceVisibility,
    /// `account_state='active'`; deleted accounts decode to [`MemberProfile::Deleted`], otherwise this is the
    /// account deletion cooling-off period (`deleting`) and the profile page shows the "Being deleted" label.
    pub active: bool,
    /// `freeze` is not 0: the profile page shows the "Frozen" label without reason or duration; it disappears
    /// automatically once unfrozen.
    pub frozen: bool,
    pub posts: Vec<MemberPost>,
}
/// A member read by uid for the profile page. A deleted account keeps only a marker: the 410 placeholder page shows
/// no original username, avatar, bio, statistics or content, so this branch carries
/// no profile fields.
pub enum MemberProfile {
    Deleted,
    Member(PublicMember),
}
pub struct MemberPost {
    pub tid: u64,
    pub pid: u64,
    pub position: usize,
    pub href: String,
    pub subject: String,
    pub message: String,
    pub time: String,
    /// The post time as a timestamp, for the `datetime` of the shown time.
    pub dateline: u64,
    pub forum_name: String,
    pub forum_slug: String,
    pub category: i64,
    pub views: usize,
    pub replies: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberContentKind {
    Threads,
    Posts,
}

pub struct MemberContentPage {
    pub posts: Vec<MemberPost>,
    pub total: usize,
}

/// The personal space joins each post to its thread and forum: readability is judged by the forum ids this visitor
/// can read; forum name and URL come from the forum table (the thread row carries no forum copies, and the member's
/// own content may be in forums the member can no longer read, which are not in the forum cache).
///
/// Every statement on these joins filters `p.authorid = ?` and reads the member's posts from
/// `gonof_post_author_created (authorid, first, dateline)`, forced: with a long list of readable forums MySQL would
/// otherwise start from the threads of those forums and look for the member's posts in each. Lists sort by
/// [`MEMBER_CONTENT_ORDER`].
const MEMBER_CONTENT_JOINS: &str = "FROM gonof_forum_post p FORCE INDEX (gonof_post_author_created)
    JOIN gonof_forum_thread t ON t.tid = p.tid
    JOIN gonof_forum_forum f ON f.fid = t.fid";

/// Newest first; ties go by the primary key `(tid, position)`, which InnoDB keeps at the end of every secondary
/// index, so with `p.first` fixed MySQL reads a page in index order and stops after it instead of sorting all of the
/// member's posts.
const MEMBER_CONTENT_ORDER: &str = "p.dateline DESC,p.tid DESC,p.position DESC";

fn member_content_columns() -> String {
    "SELECT p.pid,p.position,p.tid,p.root_pid,p.root_position,p.sub_position,
            parent.root_position AS parent_root_position,p.dateline,p.message,
            t.subject,t.deleted_at,t.deleted_at AS first_deleted_at,
            f.name AS forum_name,f.slug AS forum_slug,t.category,t.views,t.replies"
        .into()
}

fn member_content_select() -> String {
    format!(
        "{} {MEMBER_CONTENT_JOINS}
        LEFT JOIN gonof_forum_post parent ON parent.pid = p.root_pid AND parent.tid = p.tid",
        member_content_columns()
    )
}

/// The member's posts the visitor can see: the thread is readable ([`thread_visible_sql`]), the post is visible,
/// neither the author nor the thread author is blocked by the visitor (`blocked`, members the visitor blocked), and
/// it is not in a forum the visitor blocked. No subqueries.
fn member_content_filter_for(
    readable: Readable<'_>,
    blocked: &[i64],
    blocked_fids: &[i64],
) -> String {
    format!(
        "{} AND {}{}{}{} AND p.authorid = ?",
        thread_visible_sql(readable),
        post_visible("p"),
        not_blocked("p.authorid", blocked),
        not_blocked("t.authorid", blocked),
        excluded_sql("t.fid", blocked_fids)
    )
}

/// Members the visitor has blocked; empty for anonymous visitors.
async fn viewer_blocks(db: &Database, viewer_uid: Option<i64>) -> Result<Vec<i64>> {
    match viewer_uid {
        Some(uid) => blocked_authors(db, uid).await,
        None => Ok(Vec::new()),
    }
}

fn decode_member_post(row: DbRow, paging: crate::urls::Paging) -> Result<MemberPost> {
    let tid = u64::try_from(row.try_get::<i64, _>("tid")?)?;
    let position = usize::try_from(row.try_get::<i64, _>("position")?)?;
    let root_pid = row
        .try_get::<Option<i64>, _>("root_pid")?
        .map(u64::try_from)
        .transpose()?;
    let root_position = root_position(&row)?;
    let sub_position =
        usize::try_from(row.try_get::<Option<i64>, _>("sub_position")?.unwrap_or(0))?;
    let dateline = u64::try_from(row.try_get::<i64, _>("dateline")?)?;
    ensure!(
        position > 0
            && (position == 1 || root_position > 0)
            && (root_pid.is_none() || sub_position > 0),
        "invalid position of a member's post"
    );
    Ok(MemberPost {
        tid,
        pid: u64::try_from(row.try_get::<i64, _>("pid")?)?,
        position,
        href: crate::urls::position_location(
            tid,
            position,
            root_pid,
            root_position,
            sub_position,
            false,
            paging,
        ),
        subject: if row.try_get::<i64, _>("deleted_at")? > 0
            || row.try_get::<i64, _>("first_deleted_at")? > 0
        {
            crate::i18n::current().common_withdrawn_title().into()
        } else {
            row.try_get("subject")?
        },
        message: decode_text(&row, "message")?,
        time: display_time(dateline)?,
        dateline,
        forum_name: row.try_get("forum_name")?,
        forum_slug: row.try_get("forum_slug")?,
        category: row.try_get("category")?,
        views: usize::try_from(row.try_get::<i64, _>("views")?)?,
        replies: usize::try_from(row.try_get::<i64, _>("replies")?)?,
    })
}

pub async fn member_content(
    db: &Database,
    uid: i64,
    kind: MemberContentKind,
    page: usize,
    page_size: usize,
    paging: crate::urls::Paging,
) -> Result<MemberContentPage> {
    let forums = anonymous_forums(db).await?;
    let readable = Readable {
        uid: None,
        forums: &forums,
    };
    member_content_for_viewer(db, uid, kind, page, page_size, readable, &[], paging).await
}
/// `blocked_fids` are the forums the visitor blocked (child forums expanded); hidden forums still appear in the
/// personal space. `paging` is the floor pagination (post links point to the page holding the floor).
#[allow(clippy::too_many_arguments)]
pub async fn member_content_for_viewer(
    db: &Database,
    uid: i64,
    kind: MemberContentKind,
    page: usize,
    page_size: usize,
    readable: Readable<'_>,
    blocked_fids: &[i64],
    paging: crate::urls::Paging,
) -> Result<MemberContentPage> {
    ensure!(
        page > 0 && page_size > 0 && page_size <= 100,
        "invalid page or page size"
    );
    let first = match kind {
        MemberContentKind::Threads => 1,
        MemberContentKind::Posts => 0,
    };
    let filter = format!(
        "{} AND p.first = {first}",
        member_content_filter_for(
            readable,
            &viewer_blocks(db, readable.uid).await?,
            blocked_fids
        )
    );
    let count_sql = format!("SELECT COUNT(*) {MEMBER_CONTENT_JOINS} WHERE {filter}");
    let total = usize::try_from(
        crate::db::query_scalar::<i64>(&count_sql)
            .bind(uid)
            .fetch_one(db.executor())
            .await?,
    )?;
    member_content_page_for_viewer(
        db,
        uid,
        kind,
        page,
        page_size,
        readable,
        blocked_fids,
        (total, paging),
    )
    .await
}

/// Fetch only this page when the total is already known (the profile page's total comes from
/// [`member_stats_for_viewer`] and is not counted again). `total` must be the total for the same visitor and the
/// same set of blocked forums.
#[allow(clippy::too_many_arguments)]
pub async fn member_content_page_for_viewer(
    db: &Database,
    uid: i64,
    kind: MemberContentKind,
    page: usize,
    page_size: usize,
    readable: Readable<'_>,
    blocked_fids: &[i64],
    (total, paging): (usize, crate::urls::Paging),
) -> Result<MemberContentPage> {
    ensure!(
        page > 0 && page_size > 0 && page_size <= 100,
        "invalid page or page size"
    );
    let first = match kind {
        MemberContentKind::Threads => 1,
        MemberContentKind::Posts => 0,
    };
    let filter = format!(
        "{} AND p.first = {first}",
        member_content_filter_for(
            readable,
            &viewer_blocks(db, readable.uid).await?,
            blocked_fids
        )
    );
    if page > total.div_ceil(page_size).max(1) {
        return Ok(MemberContentPage {
            posts: Vec::new(),
            total,
        });
    }
    let offset = (page - 1)
        .checked_mul(page_size)
        .context("page offset out of range")?;
    let sql = format!(
        "{} WHERE {filter} ORDER BY {MEMBER_CONTENT_ORDER} LIMIT ? OFFSET ?",
        member_content_select()
    );
    let posts = crate::db::query(&sql)
        .bind(uid)
        .bind(i64::try_from(page_size)?)
        .bind(i64::try_from(offset)?)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| decode_member_post(row, paging))
        .collect::<Result<_>>()?;
    Ok(MemberContentPage { posts, total })
}

/// Profile page statistics: thread, reply and highlighted thread counts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemberStats {
    pub threads: usize,
    pub replies: usize,
    pub digests: usize,
}

/// A member's thread, reply and highlight counts (`gonof_common_member_count`): the member's own posts that are not
/// withdrawn or hidden, read from one row of the counter table, the same number for every visitor. The pagination
/// total of the profile page's "Threads" / "Replies" tabs uses it too; entries
/// the visitor cannot read are still not listed, so the last few pages may not be full. All 0 when the visitor has
/// blocked the member (none of their posts would be listed).
pub async fn member_stats_for_viewer(
    db: &Database,
    uid: i64,
    viewer_uid: Option<i64>,
) -> Result<MemberStats> {
    if let Some(viewer) = viewer_uid {
        let blocked: Option<i64> = crate::db::query_scalar(
            "SELECT blocked_uid FROM gonof_home_member_block WHERE uid=? AND blocked_uid=?",
        )
        .bind(viewer)
        .bind(uid)
        .fetch_optional(db.executor())
        .await?;
        if blocked.is_some() {
            return Ok(MemberStats::default());
        }
    }
    member_counts(db, uid).await
}

/// The member's row in the counter table; members who have never posted have no row and count as 0.
pub async fn member_counts(db: &Database, uid: i64) -> Result<MemberStats> {
    let row = crate::db::query(
        "SELECT threads,replies,digests FROM gonof_common_member_count WHERE uid=?",
    )
    .bind(uid)
    .fetch_optional(db.executor())
    .await?;
    let Some(row) = row else {
        return Ok(MemberStats::default());
    };
    let count = |column: &str| -> Result<usize> {
        Ok(usize::try_from(row.try_get::<i64, _>(column)?.max(0))?)
    };
    Ok(MemberStats {
        threads: count("threads")?,
        replies: count("replies")?,
        digests: count("digests")?,
    })
}

/// Online status on the profile page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    /// Active within the last 15 minutes.
    Online,
    /// Time of the last activity; the page only shows the day.
    LastSeen(i64),
    /// No record (never signed in, or the record predates this feature).
    Unknown,
}

/// Presence (docs/queries.md, "Last online"): requests only write the online table, and the last-online table is
/// written back only when expired sessions are cleaned up. So check the online table first: activity within the
/// last 15 minutes means online; otherwise read the last-online table, and take the later of the two.
pub async fn member_presence(db: &Database, uid: i64, now: i64) -> Result<Presence> {
    let active: Option<i64> =
        crate::db::query_scalar("SELECT MAX(lastactivity) FROM gonof_common_session WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await?;
    if let Some(active) = active
        && now.saturating_sub(active) < crate::identity::ONLINE_WINDOW
    {
        return Ok(Presence::Online);
    }
    let seen: Option<i64> =
        crate::db::query_scalar("SELECT last_seen FROM gonof_common_member_presence WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.executor())
            .await?;
    Ok(match active.max(seen) {
        Some(seen) if now.saturating_sub(seen) < crate::identity::ONLINE_WINDOW => Presence::Online,
        Some(seen) => Presence::LastSeen(seen),
        None => Presence::Unknown,
    })
}

/// One item from the member's own view: besides the public set it includes moderation-hidden and author-withdrawn
/// items, with a status note.
pub struct OwnPost {
    pub post: MemberPost,
    pub status: &'static str,
    /// The post's own status, used to judge eligibility for a restore appeal; kept apart from the display status
    /// above, which also accounts for cascading hides.
    pub own: OwnState,
}

/// The post's own moderation and withdrawal status (excluding hides cascading from the first post or the parent
/// post).
#[derive(Clone, Copy, Debug, Default)]
pub struct OwnState {
    pub hidden: bool,
    pub reserved: bool,
    pub withdrawn: bool,
    pub inherited: bool,
    pub dateline: i64,
}

pub struct OwnContentPage {
    pub rows: Vec<OwnPost>,
    pub total: usize,
}

/// The member's own thread or reply list.
///
/// It differs from the public set only in visibility: moderation hides and author withdrawals must stay visible
/// to the member; only permanently erased content no longer appears. The status is judged in code from the
/// `invisible` of the post, its parent post and the first post, and the two `deleted_at` columns, not composed
/// as an expression in SQL.
pub async fn own_content(
    db: &Database,
    uid: i64,
    kind: MemberContentKind,
    page: usize,
    page_size: usize,
    paging: crate::urls::Paging,
) -> Result<OwnContentPage> {
    ensure!(
        page > 0 && page_size > 0 && page_size <= 100,
        "invalid page or page size"
    );
    let first = match kind {
        MemberContentKind::Threads => 1,
        MemberContentKind::Posts => 0,
    };
    let filter = format!("p.erased_at = 0 AND p.authorid = ? AND p.first = {first}");
    let total = usize::try_from(
        crate::db::query_scalar::<i64>(&format!(
            "SELECT COUNT(*) {MEMBER_CONTENT_JOINS} WHERE {filter}"
        ))
        .bind(uid)
        .fetch_one(db.executor())
        .await?,
    )?;
    if page > total.div_ceil(page_size).max(1) {
        return Ok(OwnContentPage {
            rows: Vec::new(),
            total,
        });
    }
    let offset = (page - 1)
        .checked_mul(page_size)
        .context("page offset out of range")?;
    let sql = format!(
        "{},p.invisible,firstpost.invisible AS first_invisible,parent.invisible AS parent_invisible,
            p.deleted_at AS post_deleted_at,t.visibility AS thread_visibility,
            f.visibility AS forum_visibility,f.status AS forum_status,pf.visibility AS parent_visibility,pf.status AS parent_status
         {MEMBER_CONTENT_JOINS}
         LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid
         JOIN gonof_forum_post firstpost ON firstpost.first_tid = t.tid
         LEFT JOIN gonof_forum_post parent ON parent.pid = p.root_pid AND parent.tid = p.tid
         WHERE {filter} ORDER BY {MEMBER_CONTENT_ORDER} LIMIT ? OFFSET ?",
        member_content_columns()
    );
    let rows = crate::db::query(&sql)
        .bind(uid)
        .bind(i64::try_from(page_size)?)
        .bind(i64::try_from(offset)?)
        .fetch_all(db.executor())
        .await?
        .into_iter()
        .map(|row| {
            // The public set judges hiding by the invisible of the post, its parent post and the first post (see
            // public_post and readable_thread_sql); the member's own list status matches that and cannot look at
            // the post alone.
            let hidden = row.try_get::<i64, _>("invisible")? != 0
                || row.try_get::<i64, _>("first_invisible")? != 0
                || row
                    .try_get::<Option<i64>, _>("parent_invisible")?
                    .is_some_and(|value| value != 0);
            let withdrawn: i64 = row.try_get("post_deleted_at")?;
            let thread_withdrawn: i64 = row.try_get("deleted_at")?;
            let invisible: i64 = row.try_get("invisible")?;
            let own = OwnState {
                hidden: invisible == -1,
                reserved: ![0, -1].contains(&invisible),
                withdrawn: withdrawn > 0 || (first == 1 && thread_withdrawn > 0),
                inherited: first == 0
                    && (row.try_get::<i64, _>("first_invisible")? != 0
                        || row
                            .try_get::<Option<i64>, _>("parent_invisible")?
                            .is_some_and(|value| value != 0)),
                dateline: row.try_get("dateline")?,
            };
            // When the thread or its forum (after inheritance) is members-only, the post is only shown to signed-in
            // members.
            let members_only = decode_text(&row, "thread_visibility")? == "members"
                || copies::forum_scope(&row)?.visibility == copies::ForumVisibility::Members;
            let status = if withdrawn > 0 || thread_withdrawn > 0 {
                crate::i18n::current().common_status_withdrawn()
            } else if hidden {
                crate::i18n::current().common_status_hidden()
            } else if members_only {
                crate::i18n::current().common_status_members()
            } else {
                crate::i18n::current().common_status_public()
            };
            Ok(OwnPost {
                post: decode_member_post(row, paging)?,
                status,
                own,
            })
        })
        .collect::<Result<_>>()?;
    Ok(OwnContentPage { rows, total })
}

const PUBLIC_MEMBER_SQL: &str = "SELECT uid, username, avatar, avatar_aid, bio, regdate, space_visibility, account_state, `freeze` FROM gonof_common_member";

/// The personal space is located by uid, so renaming does not change the URL. A deleted account returns
/// [`MemberProfile::Deleted`], and the profile page gives a 410 placeholder page.
pub async fn public_member_profile(db: &Database, uid: i64) -> Result<Option<MemberProfile>> {
    let member = crate::db::query(&format!("{PUBLIC_MEMBER_SQL} WHERE uid=?"))
        .bind(uid)
        .fetch_optional(db.executor())
        .await?;
    decode_public_member(member)
}

/// Find a member by username. The username is not the profile page URL; deleted accounts are still treated as
/// nonexistent.
pub async fn public_member_profile_by_name(
    db: &Database,
    username: &str,
) -> Result<Option<PublicMember>> {
    let member = crate::db::query(&format!("{PUBLIC_MEMBER_SQL} WHERE username=?"))
        .bind(username)
        .fetch_optional(db.executor())
        .await?;
    Ok(match decode_public_member(member)? {
        Some(MemberProfile::Member(member)) => Some(member),
        Some(MemberProfile::Deleted) | None => None,
    })
}

fn decode_public_member(member: Option<DbRow>) -> Result<Option<MemberProfile>> {
    let Some(member) = member else {
        return Ok(None);
    };
    let account_state = decode_text(&member, "account_state")?;
    if account_state == "deleted" {
        return Ok(Some(MemberProfile::Deleted));
    }
    Ok(Some(MemberProfile::Member(PublicMember {
        uid: member.try_get("uid")?,
        username: member.try_get("username")?,
        avatar: member.try_get("avatar")?,
        avatar_aid: member.try_get::<Option<i64>, _>("avatar_aid")?.unwrap_or(0),
        bio: decode_text(&member, "bio")?,
        regdate: member.try_get("regdate")?,
        space_visibility: crate::identity::SpaceVisibility::parse(
            &member.try_get::<String, _>("space_visibility")?,
        )?,
        active: account_state == "active",
        frozen: member.try_get::<i64, _>("freeze")? != 0,
        posts: Vec::new(),
    })))
}

pub async fn public_member(
    db: &Database,
    username: &str,
    paging: crate::urls::Paging,
) -> Result<Option<PublicMember>> {
    let Some(mut member) = public_member_profile_by_name(db, username).await? else {
        return Ok(None);
    };
    if !member.space_visibility.can_view(member.uid, None) {
        return Ok(None);
    }
    member.posts = recent_member_content(db, member.uid, paging).await?;
    Ok(Some(member))
}

pub async fn recent_member_content(
    db: &Database,
    uid: i64,
    paging: crate::urls::Paging,
) -> Result<Vec<MemberPost>> {
    let forums = anonymous_forums(db).await?;
    let readable = Readable {
        uid: None,
        forums: &forums,
    };
    recent_member_content_for_viewer(db, uid, readable, &[], paging).await
}
pub async fn recent_member_content_for_viewer(
    db: &Database,
    uid: i64,
    readable: Readable<'_>,
    blocked_fids: &[i64],
    paging: crate::urls::Paging,
) -> Result<Vec<MemberPost>> {
    let sql = format!(
        "{} WHERE {} ORDER BY {MEMBER_CONTENT_ORDER} LIMIT 20",
        member_content_select(),
        member_content_filter_for(
            readable,
            &viewer_blocks(db, readable.uid).await?,
            blocked_fids
        )
    );
    let rows = crate::db::query(&sql)
        .bind(uid)
        .fetch_all(db.executor())
        .await?;
    rows.into_iter()
        .map(|row| decode_member_post(row, paging))
        .collect()
}

pub fn category_label(category: i64) -> &'static str {
    match category {
        1 => crate::i18n::current().common_category_bounty(),
        2 => crate::i18n::current().common_category_red_packet(),
        3 => crate::i18n::current().common_category_lottery(),
        4 => crate::i18n::current().common_category_poll(),
        5 => crate::i18n::current().common_category_news(),
        _ => crate::i18n::current().common_category_normal(),
    }
}
impl Thread {
    pub fn badge_class(&self) -> &'static str {
        match self.category {
            1 => "tag--bounty",
            2 => "tag--redpacket",
            3 => "tag--lottery",
            4 => "tag--poll",
            5 => "news-badge",
            _ => "",
        }
    }
    pub fn category_label(&self) -> &'static str {
        category_label(self.category)
    }
    /// The category code of a list row's `data-kind` (docs/skins.md, "List rows"): `bounty`, `redpacket`,
    /// `lottery`, `poll` or `news`; empty for an ordinary thread, which has no `data-kind`.
    pub fn kind_code(&self) -> &'static str {
        match self.category {
            1 => "bounty",
            2 => "redpacket",
            3 => "lottery",
            4 => "poll",
            5 => "news",
            _ => "",
        }
    }
    /// The states of a list row's `data-state` (docs/skins.md, "List rows"): `pinned`, `featured` and `closed`,
    /// space-separated in that order; empty when none applies, and the row then has no `data-state`.
    pub fn state_tokens(&self) -> String {
        [
            (self.pinned, "pinned"),
            (self.featured, "featured"),
            (self.closed, "closed"),
        ]
        .into_iter()
        .filter_map(|(on, token)| on.then_some(token))
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// The site-wide content version: floor and render cache keys carry it (plus the thread version); it is only
/// incremented on structural events (`copies`).
pub async fn cache_revision(db: &Database) -> Result<i64> {
    Ok(
        crate::db::query_scalar("SELECT revision FROM gonof_common_content_revision WHERE id=1")
            .fetch_one(db.executor())
            .await?,
    )
}

/// Version of list caches (home page and forum lists, sidebar hot and latest, feeds): the site-wide version plus
/// the new-post watermark, fetched in one round trip.
///
/// The watermark is the largest PID in the post table (read from the end of the primary key, without a lock). New
/// threads and new replies both bring a new PID, so the list key changes at once and replies need not increment the
/// site-wide version; likes and poll votes do not change list content and do not invalidate lists. When two
/// replies commit interleaved, the one that got the smaller id but committed later shows up in lists at most one
/// cache period late (or at the next new post), which only affects freshness.
pub async fn list_revision(db: &Database) -> Result<(i64, i64)> {
    let row = crate::db::query(
        "SELECT revision,(SELECT MAX(pid) FROM gonof_forum_post) AS watermark FROM gonof_common_content_revision WHERE id=1",
    )
    .fetch_one(db.executor())
    .await?;
    Ok((
        row.try_get("revision")?,
        row.try_get::<Option<i64>, _>("watermark")?.unwrap_or(0),
    ))
}
#[allow(clippy::too_many_arguments)]
/// Anonymous list by forum URL (for tests): an empty `slug` takes the whole site, otherwise only that forum itself;
/// an unknown URL gives an empty page.
pub async fn cached_list_threads(
    db: &Database,
    slug: &str,
    sort: Sort,
    search: Option<&str>,
    category: Option<i64>,
    cache: &crate::cache::TimedCache,
    page: usize,
    page_size: usize,
) -> Result<ThreadPage> {
    let readable = crate::organization::forums(db, None).await?;
    let scope = if slug.is_empty() {
        ListScope::All
    } else {
        match readable.iter().find(|forum| forum.slug == slug) {
            Some(forum) => ListScope::Forum(forum.fid),
            None => {
                return Ok(ThreadPage {
                    threads: vec![],
                    total: 0,
                    site_revision: None,
                });
            }
        }
    };
    let forums: Vec<i64> = readable.iter().map(|forum| forum.fid).collect();
    cached_scope_threads(
        db,
        &scope,
        (sort, search, category),
        cache,
        (page, page_size),
        &forums,
    )
    .await
}

/// Shared cache of anonymous lists: the scope and the forum ids anonymous visitors can see (different for each
/// multi-domain site) are encoded in the key.
pub async fn cached_scope_threads(
    db: &Database,
    scope: &ListScope,
    (sort, search, category): (Sort, Option<&str>, Option<i64>),
    cache: &crate::cache::TimedCache,
    (page, page_size): (usize, usize),
    forums: &[i64],
) -> Result<ThreadPage> {
    let load = || {
        list_scope_for_viewer(
            db,
            scope,
            sort,
            search,
            category,
            page,
            page_size,
            ListViewer {
                forums,
                ..ListViewer::default()
            },
        )
    };
    if !cache.cache().enabled() {
        return load().await;
    }
    let (revision, watermark) = list_revision(db).await?;
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::THREAD_LIST,
        revision,
        &(
            watermark,
            scope,
            forums,
            sort.value(),
            search,
            category,
            page,
            page_size,
            // The cached value holds times already formatted by site time zone and style.
            crate::clock::current(),
        ),
    )?;
    let mut result = cache.get_or_fill_json(&key, load).await?;
    result.site_revision = Some(revision);
    Ok(result)
}

/// For pages: the signed-in view differs per member and reads the database directly; anonymous visitors use the
/// shared cache ([`cached_scope_threads`]). `scope` is the list's forum scope: the home page and search are
/// site-wide (or the multi-domain site scope), a top-level forum page includes its child forums, and a child forum
/// page is just itself.
#[allow(clippy::too_many_arguments)]
pub async fn cached_list_threads_for_viewer(
    db: &Database,
    scope: &ListScope,
    sort: Sort,
    search: Option<&str>,
    category: Option<i64>,
    cache: &crate::cache::TimedCache,
    page: usize,
    page_size: usize,
    viewer: ListViewer<'_>,
) -> Result<ThreadPage> {
    if viewer.uid.is_some() {
        list_scope_for_viewer(db, scope, sort, search, category, page, page_size, viewer).await
    } else {
        cached_scope_threads(
            db,
            scope,
            (sort, search, category),
            cache,
            (page, page_size),
            viewer.forums,
        )
        .await
    }
}

pub async fn cached_sidebar(
    db: &Database,
    cache: &crate::cache::TimedCache,
    params: SidebarParams,
) -> Result<Overview> {
    let forums = anonymous_forums(db).await?;
    let anonymous = ListViewer {
        forums: &forums,
        ..ListViewer::default()
    };
    cached_sidebar_for_viewer(db, cache, anonymous, params).await
}

/// Sidebar statistics do not follow the content version; they are only segmented by
/// `settings.cache.statistics_seconds` (default 600 seconds).
///
/// The three aggregates scan all public posts, so a key that changed with every reply would rerun them almost every
/// time. The statistics are just three counts; being stale by at most one segment leaks no content. The
/// hot and latest cards are cached by the list version (site-wide version plus new-post watermark), so titles do
/// not keep showing after withdrawal or hiding. The version parameter is therefore fixed at 0. The segment length
/// is a setting; changing it changes the segment number and naturally the key. The start of this week in the site
/// time zone is encoded in the key as well, so the week's contributors start again at local Monday 00:00 and a time
/// zone change takes a new key (docs/settings-and-caching.md, "The Redis cache").
pub async fn cached_statistics(
    db: &Database,
    cache: &crate::cache::TimedCache,
    now: i64,
    offset_minutes: i64,
) -> Result<Statistics> {
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::STATISTICS,
        0,
        &statistics_segment(cache.lifetimes(), now, offset_minutes),
    )?;
    cache
        .get_or_fill_json(&key, || async {
            statistics_at(db, u64::try_from(now)?, offset_minutes).await
        })
        .await
}

async fn cached_sidebar_topics(
    db: &Database,
    cache: &crate::cache::TimedCache,
    now: i64,
    params: SidebarParams,
    scope: &ListScope,
    forums: &[i64],
) -> Result<(Vec<Thread>, Vec<Thread>)> {
    let anonymous = ListViewer {
        forums,
        ..ListViewer::default()
    };
    if !cache.cache().enabled() {
        return sidebar_topics(db, now, params, &anonymous, scope).await;
    }
    let (revision, watermark) = list_revision(db).await?;
    // Hot threads are computed over "the last N days" and move with time, so besides the list version they are also
    // segmented by the list TTL (the two sidebar cards follow the list cache). Days and counts are encoded in the
    // key: changing the settings naturally changes the key (docs/settings-and-caching.md, "The Redis cache").
    let list = i64::try_from(cache.lifetimes().list).unwrap_or(i64::MAX);
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::SIDEBAR_TOPICS,
        revision,
        &(
            watermark,
            now.div_euclid(list),
            crate::clock::current(),
            params,
            // A forum page's right column only looks at that forum (a parent forum includes its child forums); the
            // home page and search are site-wide. Visible forums change with the multi-domain site scope.
            scope,
            forums,
        ),
    )?;
    let (mut hot, mut latest) = cache
        .get_or_fill_json(&key, || sidebar_topics(db, now, params, &anonymous, scope))
        .await?;
    refresh_thread_times(&mut hot)?;
    refresh_thread_times(&mut latest)?;
    Ok((hot, latest))
}
/// Personal block lists only mark the page view; they never modify or write back the shared body cache.
pub async fn apply_blocks(
    db: &Database,
    viewer_uid: Option<i64>,
    thread: &mut Thread,
) -> Result<()> {
    let Some(uid) = viewer_uid else {
        return Ok(());
    };
    let ids: Vec<i64> = thread
        .authorid
        .into_iter()
        .chain(thread.posts.iter().filter_map(|p| p.authorid))
        .collect();
    let mut blocked = std::collections::BTreeSet::new();
    for candidates in ids.chunks(500) {
        blocked.extend(crate::interaction::social::blocked_uids(db, uid, candidates).await?);
    }
    thread.blocked = thread.authorid.is_some_and(|id| blocked.contains(&id));
    for post in &mut thread.posts {
        post.blocked = post.authorid.is_some_and(|id| blocked.contains(&id));
    }
    Ok(())
}
/// Signed-in queries for the sidebar's hot and latest cards. Members-only content and personal block lists differ
/// per member, so the public results in the shared cache cannot be used; the user center only needs these two
/// cards and need not run the three statistics queries along with them. The scope is site-wide.
pub async fn sidebar_topics_for_viewer(
    db: &Database,
    viewer: ListViewer<'_>,
    params: SidebarParams,
) -> Result<(Vec<Thread>, Vec<Thread>)> {
    sidebar_topics(db, crate::identity::now(), params, &viewer, &ListScope::All).await
}

/// The two signed-in cards differ per member and do not go into the shared cache; the three statistics are shared
/// with anonymous visitors.
pub async fn cached_sidebar_for_viewer(
    db: &Database,
    cache: &crate::cache::TimedCache,
    viewer: ListViewer<'_>,
    params: SidebarParams,
) -> Result<Overview> {
    let now = crate::identity::now();
    let statistics = cached_statistics(db, cache, now, params.offset_minutes).await?;
    sidebar_with(db, cache, viewer, params, now, statistics, &ListScope::All).await
}

/// For pages: same as [`cached_sidebar_for_viewer`], with statistics checked in the process cache first. `scope` is
/// the scope of the hot and latest cards (a forum page's right column: that forum, a parent forum including its
/// child forums; the home page and search: site-wide or the multi-domain site scope); statistics stay site-wide,
/// and when the multi-domain site scope is not everything they only count forums in `statistics_fids`; empty means
/// site-wide.
pub async fn layered_sidebar_for_viewer(
    db: &Database,
    global: &crate::cache::GlobalCache,
    cache: &crate::cache::TimedCache,
    viewer: ListViewer<'_>,
    params: SidebarParams,
    scope: &ListScope,
    statistics_fids: &[i64],
) -> Result<Overview> {
    let now = crate::identity::now();
    let statistics = layered_statistics_in(
        db,
        global,
        cache,
        (now, params.offset_minutes),
        statistics_fids,
    )
    .await?;
    sidebar_with(db, cache, viewer, params, now, statistics, scope).await
}

async fn sidebar_with(
    db: &Database,
    cache: &crate::cache::TimedCache,
    viewer: ListViewer<'_>,
    params: SidebarParams,
    now: i64,
    statistics: Statistics,
    scope: &ListScope,
) -> Result<Overview> {
    let (hot, latest) = if viewer.uid.is_some() {
        sidebar_topics(db, now, params, &viewer, scope).await?
    } else {
        cached_sidebar_topics(db, cache, now, params, scope, viewer.forums).await?
    };
    Ok(Overview {
        hot,
        latest,
        total: statistics.total,
        replies: statistics.replies,
        contributors: statistics.contributors,
    })
}

/// Signed-in reading always reads directly; personal block lists and members-only content never enter the shared
/// cache.
#[allow(clippy::too_many_arguments)]
pub async fn cached_replies_for_viewer(
    db: &Database,
    cache: &crate::cache::TimedCache,
    thread: &mut Thread,
    page: usize,
    flat: bool,
    sub_root: Option<u64>,
    sub_page: usize,
    (viewer_uid, paging): (Option<i64>, crate::urls::Paging),
) -> Result<()> {
    if viewer_uid.is_some() {
        load_replies(db, thread, page, flat, sub_root, sub_page, paging).await
    } else {
        cached_replies(db, cache, thread, page, flat, sub_root, (sub_page, paging)).await
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cached_replies(
    db: &Database,
    cache: &crate::cache::TimedCache,
    thread: &mut Thread,
    page: usize,
    flat: bool,
    sub_root: Option<u64>,
    (sub_page, paging): (usize, crate::urls::Paging),
) -> Result<()> {
    if !cache.cache().enabled() {
        return load_replies(db, thread, page, flat, sub_root, sub_page, paging).await;
    }
    // Caller has just loaded current public metadata. Never cache account controls or vote state.
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::FLOORS,
        cache_revision(db).await?,
        &(
            thread.id,
            thread.cache_revision,
            page,
            flat,
            sub_root,
            sub_page,
            // Floors store times formatted by site time zone and style when loaded; after reading
            // they are recomputed for this request instead of using that copy (refresh_post_times), and the key
            // still carries the definition.
            crate::clock::current(),
            // Posts per page and nested replies per post are settings: changing them naturally changes the key.
            paging,
        ),
    )?;
    let template = thread.clone();
    thread.posts = cache
        .get_or_fill_json(&key, || async move {
            let mut loaded = template;
            load_replies(db, &mut loaded, page, flat, sub_root, sub_page, paging).await?;
            Ok(loaded.posts)
        })
        .await?;
    refresh_post_times(&mut thread.posts)
}

/// Refreshes the live content copies during moderation or fixture initialization; reading and pagination never
/// call this statistic. The placeholder replies/root_replies/sub_replies_count do not take part in this update.
pub(crate) async fn refresh_effective_replies(
    db: &Database,
    tx: &mut Tx<'_>,
    tid: i64,
) -> Result<()> {
    // The parent floor's hidden state is read from the `root_invisible` copy rather than by joining the parent: every
    // caller changes `invisible` only through paths that rewrite the copy first (`copies::sync_root_invisible`), and
    // the content consistency check compares the two.
    let valid = "p.tid=? AND p.first=0 AND p.invisible=0 AND p.deleted_at=0 AND p.root_invisible=0";
    let count: i64 = crate::db::query_scalar(&format!(
        "SELECT COUNT(*) FROM gonof_forum_post p WHERE {valid}"
    ))
    .bind(tid)
    .fetch_one(db.observe(&mut **tx))
    .await?;
    // The excerpt takes the full text and cuts it to 255 characters in code: a single row,
    // the same statement on both backends.
    // FORCE INDEX (PRIMARY) keeps MySQL on this thread's rows: left alone, it walks the site-wide
    // `gonof_post_dateline` index backwards for the ORDER BY … LIMIT 1 and, for an old thread, reads every newer post
    // on the site first, inside this write transaction.
    let last = crate::db::query(&format!(
        "SELECT p.pid,p.dateline,p.authorid,m.username AS author,m.account_state AS author_state,p.message AS summary
         FROM gonof_forum_post p FORCE INDEX (PRIMARY)
         LEFT JOIN gonof_common_member m ON m.uid=p.authorid
         WHERE {valid} ORDER BY p.dateline DESC,p.position DESC LIMIT 1"
    ))
        .bind(tid)
        .fetch_optional(db.observe(&mut **tx))
        .await?;
    let (pid, date, author, uid, summary) = match last {
        Some(row) => {
            let state = row.try_get::<Option<String>, _>("author_state")?;
            (
                Some(row.try_get::<i64, _>("pid")?),
                row.try_get::<i64, _>("dateline")?,
                copies::last_reply_name(row.try_get("author")?, state.as_deref()),
                copies::last_reply_uid(row.try_get("authorid")?, state.as_deref()),
                row.try_get::<String, _>("summary")?
                    .chars()
                    .take(255)
                    .collect(),
            )
        }
        None => (None, 0, String::new(), None, String::new()),
    };
    // A structural change: the caller increments the site-wide version at the end of the transaction
    // (`copies::bump_site`).
    crate::db::query("UPDATE gonof_forum_thread SET valid_replies=?,last_reply_pid=?,last_reply_author=?,last_reply_uid=?,last_reply_summary=?,lastpost=GREATEST(dateline,?),cache_revision=cache_revision+1 WHERE tid=?")
        .bind(count).bind(pid).bind(author).bind(uid).bind(summary).bind(date).bind(tid).execute(db.observe(&mut **tx)).await?;
    Ok(())
}
