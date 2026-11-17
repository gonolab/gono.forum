//! The page data block (docs/skins.md, "Page data"): one `<script type="application/json" id="gonof-page">` in the
//! head of every page built on `layout/base.html`, for page scripts and skins. It carries only what the page itself
//! shows (the page's ids, its floors' positions and times, the site's own words, the records of its data-source
//! blocks as this visitor sees them, the latest threads of the board directory) and nothing personal: no `me.*` source, nothing a page shows only to its viewer.
//!
//! Version 1 only grows: a new field is added with a value on every page (`null` where it does not apply), and
//! removing or changing a field means a new `v`.
use serde::Serialize;

/// The version of the block's layout, its `v`.
const VERSION: u32 = 1;

/// The kind of page, the block's `type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    /// The site's thread list (`/`, `/page-N`).
    Home,
    /// A board's thread list (`/<slug>`).
    Board,
    Thread,
    /// A member's space (`/space/<uid>` and its tabs).
    Member,
    /// Thread search results (`/search`).
    Search,
    /// Every other page.
    #[default]
    Other,
}

/// What a handler knows about its page; the site part is added from the page frame when the block is written.
#[derive(Clone, Debug, Default)]
pub(super) struct PageData {
    kind: Kind,
    tid: Option<u64>,
    fid: Option<i64>,
    uid: Option<i64>,
    page: Option<usize>,
    thread: Option<ThreadData>,
}

/// The thread part of a thread page.
#[derive(Clone, Debug, Serialize)]
pub(super) struct ThreadData {
    /// The reply count the thread head shows.
    replies: usize,
    /// The number of pages in the view being read (flat or nested).
    pages: usize,
    /// The floors on this page in page order: the opening post first, then each reply and, in the nested view, its
    /// nested replies after it.
    floors: Vec<Floor>,
    /// The highest floor such that every floor up to it is on this page or an earlier page of the view: what opening
    /// this page counts as having read (docs/skins.md, "Unread").
    read_through: usize,
}

/// One floor of a thread page.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Floor {
    /// The floor number, the `N` of `#floor-N`.
    position: usize,
    pid: u64,
    /// The post time, in UTC (`clock::iso`).
    at: String,
}

impl Floor {
    pub(super) fn new(position: usize, pid: u64, dateline: u64) -> Self {
        Self {
            position,
            pid,
            at: crate::clock::iso(i64::try_from(dateline).unwrap_or(i64::MAX)),
        }
    }
}

/// The written block, in field order.
#[derive(Serialize)]
struct Block<'a> {
    v: u32,
    #[serde(rename = "type")]
    kind: Kind,
    tid: Option<u64>,
    fid: Option<i64>,
    uid: Option<i64>,
    page: Option<usize>,
    thread: Option<&'a ThreadData>,
    site: Site<'a>,
    /// The envelopes of the page's data-source blocks by block id (docs/data.md), as the blocks show them; empty on
    /// pages without places.
    blocks: &'a serde_json::Map<String, serde_json::Value>,
    /// The boards of the board directory a home page opens with, in its order, with their latest threads; `null` on
    /// every other page.
    directory: Option<Vec<DirectoryBoard>>,
}

/// One board of the board directory, for comparing its latest thread with what the reader saw (docs/skins.md,
/// "Unread").
#[derive(Serialize)]
struct DirectoryBoard {
    fid: i64,
    last_post: Option<DirectoryPost>,
}

/// The latest thread of a directory board: its id, last activity (UTC, as in `<time datetime>`) and highest floor.
#[derive(Serialize)]
struct DirectoryPost {
    tid: i64,
    at: String,
    max_position: i64,
}

/// The site's own words and settings that scripts need to write text and dates like the server does.
#[derive(Serialize)]
struct Site<'a> {
    /// This site's name: another domain's own name, else the site name (`site::site_name`).
    name: String,
    description: &'a str,
    /// The interface language tag (`en`, `zh-CN`).
    language: &'static str,
    /// `ltr` or `rtl`.
    dir: &'static str,
    /// The path prefix of a forum installed under a sub-path, such as `/forum`; empty at the root.
    base: String,
    /// The name of the site's points (`settings.site.credit_name`, filled with the language's default).
    credit_name: &'a str,
    /// The name of the weekly activity figure (`settings.site.index_name`, filled likewise).
    index_name: &'a str,
    /// The date style setting: `locale`, `ymd-dash`, `ymd-cn` or `mdy-slash`.
    date_format: &'a str,
}

impl PageData {
    /// The board of a board page, or of a thread page's thread.
    pub(super) fn fid(&self) -> Option<i64> {
        self.fid
    }

    /// A thread list: the home page, a board page or the search results, at `page`. `fid` is the board of a board
    /// page.
    pub(super) fn list(kind: Kind, fid: Option<i64>, page: usize) -> Self {
        Self {
            kind,
            fid,
            page: Some(page),
            ..Self::default()
        }
    }

    /// A member's space at `page` of its tab.
    pub(super) fn member(uid: i64, page: usize) -> Self {
        Self {
            kind: Kind::Member,
            uid: Some(uid),
            page: Some(page),
            ..Self::default()
        }
    }

    /// A thread page: `page` of `pages` in the view being read, with the floors it shows.
    pub(super) fn thread(
        thread: &crate::content::Thread,
        (page, pages): (usize, usize),
        floors: Vec<Floor>,
        read_through: usize,
    ) -> Self {
        Self {
            kind: Kind::Thread,
            tid: Some(thread.id),
            fid: Some(thread.fid),
            page: Some(page),
            thread: Some(ThreadData {
                replies: thread.reply_count,
                pages,
                floors,
                read_through,
            }),
            ..Self::default()
        }
    }

    /// The block's JSON for `layout/base.html`, escaped for a data block the way the JSON-LD blocks are
    /// (`syndication::script_json`), so that no value can end the element early.
    pub(super) fn json(&self, p: &super::PageMeta<'_>) -> String {
        let settings = &p.frame.settings;
        let block = Block {
            v: VERSION,
            kind: self.kind,
            tid: self.tid,
            fid: self.fid,
            uid: self.uid,
            page: self.page,
            thread: self.thread.as_ref(),
            site: Site {
                name: crate::site::site_name(settings),
                description: &settings.site.description,
                language: p.t.tag(),
                dir: if p.t.is_rtl() { "rtl" } else { "ltr" },
                base: crate::site::base().to_string(),
                credit_name: p.credit(),
                index_name: p.index_word(),
                date_format: &settings.time.date_format,
            },
            blocks: &p.places.data,
            directory: p.places.directory.as_ref().map(|directory| {
                directory
                    .items
                    .iter()
                    .map(|item| DirectoryBoard {
                        fid: item.board.fid,
                        last_post: item.board.last_post.as_ref().map(|post| DirectoryPost {
                            tid: post.tid,
                            at: post.at.at.clone(),
                            max_position: post.max_position,
                        }),
                    })
                    .collect()
            }),
        };
        // Straight to text, not through `serde_json::Value`, so the fields keep the order declared above.
        match serde_json::to_string(&block) {
            Ok(json) => super::syndication::script_escape(&json),
            Err(error) => {
                tracing::error!(error = %error, "the page data block could not be written");
                "null".into()
            }
        }
    }
}
