//! The blocks of a page's places (docs/skins.md, "Places and blocks"): which blocks a page shows, read through the
//! data sources (`router/data_sources.rs`) and rendered by their views (`templates/blocks/<source>/<view>.html`).
//!
//! A block that fails to read (its board was deleted, its thread is gone) is skipped and logged; a page never fails
//! because of a block, except that an unavailable Redis turns the request into a 503 as everywhere else. Every block,
//! shown or skipped, is recorded for the debug panel with its time.
use std::time::Instant;

use askama::Template;
use serde_json::{Map, Value};

use super::data_sources::{self, FetchError, Filters, PageContext};
use super::{AppState, AvatarView, FriendLinksCard, NavigationLink, PageMeta, list_avatar};
use crate::content::Thread;
use crate::data::{
    MAX_BLOCKS_PER_PAGE,
    params::{Days, Params, Sort},
    records::{self, Records},
};
use crate::debug::trace::LayoutSkip;
use crate::layout::{self, Block, Core, Kind, Place};
use crate::organization::ForumRecord;

/// A board page's head, which the board-head block shows (and the search results show without block hooks).
pub(super) struct BoardHead {
    pub heading: String,
    pub icon: String,
    pub follow: Option<super::social::Control>,
    pub filter: Option<super::forum_filters::Control>,
    pub description: String,
    /// `(uid, name)`: own moderators first, then those inherited from the parent board.
    pub moderators: Vec<(i64, String)>,
    pub guidance: Vec<String>,
}

/// What a page offers its blocks.
pub(super) struct Page<'a> {
    pub kind: layout::Page,
    /// The board of a board page or of a thread page's thread.
    pub board: Option<&'a ForumRecord>,
    pub thread: Option<&'a Thread>,
    /// The head of a board page, taken by its board-head block.
    pub board_head: Option<BoardHead>,
    /// The page shows the friend links (page 1 of the main domain's home page, not search).
    pub friend_links: bool,
    /// The site-wide content version the page's list already read, for the newest members.
    pub list_revision: Option<i64>,
    /// Members the viewer blocked, as the page already read them; `None` reads them only when a data-source block
    /// needs them.
    pub blocked: Option<Vec<i64>>,
}

/// One block as its view template sees it.
pub(super) struct BlockView {
    pub id: String,
    source: &'static str,
    view: &'static str,
    pub keep: bool,
    pub title: String,
    pub body: Body,
    /// Whether the outer element carries the block hooks; the member card drawn outside places has none.
    hooked: bool,
}

impl BlockView {
    /// ` block block--<source> block--<view>`, appended to the outer element's classes; the dot of a source name
    /// becomes a hyphen (`block--site-stats`), so the class needs no escaping in a selector.
    pub fn hooks(&self) -> String {
        if self.hooked {
            format!(
                " block block--{} block--{}",
                self.source.replace('.', "-"),
                self.view
            )
        } else {
            String::new()
        }
    }

    /// ` data-block="<id>"`; ids are lowercase letters, digits and hyphens, so nothing needs escaping.
    pub fn attribute(&self) -> String {
        if self.hooked {
            format!(" data-block=\"{}\"", self.id)
        } else {
            String::new()
        }
    }

    /// The member card on pages without places (`partials/user_card.html`).
    pub fn bare(body: Body) -> Self {
        Self {
            id: String::new(),
            source: "",
            view: "",
            keep: false,
            title: String::new(),
            body,
            hooked: false,
        }
    }
}

pub(super) struct SiteOverview {
    pub total: usize,
    pub replies: usize,
    pub contributors: usize,
    pub members: Vec<(String, AvatarView)>,
}

pub(super) struct ThreadsData {
    pub items: Vec<records::ThreadRecord>,
    pub hot: bool,
    /// The hot window in days, for the empty note.
    pub days: i64,
}

pub(super) struct MemberItem {
    pub name: String,
    pub avatar: AvatarView,
    pub note: String,
    pub position: i64,
}

pub(super) struct MemberGroup {
    pub heading: Option<String>,
    pub items: Vec<MemberItem>,
}

pub(super) struct MembersData {
    pub groups: Vec<MemberGroup>,
}

pub(super) struct OutlineLink {
    pub label: String,
    pub href: String,
}

pub(super) struct OutlineDate {
    pub at: records::Time,
    pub href: String,
}

pub(super) struct OutlineData {
    pub tid: i64,
    pub pages: Vec<OutlineLink>,
    pub dates: Vec<OutlineDate>,
}

pub(super) struct LinksData {
    pub items: Vec<records::Link>,
}

pub(super) struct BoardsData {
    pub items: Vec<records::BoardRecord>,
}

pub(super) struct NavLinksData {
    pub links: Vec<NavigationLink>,
}

pub(super) struct MarkdownData {
    pub html: String,
}

/// What a block shows.
pub(super) enum Body {
    MemberCard,
    RecentLocal,
    SiteOverview(SiteOverview),
    FriendLinks(FriendLinksCard),
    BoardHead(Box<BoardHead>),
    NavBoards(BoardsData),
    NavLinks(NavLinksData),
    Markdown(MarkdownData),
    Threads(ThreadsData),
    Boards(BoardsData),
    Members(MembersData),
    Stats(records::Stats),
    Links(LinksData),
    Outline(OutlineData),
}

macro_rules! view {
    ($name:ident, $path:literal) => {
        #[derive(Template)]
        #[template(path = $path)]
        struct $name<'p, 'a> {
            p: &'p PageMeta<'a>,
            block: &'p BlockView,
        }
    };
    ($name:ident, $path:literal, $data:ty) => {
        #[derive(Template)]
        #[template(path = $path)]
        struct $name<'p, 'a> {
            // Every view takes the page; some only read their data.
            #[allow(dead_code)]
            p: &'p PageMeta<'a>,
            block: &'p BlockView,
            data: &'p $data,
        }
    };
}

view!(MemberCardView, "blocks/member-card/card.html");
view!(RecentLocalView, "blocks/recent-local/list.html");
view!(
    SiteOverviewView,
    "blocks/site-overview/card.html",
    SiteOverview
);
view!(
    FriendLinksView,
    "blocks/friend-links/list.html",
    FriendLinksCard
);
view!(BoardHeadView, "blocks/board-head/head.html", BoardHead);
view!(NavBoardsView, "blocks/nav-boards/tree.html", BoardsData);
view!(NavLinksView, "blocks/nav-links/list.html", NavLinksData);
view!(MarkdownView, "blocks/markdown/prose.html", MarkdownData);
view!(
    ThreadsMiniList,
    "blocks/threads/mini-list.html",
    ThreadsData
);
view!(ThreadsList, "blocks/threads/list.html", ThreadsData);
view!(ThreadsCards, "blocks/threads/cards.html", ThreadsData);
view!(BoardsTree, "blocks/boards/tree.html", BoardsData);
view!(BoardsGrid, "blocks/boards/grid.html", BoardsData);
view!(BoardsDirectory, "blocks/boards/directory.html", BoardsData);
view!(MembersAvatars, "blocks/members/avatars.html", MembersData);
view!(MembersList, "blocks/members/list.html", MembersData);
view!(MembersTable, "blocks/members/table.html", MembersData);
view!(
    StatsFigures,
    "blocks/site.stats/figures.html",
    records::Stats
);
view!(
    StatsCompact,
    "blocks/site.stats/compact.html",
    records::Stats
);
view!(LinksView, "blocks/site.links/links.html", LinksData);
view!(
    OutlineTimeline,
    "blocks/thread.outline/timeline.html",
    OutlineData
);
view!(
    OutlineProgress,
    "blocks/thread.outline/progress.html",
    OutlineData
);

/// The block rendered by its view. The member sources share `blocks/members/<view>.html`, which their own
/// `blocks/<source>/<view>.html` include.
pub(super) fn render(p: &PageMeta<'_>, block: &BlockView) -> askama::Result<String> {
    match (&block.body, block.view) {
        (Body::MemberCard, _) => MemberCardView { p, block }.render(),
        (Body::RecentLocal, _) => RecentLocalView { p, block }.render(),
        (Body::SiteOverview(data), _) => SiteOverviewView { p, block, data }.render(),
        (Body::FriendLinks(data), _) => FriendLinksView { p, block, data }.render(),
        (Body::BoardHead(data), _) => BoardHeadView {
            p,
            block,
            data: data.as_ref(),
        }
        .render(),
        (Body::NavBoards(data), _) => NavBoardsView { p, block, data }.render(),
        (Body::NavLinks(data), _) => NavLinksView { p, block, data }.render(),
        (Body::Markdown(data), _) => MarkdownView { p, block, data }.render(),
        (Body::Threads(data), "list") => ThreadsList { p, block, data }.render(),
        (Body::Threads(data), "cards") => ThreadsCards { p, block, data }.render(),
        (Body::Threads(data), _) => ThreadsMiniList { p, block, data }.render(),
        (Body::Boards(data), "grid") => BoardsGrid { p, block, data }.render(),
        (Body::Boards(data), "directory") => BoardsDirectory { p, block, data }.render(),
        (Body::Boards(data), _) => BoardsTree { p, block, data }.render(),
        (Body::Members(data), "list") => MembersList { p, block, data }.render(),
        (Body::Members(data), "table") => MembersTable { p, block, data }.render(),
        (Body::Members(data), _) => MembersAvatars { p, block, data }.render(),
        (Body::Stats(data), "compact") => StatsCompact { p, block, data }.render(),
        (Body::Stats(data), _) => StatsFigures { p, block, data }.render(),
        (Body::Links(data), _) => LinksView { p, block, data }.render(),
        (Body::Outline(data), "progress") => OutlineProgress { p, block, data }.render(),
        (Body::Outline(data), _) => OutlineTimeline { p, block, data }.render(),
    }
}

/// The rendered blocks of a page, by where they go.
#[derive(Default)]
pub(super) struct Placed {
    pub aside: Vec<String>,
    pub head: Vec<String>,
    pub footer: Vec<String>,
    pub side: Vec<String>,
    /// The page head shows the board-head block, so the page writes no heading of its own.
    pub board_head: bool,
    /// The envelopes of the page's data-source blocks, for the page data block.
    pub data: Map<String, Value>,
}

impl Placed {
    fn slot(&mut self, place: Place) -> &mut Vec<String> {
        match place {
            Place::HomeAside | Place::BoardAside | Place::ThreadAside => &mut self.aside,
            Place::HeadHome | Place::HeadBoard | Place::HeadThread => &mut self.head,
            Place::Footer => &mut self.footer,
            Place::SideNav => &mut self.side,
        }
    }
}

/// The blocks of `place` for this request: the administrator's draft while previewing it, then the domain's
/// document, the site's, the skin's manifest and the core default (`layout::resolve`).
pub(super) fn blocks_of(state: &AppState, place: Place) -> &[Block] {
    let frame = &state.frame;
    layout::resolve(
        place,
        frame.domain_places.as_ref(),
        frame.site_places.as_ref(),
        frame.skin.places.as_ref(),
    )
    .0
}

/// Why a block shows nothing.
enum Skip {
    Kind(LayoutSkip),
    Unavailable(anyhow::Error),
}

/// Reads and renders the page's blocks; `Err` only when Redis is unavailable (the page answers 503).
pub(super) async fn place(
    state: &AppState,
    p: &PageMeta<'_>,
    mut page: Page<'_>,
) -> Result<Placed, anyhow::Error> {
    let signed_in = state.viewer.session.is_some();
    let side = state.frame.skin.manifest.frame.nav == crate::skins::manifest::Nav::Side;
    let sources = page
        .kind
        .places()
        .into_iter()
        .filter(|place| *place != Place::SideNav || side)
        .flat_map(|place| blocks_of(state, place))
        .any(|block| matches!(block.kind, Kind::Source(_) | Kind::Core(Core::NavBoards)));
    let blocked = match (page.blocked.take(), &state.viewer.session) {
        (Some(blocked), _) => blocked,
        (None, Some(session)) if sources => {
            crate::content::blocked_authors(&state.db, session.member.uid).await?
        }
        (None, _) => Vec::new(),
    };
    let filters = Filters::with_blocked(state, blocked);
    let profile = crate::debug::trace::current();
    let mut placed = Placed::default();
    let mut shown = 0;
    for place in page.kind.places() {
        if place == Place::SideNav && !side {
            continue;
        }
        for block in blocks_of(state, place) {
            let started = Instant::now();
            let outcome = if !block.show.shows(signed_in) {
                Err(Skip::Kind(LayoutSkip::Audience))
            } else if shown >= MAX_BLOCKS_PER_PAGE {
                Err(Skip::Kind(LayoutSkip::Budget))
            } else {
                read(state, &filters, &mut page, block).await
            };
            let outcome = outcome.and_then(|(view, envelope)| {
                render(p, &view).map(|html| (html, envelope)).map_err(|error| {
                    tracing::error!(block = %block.id, %error, "a block failed to render; skipped");
                    Skip::Kind(LayoutSkip::Failed)
                })
            });
            let skipped = match outcome {
                Ok((html, envelope)) => {
                    shown += 1;
                    if block.kind == Kind::Core(Core::BoardHead) {
                        placed.board_head = true;
                    }
                    placed.slot(place).push(html);
                    if let Some(envelope) = envelope {
                        placed.data.insert(block.id.clone(), envelope);
                    }
                    None
                }
                Err(Skip::Unavailable(error)) => return Err(error),
                Err(Skip::Kind(kind)) => Some(kind),
            };
            if let Some(profile) = &profile {
                profile.record_layout(
                    place.name(),
                    &block.id,
                    block.kind.name(),
                    started.elapsed(),
                    skipped,
                );
            }
        }
    }
    Ok(placed)
}

/// The view of one block and, for a data source, its envelope.
async fn read(
    state: &AppState,
    filters: &Filters,
    page: &mut Page<'_>,
    block: &Block,
) -> Result<(BlockView, Option<Value>), Skip> {
    let t = crate::i18n::current();
    let view = |body| BlockView {
        id: block.id.clone(),
        source: block.kind.name(),
        view: block.view,
        keep: block.keep_on_phone,
        title: block
            .title
            .as_ref()
            .map(|title| title.text(t))
            .unwrap_or_default(),
        body,
        hooked: true,
    };
    let failed = |error: anyhow::Error| {
        if error.downcast_ref::<crate::cache::Unavailable>().is_some() {
            return Skip::Unavailable(error);
        }
        tracing::error!(block = %block.id, error = %format!("{error:#}"), "a block failed to read; skipped");
        Skip::Kind(LayoutSkip::Failed)
    };
    let body = match block.kind {
        Kind::Core(Core::MemberCard) => Body::MemberCard,
        Kind::Core(Core::RecentLocal) => Body::RecentLocal,
        Kind::Core(Core::SiteOverview) => {
            Body::SiteOverview(site_overview(state, page).await.map_err(failed)?)
        }
        Kind::Core(Core::FriendLinks) => {
            if !page.friend_links {
                return Err(Skip::Kind(LayoutSkip::Empty));
            }
            let links = state
                .global
                .home_links(&state.db, state.cache_ttl())
                .await
                .map_err(failed)?;
            Body::FriendLinks(FriendLinksCard {
                // The cache stores up to the hard limit; the page takes `settings.links.home_count` of them.
                links: links
                    .iter()
                    .take(usize::try_from(state.frame.settings.links.home_count).unwrap_or(0))
                    .cloned()
                    .collect(),
                applications_open: state.frame.settings.links.applications,
            })
        }
        Kind::Core(Core::BoardHead) => match page.board_head.take() {
            Some(head) => Body::BoardHead(Box::new(head)),
            None => return Err(Skip::Kind(LayoutSkip::Empty)),
        },
        Kind::Core(Core::NavBoards) => {
            let params = Params::Boards(crate::data::params::Boards {
                parent: crate::data::params::Parent::Top,
                counts: false,
                last_post: false,
            });
            match data_sources::fetch(state, filters, PageContext::default(), &params).await {
                Ok(records::Output {
                    records: Records::Boards(items),
                    ..
                }) => Body::NavBoards(BoardsData { items }),
                Ok(_) => return Err(Skip::Kind(LayoutSkip::Empty)),
                Err(error) => return Err(fetch_skip(block, error)),
            }
        }
        Kind::Core(Core::NavLinks) => Body::NavLinks(NavLinksData {
            links: super::highlighted(state, page.board.map_or("", |board| board.slug.as_str())),
        }),
        Kind::Core(Core::Markdown) => {
            let source = block.body.as_ref().map_or("", |body| body.get(t.tag()));
            Body::Markdown(MarkdownData {
                html: crate::markdown::render(source, &std::collections::BTreeMap::new(), None),
            })
        }
        Kind::Source(source) => {
            let Some(params) = block.params.and_then(|params| {
                data_sources::resolve(
                    params,
                    data_sources::PageContext {
                        board: page.board,
                        thread: page.thread,
                    },
                )
            }) else {
                return Err(Skip::Kind(LayoutSkip::Empty));
            };
            let output = data_sources::fetch(
                state,
                filters,
                PageContext {
                    board: page.board,
                    thread: page.thread,
                },
                &params,
            )
            .await
            .map_err(|error| fetch_skip(block, error))?;
            let envelope = (!source.personal())
                .then(|| records::envelope(source, params.json(), &output, crate::identity::now()));
            return Ok((view(source_body(state, &params, output)), envelope));
        }
    };
    Ok((view(body), None))
}

fn fetch_skip(block: &Block, error: FetchError) -> Skip {
    match error {
        FetchError::Internal(error)
            if error.downcast_ref::<crate::cache::Unavailable>().is_some() =>
        {
            Skip::Unavailable(error)
        }
        FetchError::Internal(error) => {
            tracing::error!(block = %block.id, error = %format!("{error:#}"), "a block failed to read; skipped");
            Skip::Kind(LayoutSkip::Failed)
        }
        FetchError::NotFound | FetchError::Unauthorized => {
            tracing::error!(block = %block.id, "a block's board, thread or group no longer exists or cannot be read; skipped");
            Skip::Kind(LayoutSkip::Failed)
        }
    }
}

async fn site_overview(state: &AppState, page: &Page<'_>) -> anyhow::Result<SiteOverview> {
    let now = crate::identity::now();
    let statistics = crate::content::layered_statistics_in(
        &state.db,
        &state.global,
        &state.timed_cache(),
        (now, state.frame.settings.time.offset_minutes),
        &state.frame.statistics_fids(),
    )
    .await?;
    let members = crate::content::home::layered_recent_members(
        &state.db,
        &state.global,
        &state.timed_cache(),
        state
            .viewer
            .session
            .as_ref()
            .map(|session| session.member.uid),
        (now, state.frame.settings.display.recent_members),
        page.list_revision,
    )
    .await?;
    Ok(SiteOverview {
        total: statistics.total,
        replies: statistics.replies,
        contributors: statistics.contributors,
        // Avatar by uid: the anonymous copy goes through the cache, changing an avatar does not bump the site-wide
        // version, and an address with an aid would go stale.
        members: members
            .into_iter()
            .map(|member| {
                let avatar = list_avatar(Some(member.uid), &member.username);
                (member.username, avatar)
            })
            .collect(),
    })
}

fn member_item(member: &records::MemberRef, note: String, position: i64) -> MemberItem {
    MemberItem {
        name: member.name.clone(),
        avatar: list_avatar(member.uid, &member.name),
        note,
        position,
    }
}

/// A data source's records as its views take them.
fn source_body(state: &AppState, params: &Params, output: records::Output) -> Body {
    let one = |items: Vec<MemberItem>| {
        Body::Members(MembersData {
            groups: vec![MemberGroup {
                heading: None,
                items,
            }],
        })
    };
    match output.records {
        Records::Threads(items) => {
            let (hot, days) = match params {
                Params::Threads(threads) if threads.sort == Sort::Hot => (
                    true,
                    match threads.days {
                        Some(Days::Days(days)) => days,
                        _ => state.frame.settings.display.hot_days,
                    },
                ),
                _ => (false, 0),
            };
            Body::Threads(ThreadsData { items, hot, days })
        }
        Records::Boards(items) => Body::Boards(BoardsData { items }),
        Records::Recent(items) => one(items
            .iter()
            .map(|item| member_item(&item.member, item.joined.text.clone(), 0))
            .collect()),
        Records::Ranked(items) => one(items
            .iter()
            .map(|item| member_item(&item.member, item.score.to_string(), item.position))
            .collect()),
        Records::Honors(groups) => Body::Members(MembersData {
            groups: groups
                .iter()
                .map(|group| MemberGroup {
                    heading: Some(group.honor.name.clone()),
                    items: group
                        .members
                        .iter()
                        .map(|member| member_item(member, String::new(), 0))
                        .collect(),
                })
                .collect(),
        }),
        Records::Verified(groups) => Body::Members(MembersData {
            groups: groups
                .iter()
                .map(|group| MemberGroup {
                    heading: Some(group.name.clone()),
                    items: group
                        .members
                        .iter()
                        .map(|member| {
                            let host = member
                                .site
                                .as_ref()
                                .map(|site| site.host.clone())
                                .unwrap_or_default();
                            member_item(&member.member, host, 0)
                        })
                        .collect(),
                })
                .collect(),
        }),
        Records::Moderators(items) => one(items
            .iter()
            .map(|item| {
                let note = if item.role == "trainee" {
                    crate::i18n::current().block_moderator_trainee().to_owned()
                } else {
                    String::new()
                };
                member_item(&item.member, note, 0)
            })
            .collect()),
        Records::Stats(mut items) => Body::Stats(items.pop().unwrap_or(records::Stats {
            threads: None,
            replies: None,
            contributors: None,
            today: None,
        })),
        Records::Links(items) => Body::Links(LinksData { items }),
        Records::Outline(mut items) => {
            let outline = items.pop();
            let t = crate::i18n::current();
            let tid = outline.as_ref().map_or(0, |outline| outline.tid);
            let href = |page: i64, position: Option<i64>| {
                let path = crate::urls::thread(
                    u64::try_from(tid).unwrap_or(0),
                    usize::try_from(page).unwrap_or(1).max(1),
                    true,
                );
                crate::site::href(match position {
                    Some(position) => format!("{path}#floor-{position}"),
                    None => path,
                })
            };
            Body::Outline(OutlineData {
                tid,
                pages: outline
                    .iter()
                    .flat_map(|outline| &outline.page_marks)
                    .map(|mark| OutlineLink {
                        label: t.block_outline_page(mark.page, mark.from, mark.to),
                        href: href(mark.page, None),
                    })
                    .collect(),
                dates: outline
                    .iter()
                    .flat_map(|outline| &outline.date_marks)
                    .map(|mark| OutlineDate {
                        at: mark.at.clone(),
                        href: href(mark.page, Some(mark.position)),
                    })
                    .collect(),
            })
        }
        // The personal sources have no views, so layouts refuse them; nothing to show.
        Records::Card(_) | Records::Counts(_) | Records::Checkin(_) => {
            Body::Links(LinksData { items: Vec::new() })
        }
    }
}
