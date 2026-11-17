//! Fetching the data sources (docs/data.md) for the three outputs that share their records: the blocks of a layout
//! (`router/blocks.rs`), the page data block's `blocks`, and `GET /data/v1/<source>` (here).
//!
//! Every source reads as the visitor: this site's boards they may read, minus the boards they hid or blocked
//! (`SiteFrame::listable_forums`), minus the members they blocked (`content::blocked_authors`), anonymous visitors
//! only public threads (`content::ListViewer`). A board or thread the visitor cannot read is "not found".
use std::collections::HashSet;

use axum::{
    extract::{OriginalUri, Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

use super::{AppState, auth};
use crate::content::{self, ListScope, ListViewer, Thread};
use crate::data::{
    Source,
    params::{self, Days, Limit, LinkPlace, Params, Parent, Scope, Sort, Target},
    records::{self, BoardRef, MemberRef, Output, Records, Time},
};
use crate::organization::ForumRecord;

pub(super) const PREFIX: &str = "/data/v1/";

/// Why a source returned nothing.
#[derive(Debug)]
pub(super) enum FetchError {
    /// The board, thread or honor group does not exist or the visitor cannot read it.
    NotFound,
    /// A personal source without a session.
    Unauthorized,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for FetchError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

/// What the visitor filters out, read once per request and shared by every source on a page.
pub(super) struct Filters {
    /// Members the visitor blocked.
    pub blocked: Vec<i64>,
    /// This site's boards the visitor may read, minus those they hid from the timeline or blocked.
    pub listable: Vec<i64>,
}

impl Filters {
    pub async fn load(state: &AppState) -> anyhow::Result<Self> {
        let blocked = match &state.viewer.session {
            Some(session) => content::blocked_authors(&state.db, session.member.uid).await?,
            None => Vec::new(),
        };
        Ok(Self::with_blocked(state, blocked))
    }

    /// With the blocked members a page already read.
    pub fn with_blocked(state: &AppState, blocked: Vec<i64>) -> Self {
        let excluded = state.frame.forum_filters.timeline_excluded();
        Self {
            blocked,
            listable: state.frame.listable_forums(&excluded),
        }
    }

    fn blocks(&self, uid: Option<i64>) -> bool {
        uid.is_some_and(|uid| self.blocked.contains(&uid))
    }
}

/// What `current` means on a page: the board of a board page or of a thread page's thread, and the thread (whose head
/// `thread.outline` then need not read).
#[derive(Clone, Copy, Default)]
pub(super) struct PageContext<'a> {
    pub board: Option<&'a ForumRecord>,
    pub thread: Option<&'a Thread>,
}

/// Replaces `current` by the page's own board or thread; `None` when the page has none (a `moderators` block with
/// `board: current` on the home page shows nothing).
pub(super) fn resolve(params: Params, page: PageContext<'_>) -> Option<Params> {
    let scope = |scope: Scope| match scope {
        Scope::Current => Some(
            page.board
                .map_or(Scope::Site, |board| Scope::Board(board.fid)),
        ),
        other => Some(other),
    };
    let board = |target: Target| match target {
        Target::Current => page.board.map(|board| Target::Id(board.fid)),
        other => Some(other),
    };
    Some(match params {
        Params::Threads(threads) => Params::Threads(params::Threads {
            scope: scope(threads.scope)?,
            ..threads
        }),
        Params::SiteStats { scope: value } => Params::SiteStats {
            scope: scope(value)?,
        },
        Params::Moderators { board: target } => Params::Moderators {
            board: board(target)?,
        },
        Params::ThreadOutline { tid } => Params::ThreadOutline {
            tid: match tid {
                Target::Current => Target::Id(i64::try_from(page.thread?.id).ok()?),
                other => other,
            },
        },
        other => other,
    })
}

/// The boards of a scope: all of this site's listable boards, or one board with its readable child boards that the
/// member did not block (a child board alone), as board pages list them.
fn scope_boards(
    state: &AppState,
    filters: &Filters,
    scope: Scope,
) -> Result<(ListScope, Vec<i64>), FetchError> {
    match scope {
        Scope::Site | Scope::Current => Ok((ListScope::All, filters.listable.clone())),
        Scope::Board(fid) => {
            let forum = state
                .frame
                .forum_by_fid(fid)
                .filter(|forum| !state.frame.forum_filters.is_blocked(forum.fid))
                .ok_or(FetchError::NotFound)?;
            let mut fids = vec![forum.fid];
            if forum.parent_fid.is_none() {
                fids.extend(
                    state
                        .frame
                        .forums
                        .iter()
                        .filter(|child| {
                            child.parent_fid == Some(forum.fid)
                                && !state.frame.forum_filters.is_blocked(child.fid)
                        })
                        .map(|child| child.fid),
                );
            }
            Ok((ListScope::of_forum(forum.fid, forum.parent_fid), fids))
        }
    }
}

/// Reads one source with resolved parameters ([`resolve`]).
pub(super) async fn fetch(
    state: &AppState,
    filters: &Filters,
    page: PageContext<'_>,
    params: &Params,
) -> Result<Output, FetchError> {
    let now = crate::identity::now();
    let settings = &state.frame.settings;
    let ttl = state.cache_ttl();
    let uid = state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.uid);
    match *params {
        Params::Threads(threads) => {
            let (scope, forums) = scope_boards(state, filters, threads.scope)?;
            let sort = match threads.sort {
                Sort::Latest => content::sources::SourceSort::Latest,
                Sort::Created => content::sources::SourceSort::Created,
                Sort::Featured => content::sources::SourceSort::Featured,
                Sort::Pinned => content::sources::SourceSort::Pinned,
                Sort::Hot => {
                    let days = match threads.days {
                        Some(Days::Days(days)) => days,
                        _ => settings.display.hot_days,
                    };
                    content::sources::SourceSort::Hot {
                        since: now.saturating_sub(days.saturating_mul(86_400)),
                    }
                }
            };
            let limit = match threads.limit {
                Limit::Count(limit) => limit,
                Limit::Site if threads.sort == Sort::Hot => settings.display.hot_count,
                Limit::Site => settings.display.latest_count,
            };
            let list = content::sources::SourceList {
                scope: &scope,
                sort,
                category: threads.category,
                limit,
                thumbnails: settings.display.list_thumbnails > 0,
            };
            let viewer = ListViewer {
                uid,
                forums: &forums,
                blocked_uids: &filters.blocked,
            };
            let threads =
                content::sources::threads(&state.db, &state.timed_cache(), list, viewer, now)
                    .await?;
            Ok(Output::new(Records::Threads(
                threads
                    .iter()
                    .map(|thread| thread_record(state, thread))
                    .collect(),
            )))
        }
        Params::Boards(boards) => boards_records(state, filters, boards).await,
        Params::MembersRecent { limit } => {
            let members = content::home::layered_recent_members(
                &state.db,
                &state.global,
                &state.timed_cache(),
                uid,
                (now, limit),
                None,
            )
            .await?;
            Ok(Output::new(Records::Recent(
                members
                    .iter()
                    .filter(|member| !filters.blocks(Some(member.uid)))
                    .map(|member| records::RecentMember {
                        member: MemberRef::of(Some(member.uid), &member.username),
                        joined: Time::of(member.regdate),
                    })
                    .collect(),
            )))
        }
        Params::MembersRank(rank) => {
            let signed_in = uid.is_some();
            let segment = list_segment(state, now);
            let db = state.db.clone();
            let mut snapshot: crate::ranks::Snapshot = class_cached(
                state,
                &(
                    "rank",
                    rank.board.slug(),
                    rank.period.slug(),
                    signed_in,
                    segment,
                ),
                || async move {
                    crate::ranks::read_listed(&db, rank.board, rank.period, signed_in).await
                },
            )
            .await?;
            snapshot.without(&filters.blocked.iter().copied().collect::<HashSet<_>>());
            Ok(Output {
                records: Records::Ranked(
                    snapshot
                        .standings
                        .iter()
                        .take(usize::try_from(rank.limit).unwrap_or(0))
                        .map(|standing| records::Ranked {
                            position: i64::try_from(standing.rank).unwrap_or(0),
                            member: MemberRef::of(Some(standing.uid), &standing.username),
                            score: standing.score,
                        })
                        .collect(),
                ),
                computed_at: snapshot.computed_at.map(Time::of),
            })
        }
        Params::MembersHonors { honor, limit } => {
            let groups = state.global.honors(&state.db, ttl).await?;
            let chosen: Vec<&crate::honors::Honor> = match honor {
                Some(id) => vec![
                    groups
                        .iter()
                        .find(|group| group.id == id && group.enabled)
                        .ok_or(FetchError::NotFound)?,
                ],
                None => groups
                    .iter()
                    .filter(|group| group.enabled)
                    .take(MAX_GROUPS)
                    .collect(),
            };
            let signed_in = uid.is_some();
            let segment = list_segment(state, now);
            let fetched = limit + i64::try_from(filters.blocked.len().min(20)).unwrap_or(0);
            let mut items = Vec::new();
            for group in chosen {
                let db = state.db.clone();
                let id = group.id;
                let holders: Vec<crate::honors::Listed> = class_cached(
                    state,
                    &("honors", id, signed_in, fetched, segment),
                    || async move {
                        crate::honors::listed_holders(&db, id, signed_in, now, fetched).await
                    },
                )
                .await?;
                items.push(records::HonorGroup {
                    honor: records::HonorRef {
                        id: group.id,
                        name: group.name.clone(),
                        description: group.description.clone(),
                    },
                    members: holders
                        .iter()
                        .filter(|holder| !filters.blocks(Some(holder.uid)))
                        .take(usize::try_from(limit).unwrap_or(0))
                        .map(|holder| MemberRef::of(Some(holder.uid), &holder.username))
                        .collect(),
                });
            }
            Ok(Output::new(Records::Honors(items)))
        }
        Params::MembersVerified { kind, limit } => {
            let signed_in = uid.is_some();
            let segment = list_segment(state, now);
            let fetched = limit + i64::try_from(filters.blocked.len().min(20)).unwrap_or(0);
            let categories: Vec<crate::directory::Category> = match kind {
                Some(kind) => vec![kind],
                None => crate::directory::Category::ALL.to_vec(),
            };
            let mut items = Vec::new();
            for category in categories {
                let db = state.db.clone();
                let listed: Vec<crate::directory::Listed> = class_cached(
                    state,
                    &("verified", category.slug(), signed_in, fetched, segment),
                    || async move {
                        Ok(
                            crate::directory::listed(&db, category, signed_in, now, fetched)
                                .await?,
                        )
                    },
                )
                .await?;
                items.push(records::VerifiedGroup {
                    kind: category.slug(),
                    name: category.label().to_owned(),
                    members: listed
                        .iter()
                        .filter(|member| !filters.blocks(Some(member.uid)))
                        .take(usize::try_from(limit).unwrap_or(0))
                        .map(|member| records::VerifiedMember {
                            member: MemberRef::of(Some(member.uid), &member.username),
                            site: member.site_url.as_ref().map(|_| records::Site {
                                href: member.site_href().to_owned(),
                                host: member.site_host(),
                            }),
                        })
                        .collect(),
                });
            }
            Ok(Output::new(Records::Verified(items)))
        }
        Params::Moderators { board } => {
            let Target::Id(fid) = board else {
                return Ok(Output::new(Records::Moderators(Vec::new())));
            };
            let forum = state.frame.forum_by_fid(fid).ok_or(FetchError::NotFound)?;
            let roster = state.global.moderators(&state.db, ttl).await?;
            Ok(Output::new(Records::Moderators(
                crate::organization::forum_moderators(&roster, forum.fid, forum.parent_fid)
                    .into_iter()
                    .map(|entry| records::Moderator {
                        member: MemberRef::of(Some(entry.uid), &entry.username),
                        role: if entry.adminid == crate::identity::ROLE_INTERN {
                            "trainee"
                        } else {
                            "moderator"
                        },
                        inherited: entry.fid != forum.fid,
                    })
                    .collect(),
            )))
        }
        Params::SiteStats { scope } => {
            let statistics = state.forum_statistics().await?;
            let stats = match scope {
                Scope::Board(fid) => {
                    let forum = state.frame.forum_by_fid(fid).ok_or(FetchError::NotFound)?;
                    let count = statistics.total(family_slugs(&state.frame, forum));
                    records::Stats {
                        threads: Some(count.threads),
                        replies: None,
                        contributors: None,
                        today: Some(count.today),
                    }
                }
                _ => {
                    let site = content::layered_statistics_in(
                        &state.db,
                        &state.global,
                        &state.timed_cache(),
                        (now, settings.time.offset_minutes),
                        &state.frame.statistics_fids(),
                    )
                    .await?;
                    let today = statistics
                        .total(state.frame.forums.iter().map(|forum| forum.slug.as_str()))
                        .today;
                    records::Stats {
                        threads: Some(i64::try_from(site.total).unwrap_or(0)),
                        replies: Some(i64::try_from(site.replies).unwrap_or(0)),
                        contributors: Some(i64::try_from(site.contributors).unwrap_or(0)),
                        today: Some(today),
                    }
                }
            };
            Ok(Output::new(Records::Stats(vec![stats])))
        }
        Params::SiteLinks { place } => {
            let links = match place {
                // Friend links belong to the main site: other domains list none, as their home pages show none.
                LinkPlace::Home if crate::site::other(|_| ()).is_some() => Vec::new(),
                LinkPlace::Home => state
                    .global
                    .home_links(&state.db, ttl)
                    .await?
                    .iter()
                    .take(usize::try_from(settings.links.home_count).unwrap_or(0))
                    .map(|link| records::Link {
                        name: link.name.clone(),
                        url: link.url.clone(),
                        domain: None,
                        description: None,
                        category: None,
                    })
                    .collect(),
                LinkPlace::Directory => state
                    .global
                    .link_directory(&state.db, ttl)
                    .await?
                    .iter()
                    .flat_map(|(category, links)| {
                        links.iter().map(move |link| records::Link {
                            name: link.name.clone(),
                            url: link.url.clone(),
                            domain: Some(link.domain.clone()),
                            description: Some(link.description.clone()),
                            category: Some(category.slug().to_owned()),
                        })
                    })
                    .collect(),
            };
            Ok(Output::new(Records::Links(links)))
        }
        Params::ThreadOutline { tid } => {
            let Target::Id(tid) = tid else {
                return Err(FetchError::NotFound);
            };
            outline(state, page, tid).await
        }
        Params::Me => Err(FetchError::Unauthorized),
    }
}

/// At most this many honor groups when a block lists every group.
const MAX_GROUPS: usize = 10;

/// The slugs of a board and, for a top-level board, its readable child boards: what board statistics add up.
fn family_slugs<'a>(frame: &'a super::frame::SiteFrame, forum: &'a ForumRecord) -> Vec<&'a str> {
    std::iter::once(forum.slug.as_str())
        .chain(
            frame
                .forums
                .iter()
                .filter(|child| forum.parent_fid.is_none() && child.parent_fid == Some(forum.fid))
                .map(|child| child.slug.as_str()),
        )
        .collect()
}

/// The list-lifetime segment the member lists are cached by.
fn list_segment(state: &AppState, now: i64) -> i64 {
    now.div_euclid(
        i64::try_from(state.timed_cache().lifetimes().list)
            .unwrap_or(i64::MAX)
            .max(1),
    )
}

/// A member list one copy of which serves a visitor class (`data-members`): from Redis when it is on, read
/// otherwise.
async fn class_cached<T, F, Fut>(
    state: &AppState,
    variant: &impl serde::Serialize,
    fill: F,
) -> anyhow::Result<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let cache = state.timed_cache();
    if !cache.cache().enabled() {
        return fill().await;
    }
    let key = crate::cache::CacheKey::new(&crate::cache::registry::DATA_MEMBERS, 0, variant)?;
    cache.get_or_fill_json(&key, fill).await
}

/// One thread as a record.
pub(super) fn thread_record(state: &AppState, thread: &Thread) -> records::ThreadRecord {
    let forum = state.frame.any_forum_by_fid(thread.fid);
    let members_forum = forum.is_some_and(|forum| {
        forum.visibility == "members"
            || forum
                .parent_fid
                .and_then(|parent| state.frame.any_forum_by_fid(parent))
                .is_some_and(|parent| parent.visibility == "members")
    });
    let paging = crate::urls::Paging::from_settings(&state.frame.settings);
    let replies = if state.frame.reading.flat {
        thread.reply_count
    } else {
        thread.root_replies
    };
    let tid = i64::try_from(thread.id).unwrap_or(0);
    records::ThreadRecord {
        tid,
        title: thread.title.clone(),
        href: crate::site::href(format!("/thread-{tid}")),
        board: forum.map(BoardRef::of),
        kind: Some(thread.kind_code()).filter(|kind| !kind.is_empty()),
        pin: thread.pin,
        featured: thread.featured,
        closed: thread.closed,
        members_only: thread.members_only || members_forum,
        author: MemberRef::of(
            thread.authorid.filter(|_| !thread.author_gone),
            &thread.author,
        ),
        created: Time::of(i64::try_from(thread.created).unwrap_or(0)),
        last_activity: Time::of(i64::try_from(thread.updated).unwrap_or(0)),
        replies: i64::try_from(thread.reply_count).unwrap_or(0),
        favorites: thread.favorites,
        tips: thread.tips,
        last_reply: thread.last_reply_pid.map(|pid| records::LastReply {
            pid,
            member: MemberRef::of(thread.last_reply_uid, &thread.last_reply_author),
            href: crate::site::href(format!("/posts/{pid}")),
            summary: crate::markdown::card_excerpt(&thread.last_reply_summary, SUMMARY_CHARS),
        }),
        pages: i64::try_from(replies.div_ceil(paging.replies).max(1)).unwrap_or(1),
        max_position: thread.max_position,
        views: i64::try_from(thread.views).unwrap_or(0),
        thumbs: super::list_thumbs(state, thread)
            .into_iter()
            .map(|thumb| records::Thumb {
                src: crate::site::href(&thumb.url),
                width: thumb.width,
                height: thumb.height,
            })
            .collect(),
    }
}

/// Characters of a reply's excerpt in records.
const SUMMARY_CHARS: usize = 120;

async fn boards_records(
    state: &AppState,
    filters: &Filters,
    params: params::Boards,
) -> Result<Output, FetchError> {
    let frame = &state.frame;
    let parent = match params.parent {
        Parent::Top => None,
        Parent::Board(fid) => Some(
            frame
                .forum_by_fid(fid)
                .filter(|forum| forum.parent_fid.is_none() && !frame.forum_filters.is_blocked(fid))
                .ok_or(FetchError::NotFound)?,
        ),
    };
    let statistics = if params.counts {
        Some(state.forum_statistics().await?)
    } else {
        None
    };
    let last_posts = if params.last_post {
        let members = state.viewer.session.is_some();
        let boards: Vec<(i64, bool)> = listed(frame, parent)
            .map(|forum| (forum.fid, forum.parent_fid.is_none()))
            .collect();
        let fids: Vec<i64> = frame.forums.iter().map(|forum| forum.fid).collect();
        let revision = content::list_revision(&state.db).await?;
        let db = state.db.clone();
        let (key_boards, key_fids) = (boards.clone(), fids.clone());
        state
            .global
            .board_last_posts(members, (revision, key_boards, key_fids), || async move {
                content::sources::board_last_posts(&db, &boards, &fids, !members).await
            })
            .await?
    } else {
        Vec::new()
    };
    Ok(Output::new(Records::Boards(board_items(
        frame,
        filters,
        parent,
        statistics.as_ref(),
        &last_posts,
    ))))
}

/// The boards under `parent` (the top-level boards for `None`) the visitor sees, in display order: not blocked.
fn listed<'a>(
    frame: &'a super::frame::SiteFrame,
    parent: Option<&'a ForumRecord>,
) -> impl Iterator<Item = &'a ForumRecord> {
    frame.forums.iter().filter(move |forum| {
        forum.parent_fid == parent.map(|parent| parent.fid)
            && !frame.forum_filters.is_blocked(forum.fid)
    })
}

/// The top-level boards with their child boards as the side navigation's board tree shows them: what the page frame
/// holds for this visitor, without counts or last posts, so no statement runs.
pub(super) fn nav_boards(frame: &super::frame::SiteFrame) -> Vec<records::BoardRecord> {
    let filters = Filters {
        blocked: Vec::new(),
        listable: Vec::new(),
    };
    board_items(frame, &filters, None, None, &[])
}

/// The records of the boards under `parent`, with the counts and last posts already read.
fn board_items(
    frame: &super::frame::SiteFrame,
    filters: &Filters,
    parent: Option<&ForumRecord>,
    statistics: Option<&content::ForumStatistics>,
    last_posts: &[(i64, Thread)],
) -> Vec<records::BoardRecord> {
    let shown = |forum: &&ForumRecord| !frame.forum_filters.is_blocked(forum.fid);
    listed(frame, parent)
        .map(|forum| {
            let members_only = forum.visibility == "members"
                || parent.is_some_and(|parent| parent.visibility == "members");
            let children: Vec<&ForumRecord> = frame
                .forums
                .iter()
                .filter(|child| forum.parent_fid.is_none() && child.parent_fid == Some(forum.fid))
                .filter(shown)
                .collect();
            records::BoardRecord {
                fid: forum.fid,
                slug: forum.slug.clone(),
                name: forum.name.clone(),
                href: crate::site::href(format!("/{}", forum.slug)),
                description: forum.description.clone(),
                parent_fid: forum.parent_fid,
                icon: forum.icon_aid.map(|aid| records::Image {
                    src: crate::site::href(format!("/forum-icon/{}/{aid}", forum.fid)),
                }),
                color: Some(forum.color.clone()).filter(|color| !color.is_empty()),
                glyph: Some(forum.glyph.clone()).filter(|glyph| !glyph.is_empty()),
                access: if members_only { "members" } else { "public" },
                // Board statistics count public content only, so boards anonymous visitors cannot read have none.
                counts: statistics
                    .filter(|_| frame.anonymous_readable(forum))
                    .map(|statistics| {
                        let count = statistics.total(family_slugs(frame, forum));
                        records::BoardCounts {
                            threads: count.threads,
                            today: count.today,
                        }
                    }),
                last_post: last_posts
                    .iter()
                    .find(|(fid, _)| *fid == forum.fid)
                    .map(|(_, thread)| thread)
                    // The latest thread's last poster (or author) is someone the visitor blocked: only the counts.
                    .filter(|thread| {
                        let poster = if thread.last_reply_pid.is_some() {
                            thread.last_reply_uid
                        } else {
                            thread.authorid
                        };
                        !filters.blocks(poster)
                    })
                    .map(|thread| {
                        let tid = i64::try_from(thread.id).unwrap_or(0);
                        let (member, summary) = match thread.last_reply_pid {
                            Some(_) => (
                                MemberRef::of(thread.last_reply_uid, &thread.last_reply_author),
                                Some(crate::markdown::card_excerpt(
                                    &thread.last_reply_summary,
                                    SUMMARY_CHARS,
                                )),
                            ),
                            None => (
                                MemberRef::of(
                                    thread.authorid.filter(|_| !thread.author_gone),
                                    &thread.author,
                                ),
                                None,
                            ),
                        };
                        records::LastPost {
                            tid,
                            title: thread.title.clone(),
                            href: crate::site::href(&match thread.last_reply_pid {
                                Some(pid) => format!("/posts/{pid}"),
                                None => format!("/thread-{tid}"),
                            }),
                            at: Time::of(i64::try_from(thread.updated).unwrap_or(0)),
                            member,
                            summary,
                        }
                    }),
                children: children.into_iter().map(BoardRef::of).collect(),
            }
        })
        .collect()
}

/// At most this many page and date marks together.
const OUTLINE_MARKS: usize = 200;

async fn outline(state: &AppState, page: PageContext<'_>, tid: i64) -> Result<Output, FetchError> {
    let (max_position, revision) = match page
        .thread
        .filter(|thread| i64::try_from(thread.id) == Ok(tid))
    {
        Some(thread) => (thread.max_position, thread.cache_revision),
        None => content::sources::outline_head(&state.db, tid, state.readable())
            .await?
            .ok_or(FetchError::NotFound)?,
    };
    let per_page = i64::try_from(crate::urls::Paging::from_settings(&state.frame.settings).replies)
        .unwrap_or(1)
        .max(1);
    let max_position = max_position.max(1);
    // Floor 1 is the opening post on every page; replies 2 to N fill the pages in floor order.
    let page_of = |position: i64| ((position - 2).max(0) / per_page) + 1;
    let pages = page_of(max_position);
    let samples = outline_samples(max_position, pages, per_page);
    let floors = {
        let cache = state.timed_cache();
        let db = state.db.clone();
        let samples = samples.clone();
        let fill = || async move {
            content::sources::outline_floors(&db, tid, max_position, &samples).await
        };
        if cache.cache().enabled() {
            let key = crate::cache::CacheKey::new(
                &crate::cache::registry::THREAD_OUTLINE,
                revision,
                &(tid, max_position, per_page),
            )?;
            cache.get_or_fill_json(&key, fill).await?
        } else {
            fill().await?
        }
    };
    let Some(&(first_position, first_at)) = floors.first() else {
        return Err(FetchError::NotFound);
    };
    let &(last_position, last_at) = floors.last().unwrap_or(&(first_position, first_at));
    // Page marks: each page's floors and the time of its first floor (the opening post for page 1).
    let mut page_marks = Vec::new();
    for page in 1..=pages {
        let from = if page == 1 {
            1
        } else {
            (page - 1) * per_page + 2
        };
        let to = (page * per_page + 1).min(max_position);
        if let Some(&(_, at)) = floors
            .iter()
            .find(|(position, _)| *position >= from && *position <= to)
        {
            page_marks.push(records::PageMark {
                page,
                from,
                to,
                first_at: Time::of(at),
            });
        }
    }
    // Date marks: the first floor of each day, or of each month once the thread spans more than 60 days.
    let monthly = last_at - first_at > 60 * 86_400;
    let offset = i64::from(crate::clock::current().offset_minutes) * 60;
    let period = |at: i64| {
        let day = (at + offset).div_euclid(86_400);
        if monthly {
            let date = chrono::DateTime::from_timestamp(day * 86_400, 0).unwrap_or_default();
            i64::from(chrono::Datelike::year(&date)) * 12
                + i64::from(chrono::Datelike::month(&date))
        } else {
            day
        }
    };
    let mut date_marks = Vec::new();
    let mut seen = None;
    for &(position, at) in &floors {
        let current = period(at);
        if seen != Some(current) {
            seen = Some(current);
            date_marks.push(records::DateMark {
                position,
                page: page_of(position),
                at: Time::of(at),
            });
        }
    }
    thin(&mut page_marks, &mut date_marks, OUTLINE_MARKS);
    Ok(Output::new(Records::Outline(vec![records::Outline {
        tid,
        pages,
        per_page,
        last_position: max_position,
        first: records::Point {
            position: first_position,
            at: Time::of(first_at),
        },
        last: records::Point {
            position: last_position,
            at: Time::of(last_at),
        },
        page_marks,
        date_marks,
    }])))
}

/// The floors a long thread's outline reads: the first floor of every page and evenly spaced floors, together at
/// most [`OUTLINE_MARKS`], always with the first and last.
fn outline_samples(max_position: i64, pages: i64, per_page: i64) -> Vec<i64> {
    let half = i64::try_from(OUTLINE_MARKS / 2).unwrap_or(100);
    let mut positions: Vec<i64> = (0..half)
        .map(|index| 1 + index * (pages.max(1) - 1) / (half - 1).max(1))
        .map(|page| {
            if page <= 1 {
                1
            } else {
                (page - 1) * per_page + 2
            }
        })
        .chain((0..half).map(|index| 1 + index * (max_position - 1) / (half - 1).max(1)))
        .chain([max_position])
        .filter(|position| (1..=max_position).contains(position))
        .collect();
    positions.sort_unstable();
    positions.dedup();
    positions
}

/// Keeps at most `limit` marks in all, evenly spaced within each list, the first and last of each kept.
fn thin(pages: &mut Vec<records::PageMark>, dates: &mut Vec<records::DateMark>, limit: usize) {
    fn sample<T: Clone>(items: &mut Vec<T>, keep: usize) {
        if items.len() <= keep || keep == 0 {
            if keep == 0 {
                items.clear();
            }
            return;
        }
        let last = items.len() - 1;
        let kept: Vec<T> = (0..keep)
            .map(|index| {
                items[if keep == 1 {
                    0
                } else {
                    index * last / (keep - 1)
                }]
                .clone()
            })
            .collect();
        *items = kept;
    }
    if pages.len() + dates.len() <= limit {
        return;
    }
    let page_share = (limit / 2).max(limit.saturating_sub(dates.len()));
    let page_keep = pages.len().min(page_share);
    sample(pages, page_keep);
    sample(dates, limit - page_keep);
}

/// The personal sources, for a signed-in member only.
async fn personal(state: &AppState, source: Source) -> Result<Output, FetchError> {
    let session = state
        .viewer
        .session
        .as_ref()
        .ok_or(FetchError::Unauthorized)?;
    let member = &session.member;
    let actor = crate::identity::Actor {
        uid: member.uid,
        credentials_version: member.credentials_version,
        api_key: None,
    };
    match source {
        Source::MeCard => {
            let summary = state.member_summary(actor).await?;
            let growth: i64 = crate::db::query_scalar(
                "SELECT COALESCE(MAX(growth),0) FROM gonof_common_growth_account WHERE uid=?",
            )
            .bind(member.uid)
            .fetch_one(state.db.executor())
            .await
            .map_err(anyhow::Error::from)?;
            let level = state.frame.settings.growth.level(growth);
            Ok(Output::new(Records::Card(vec![records::Card {
                member: MemberRef::of(Some(member.uid), &member.username),
                level: records::Level { name: level.name },
                role: match member.adminid {
                    crate::identity::ROLE_ADMIN => "admin",
                    crate::identity::ROLE_SUPER_MODERATOR => "global_moderator",
                    crate::identity::ROLE_MODERATOR => "moderator",
                    crate::identity::ROLE_INTERN => "trainee",
                    crate::identity::ROLE_REVIEWER => "reviewer",
                    _ => "member",
                },
                credits: records::Credits {
                    word: state
                        .frame
                        .settings
                        .site
                        .credit_word(crate::i18n::current()),
                    amount: summary.available,
                },
                verification: records::Verification {
                    email: member.email.is_some() && member.email_verified,
                    mobile: member.mobile.is_some(),
                },
            }])))
        }
        Source::MeCounts => {
            let summary = state.member_summary(actor).await?;
            Ok(Output::new(Records::Counts(vec![records::Counts {
                notifications_unread: state.viewer.unread_notifications,
                messages_unread: summary.unread_messages,
            }])))
        }
        _ => {
            let summary = state.member_summary(actor).await?;
            let stats = crate::activities::signin_stats(
                &state.db,
                member.uid,
                crate::identity::now(),
                state.frame.settings.time.offset_minutes,
            )
            .await?;
            Ok(Output::new(Records::Checkin(vec![records::Checkin {
                today: summary.checked_in,
                streak: i64::try_from(stats.current_streak).unwrap_or(0),
            }])))
        }
    }
}

/// `{"code","error"}` with its status (docs/api.md, "Responses").
fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    json_response(status, json!({"code": code, "error": message.into()}))
}

/// JSON that nothing may store, sniff or share across sites: no CORS headers are ever written.
fn json_response(status: StatusCode, body: Value) -> Response {
    let mut response = (status, body.to_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `GET /data/v1/<source>?…` (docs/data.md, "/data/v1"): read only, same-site only, the parameters of the source as
/// a query string, the records in the envelope.
pub(super) async fn endpoint(
    State(state): State<std::sync::Arc<AppState>>,
    axum::Extension(viewer): axum::Extension<auth::Viewer>,
    method: Method,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let t = crate::i18n::current();
    if method != Method::GET && method != Method::HEAD {
        let mut response = error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            t.data_error_method(),
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET"));
        return response;
    }
    // Pages of other sites cannot read the answers (no CORS), and should not even make the request count.
    if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes() == b"cross-site")
    {
        return error(
            StatusCode::FORBIDDEN,
            "cross_site",
            t.data_error_cross_site(),
        );
    }
    let Some(source) = Source::parse(&name) else {
        return error(StatusCode::NOT_FOUND, "not_found", t.data_error_not_found());
    };
    let mut map = Map::new();
    for (key, value) in url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
        if map
            .insert(key.to_string(), Value::String(value.into_owned()))
            .is_some()
        {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_param",
                t.data_error_param(key.to_string()),
            );
        }
    }
    let params = match Params::parse(source, &map, params::Origin::Api) {
        Ok(params) => params,
        Err(problem) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_param",
                t.data_error_param(problem.name),
            );
        }
    };
    if source.personal() && viewer.session.is_none() {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            t.data_error_unauthorized(),
        );
    }
    let state = match auth::page_state(&state, viewer).await {
        Ok(state) => state,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                t.data_error_internal(),
            );
        }
    };
    let result = if source.personal() {
        personal(&state, source).await
    } else {
        match Filters::load(&state).await {
            Ok(filters) => fetch(&state, &filters, PageContext::default(), &params).await,
            Err(error) => Err(FetchError::Internal(error)),
        }
    };
    match result {
        Ok(output) => json_response(
            StatusCode::OK,
            records::envelope(source, params.json(), &output, crate::identity::now()),
        ),
        Err(FetchError::NotFound) => {
            error(StatusCode::NOT_FOUND, "not_found", t.data_error_not_found())
        }
        Err(FetchError::Unauthorized) => error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            t.data_error_unauthorized(),
        ),
        Err(FetchError::Internal(failure)) => {
            if failure
                .downcast_ref::<crate::cache::Unavailable>()
                .is_some()
            {
                let mut response = error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    t.data_error_unavailable(),
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("3"));
                return response;
            }
            tracing::error!(error = %format!("{failure:#}"), source = source.name(), "data source failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                t.data_error_internal(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_outlines_sample_at_most_the_mark_budget() {
        let samples = outline_samples(100_000, 5_000, 20);
        assert!(samples.len() <= OUTLINE_MARKS + 1, "{}", samples.len());
        assert_eq!(samples.first(), Some(&1));
        assert_eq!(samples.last(), Some(&100_000));
        assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
        let time = |at| Time::of(at);
        let mut pages: Vec<records::PageMark> = (1..=500)
            .map(|page| records::PageMark {
                page,
                from: page,
                to: page,
                first_at: time(page),
            })
            .collect();
        let mut dates: Vec<records::DateMark> = (1..=300)
            .map(|position| records::DateMark {
                position,
                page: position,
                at: time(position),
            })
            .collect();
        thin(&mut pages, &mut dates, OUTLINE_MARKS);
        assert_eq!(pages.len() + dates.len(), OUTLINE_MARKS);
        assert_eq!(pages.first().map(|mark| mark.page), Some(1));
        assert_eq!(pages.last().map(|mark| mark.page), Some(500));
        assert_eq!(dates.last().map(|mark| mark.position), Some(300));
    }
}
