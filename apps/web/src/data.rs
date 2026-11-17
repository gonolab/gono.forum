//! Data sources (docs/data.md): named, read-only views of forum data that three outputs share as one record —
//! server-rendered blocks in the places of a layout ([`crate::layout`]), the `blocks` of the page data block, and
//! `GET /data/v1/<source>`.
//!
//! This module holds what needs no request: the sources and what each accepts ([`Source`], [`params`]) and the
//! records they return ([`records`]). Fetching runs in `router/data_sources.rs`, with the visitor's readable boards
//! and blocks (`content::ListViewer`), so a source never shows a visitor more than the pages do.
//!
//! The rules (docs/data.md, "Rules"): parameters are closed enumerations, each combination served by an index path
//! listed in docs/queries.md and capped; every source is registered in `cache/registry.rs`; records are the view
//! types of [`records`], never internal structures; fields only grow, and a removed field means a new [`VERSION`].
pub mod params;
pub mod records;

/// The version of the record format, `"v"` of every envelope.
pub const VERSION: u32 = 1;

/// Most data-source blocks one page renders (docs/data.md, "Rules"); a layout with more is refused on save, and
/// blocks past it are skipped when a page is read.
pub const MAX_BLOCKS_PER_PAGE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Threads,
    Boards,
    MembersRecent,
    MembersRank,
    MembersHonors,
    MembersVerified,
    Moderators,
    SiteStats,
    SiteLinks,
    ThreadOutline,
    MeCard,
    MeCounts,
    MeCheckin,
}

impl Source {
    pub const ALL: [Self; 13] = [
        Self::Threads,
        Self::Boards,
        Self::MembersRecent,
        Self::MembersRank,
        Self::MembersHonors,
        Self::MembersVerified,
        Self::Moderators,
        Self::SiteStats,
        Self::SiteLinks,
        Self::ThreadOutline,
        Self::MeCard,
        Self::MeCounts,
        Self::MeCheckin,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::Boards => "boards",
            Self::MembersRecent => "members.recent",
            Self::MembersRank => "members.rank",
            Self::MembersHonors => "members.honors",
            Self::MembersVerified => "members.verified",
            Self::Moderators => "moderators",
            Self::SiteStats => "site.stats",
            Self::SiteLinks => "site.links",
            Self::ThreadOutline => "thread.outline",
            Self::MeCard => "me.card",
            Self::MeCounts => "me.counts",
            Self::MeCheckin => "me.checkin",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.name() == name)
    }

    /// The signed-in member's own data: only for a session, never cached for others, never in a page's data block
    /// or a block (docs/data.md, "me.*").
    pub fn personal(self) -> bool {
        matches!(self, Self::MeCard | Self::MeCounts | Self::MeCheckin)
    }

    /// The views a block of this source can use (`templates/blocks/<source>/<view>.html`); the personal sources
    /// have none.
    pub fn views(self) -> &'static [&'static str] {
        match self {
            Self::Threads => &["mini-list", "list", "cards"],
            Self::Boards => &["tree", "grid", "directory"],
            Self::MembersRecent
            | Self::MembersHonors
            | Self::MembersVerified
            | Self::Moderators => &["avatars", "list"],
            Self::MembersRank => &["avatars", "list", "table"],
            Self::SiteStats => &["figures", "compact"],
            Self::SiteLinks => &["links"],
            Self::ThreadOutline => &["timeline", "progress"],
            Self::MeCard | Self::MeCounts | Self::MeCheckin => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_round_trip_and_personal_sources_have_no_views() {
        for source in Source::ALL {
            assert_eq!(Source::parse(source.name()), Some(source));
            assert_eq!(source.personal(), source.name().starts_with("me."));
            assert_eq!(source.views().is_empty(), source.personal(), "{source:?}");
        }
        assert_eq!(Source::parse("Threads"), None);
        assert_eq!(Source::parse("me"), None);
    }
}
