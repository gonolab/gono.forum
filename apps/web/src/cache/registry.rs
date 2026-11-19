//! Cache registry: one entry per cache, stating its name, layer, invalidation, source
//! of lifetime and whether it is per member.
//!
//! - Redis cache keys can only be built from entries here ([`super::CacheKey::new`]). Tags carry a structure version
//!   number, incremented whenever the cache's structure or meaning changes, so old keys simply expire.
//! - When the process cache reads a block it looks up the slot name in this table, and debug builds panic on an
//!   unregistered slot name; every block ([`super::Block`]) has an entry.
//! - The debug panel takes the display name from here by slot name ([`Entry::label`], translated).
//!
//! Redis's other uses are not caches and are not here: rate limiting ([`crate::rate_limit`]), the invalidation
//! broadcast of the process cache, and the deduplication keys and daily key of thread views
//! ([`crate::content::views`]).

/// Which layer it lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Only in process; present both with Redis disabled and enabled.
    Process,
    /// In process first; with Redis enabled, a process miss reads Redis, and only a second miss computes it.
    ProcessThenRedis,
    /// Only present when Redis is enabled; with Redis disabled the database is read directly.
    Redis,
}

/// How it is invalidated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalidation {
    /// Cleared by the save function after the write commits: the process cache clears this process first, then
    /// broadcasts when Redis is enabled; Redis keys are deleted directly.
    OnWrite,
    /// The key carries the version this request read from the database, so a new version means a new key; the
    /// lifetime only handles reclaiming.
    Versioned,
    /// The key carries a time segment; a new segment is recomputed, and writes do not invalidate it.
    Segmented,
    /// Stored by binding (slot, attachment id, size); the current binding is checked in the database before use;
    /// old entries whose binding changed are no longer hit and are pushed out by capacity.
    Binding,
}

/// Where the lifetime comes from ([`super::Lifetimes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    /// `settings.cache.ttl_seconds`, 0 means no caching. Only for process cache blocks, never in Redis.
    Process,
    /// Member page frame: `settings.cache.ttl_seconds`, capped at [`super::redis::MEMBER_MAX_SECONDS`].
    Member,
    /// `settings.cache.page_ttl_seconds`.
    Page,
    /// `settings.cache.list_ttl_seconds`.
    List,
    /// `settings.cache.statistics_seconds`, which is also the segment length.
    Statistics,
    /// `settings.cache.forum_statistics_seconds`, which is also the segment length.
    ForumStatistics,
    /// No lifetime; pushed out by entry count and byte limits.
    Capacity,
}

/// One cache.
#[derive(Debug)]
pub struct Entry {
    /// Registry name. For a block it equals its [`super::Block::name`].
    pub name: &'static str,
    pub layer: Layer,
    /// Invalidation methods. Most have one; those with two have both a version and a time segment in the key.
    pub invalidation: &'static [Invalidation],
    pub lifetime: Lifetime,
    /// Keyed per member (uid in the key). Per-member caches only go to Redis.
    pub per_member: bool,
    /// In-process slot names: these are what the debug panel records. Empty for Redis-only entries.
    pub slots: &'static [&'static str],
    /// Tag of the Redis key, with a structure version number; `None` for entries not in Redis.
    pub redis_tag: Option<&'static str>,
}

impl Entry {
    /// The display name in the debug panel, in `t`; the credit-rules cache is named after the site's points.
    pub fn label(&self, t: crate::i18n::Lang, credit: &str) -> String {
        match self.name {
            "settings" => t.debug_cache_settings(),
            "policy" => t.debug_cache_policy(),
            "branding" => t.debug_cache_branding(),
            "credit-rules" => return t.debug_cache_credit_rules(credit),
            "links" => t.debug_cache_links(),
            "pins" => t.debug_cache_pins(),
            "moderators" => t.debug_cache_moderators(),
            "honors" => t.debug_cache_honors(),
            "forums" => t.debug_cache_forums(),
            "domains" => t.debug_cache_domains(),
            "skins" => t.debug_cache_skins(),
            "statistics" => t.debug_cache_statistics(),
            "scoped-statistics" => t.debug_cache_scoped_statistics(),
            "recent-members" => t.debug_cache_recent_members(),
            "forum-statistics" => t.debug_cache_forum_statistics(),
            "thread-list" => t.debug_cache_thread_list(),
            "sidebar-topics" => t.debug_cache_sidebar_topics(),
            "forum-pins" => t.debug_cache_forum_pins(),
            "feed-items" => t.debug_cache_feed_items(),
            "floors" => t.debug_cache_floors(),
            "rendered-bodies" => t.debug_cache_rendered_bodies(),
            "member-frame" => t.debug_cache_member_frame(),
            "data-threads" => t.debug_cache_data_threads(),
            "data-members" => t.debug_cache_data_members(),
            "thread-outline" => t.debug_cache_thread_outline(),
            "board-last-posts" => t.debug_cache_board_last_posts(),
            "brand-images" => t.debug_cache_brand_images(),
            "display-images" => t.debug_cache_display_images(),
            "thumbnails" => t.debug_cache_thumbnails(),
            "storage-clients" => t.debug_cache_storage_clients(),
            other => other,
        }
        .to_owned()
    }

    const fn block(name: &'static str, slots: &'static [&'static str]) -> Self {
        Self {
            name,
            layer: Layer::Process,
            invalidation: &[Invalidation::OnWrite],
            lifetime: Lifetime::Process,
            per_member: false,
            slots,
            redis_tag: None,
        }
    }
}

// ---- Process cache: blocks (cleared on write, sharing settings.cache.ttl_seconds) ----

pub const SETTINGS: Entry = Entry::block("settings", &["settings"]);
/// One slot per kind of policy document; the slot name is the kind ([`super::POLICY_KINDS`]).
pub const POLICY: Entry = Entry::block("policy", &super::POLICY_KINDS);
pub const BRANDING: Entry = Entry::block("branding", &["branding"]);
pub const CREDIT_RULES: Entry = Entry::block("credit-rules", &["credit-rules"]);
pub const LINKS: Entry = Entry::block("links", &["links.home", "links.directory"]);
pub const PINS: Entry = Entry::block("pins", &["pins"]);
pub const MODERATORS: Entry = Entry::block("moderators", &["moderators"]);
pub const HONORS: Entry = Entry::block("honors", &["honors"]);
/// The board stylesheets (`board-style`, one per skin pages use) are derived from the member copy and cleared with the
/// block.
pub const FORUMS: Entry = Entry::block(
    "forums",
    &["forums-public", "forums-members", "board-style"],
);
pub const DOMAINS: Entry = Entry::block("domains", &["domains"]);
pub const SKINS: Entry = Entry::block("skins", &["skins"]);

// ---- Process cache + Redis: statistics (key changes by time segment) ----

pub const STATISTICS: Entry = Entry {
    name: "statistics",
    layer: Layer::ProcessThenRedis,
    invalidation: &[Invalidation::Segmented],
    lifetime: Lifetime::Statistics,
    per_member: false,
    slots: &["statistics"],
    redis_tag: Some("sidebar-statistics-v3"),
};
pub const SCOPED_STATISTICS: Entry = Entry {
    name: "scoped-statistics",
    layer: Layer::ProcessThenRedis,
    invalidation: &[Invalidation::Segmented],
    lifetime: Lifetime::Statistics,
    per_member: false,
    slots: &["scoped-statistics"],
    redis_tag: Some("scoped-statistics-v1"),
};
pub const RECENT_MEMBERS: Entry = Entry {
    name: "recent-members",
    layer: Layer::ProcessThenRedis,
    invalidation: &[Invalidation::Versioned, Invalidation::Segmented],
    lifetime: Lifetime::Statistics,
    per_member: false,
    slots: &["recent-members"],
    redis_tag: Some("recent-members-v4"),
};
pub const FORUM_STATISTICS: Entry = Entry {
    name: "forum-statistics",
    layer: Layer::ProcessThenRedis,
    invalidation: &[Invalidation::Segmented],
    lifetime: Lifetime::ForumStatistics,
    per_member: false,
    slots: &["forum-statistics"],
    redis_tag: Some("forum-statistics-v1"),
};

// ---- Redis only: anonymous content (key changes by version) ----

const fn anonymous(
    name: &'static str,
    invalidation: &'static [Invalidation],
    lifetime: Lifetime,
    tag: &'static str,
) -> Entry {
    Entry {
        name,
        layer: Layer::Redis,
        invalidation,
        lifetime,
        per_member: false,
        slots: &[],
        redis_tag: Some(tag),
    }
}

/// v10: rows carry the last replier's uid and excerpt, the pin level, the highest floor, favorites and tips.
pub const THREAD_LIST: Entry = anonymous(
    "thread-list",
    &[Invalidation::Versioned],
    Lifetime::List,
    "home-v10",
);
pub const SIDEBAR_TOPICS: Entry = anonymous(
    "sidebar-topics",
    &[Invalidation::Versioned, Invalidation::Segmented],
    Lifetime::List,
    "sidebar-topics-v9",
);
pub const FORUM_PINS: Entry = anonymous(
    "forum-pins",
    &[Invalidation::Versioned],
    Lifetime::List,
    "forum-pins-v4",
);
/// v8: the key carries the interface language (docs/architecture.md, "Languages").
pub const FEED_ITEMS: Entry = anonymous(
    "feed-items",
    &[Invalidation::Versioned],
    Lifetime::List,
    "feed-items-v8",
);
pub const FLOORS: Entry = anonymous(
    "floors",
    &[Invalidation::Versioned],
    Lifetime::Page,
    "floors-v1",
);
/// The value is HTML rendered by markdown.rs; bump the tag version whenever the rendered output (tags, attributes,
/// class names) changes:
///
/// - v1: attachment links carry hx-boost="false", highlight class names get the hl- prefix, class keeps only the
///   allowlist.
/// - v2: task lists are no longer parsed; `- [ ] todo` shows the brackets as written.
/// - v3: a YouTube link standing alone in a paragraph becomes a placeholder, and on-site thread links are filled into
///   cards from the anonymous point of view. Any change to a card's title, visibility or first post excerpt bumps
///   the site-wide content version (`content::copies::bump_site`), which the key already carries.
/// - v4: on-site URLs carry this request's path prefix; the prefix is part of the key, so a
///   changed prefix or several sites each store their own copy.
/// - v5: with multi-domain sites on, on-site thread links written as absolute URLs of a registered domain are also
///   filled into cards; which URLs are recognized depends on the domain table, so
///   the enabled registered URLs are part of the key; empty when off.
/// - v6: when the forum URL is empty or the Host is not registered, recognizing on-site links also depends on this
///   request's site URL; dates on cards follow the site time settings.
/// - v7: body images carry `loading="lazy"`.
/// - v8: the spoiler summary and the YouTube placeholder are written in the request's language, which the key
///   carries through the time format (docs/architecture.md, "Languages").
/// - v9: the current class names (docs/styles.md, "Class names"): `details.reveal`, `pre.code-block`, `syn-`
///   highlighting spans, `video-embed`, `topic-embed`.
/// - v10: Markdown nesting past `markdown::MAX_NESTING` levels is flattened, and raw HTML after the first
///   `markdown::MAX_HTML_TAGS` start tags is shown as text.
/// - v11: code blocks are plain `pre > code`, without syntax highlighting: no `pre.code-block` or `syn-` spans.
/// - v12: the value keeps the bodies with their card slots (`markdown::Rendered`), and the cards are filled for each
///   request, so they show the linked threads as they are now. Only a link to a whole thread becomes a card (no
///   page number, query string or anchor), and the longest matching site address recognizes it. Raw HTML keeps its
///   text when its image tags are cut out.
pub const RENDERED_BODIES: Entry = anonymous(
    "rendered-bodies",
    &[Invalidation::Versioned],
    Lifetime::Page,
    "rendered-v12",
);

// ---- Data sources (docs/data.md) ----

/// Anonymous `threads` lists of the data sources, keyed like thread lists by the list version (hot threads also by a
/// list-lifetime segment) and the request's time format.
pub const DATA_THREADS: Entry = anonymous(
    "data-threads",
    &[Invalidation::Versioned, Invalidation::Segmented],
    Lifetime::List,
    "data-threads-v1",
);
/// Member lists of the data sources (`members.rank`, `members.honors`, `members.verified`), one copy per visitor
/// class (anonymous, signed in) without anyone's blocks, keyed by the site-wide content version (a privacy change
/// raises it) and segmented by the list lifetime; each viewer's blocks are removed in code.
pub const DATA_MEMBERS: Entry = anonymous(
    "data-members",
    &[Invalidation::Versioned, Invalidation::Segmented],
    Lifetime::List,
    "data-members-v1",
);
/// The floors behind `thread.outline`, by thread and its `cache_revision`.
pub const THREAD_OUTLINE: Entry = anonymous(
    "thread-outline",
    &[Invalidation::Versioned],
    Lifetime::Page,
    "thread-outline-v1",
);
/// The latest thread of each board for `boards` with `last_post`: one slot for the anonymous view and one for
/// members, each keyed by the list version and the boards it covers, and read again after one list lifetime.
pub const BOARD_LAST_POSTS: Entry = Entry {
    name: "board-last-posts",
    layer: Layer::Process,
    invalidation: &[Invalidation::Versioned],
    lifetime: Lifetime::List,
    per_member: false,
    slots: &["board-last-posts-public", "board-last-posts-members"],
    redis_tag: None,
};

/// How each data source (docs/data.md) is read: the caches above or the process blocks it draws on, or none.
pub struct SourceCache {
    pub source: &'static str,
    pub entries: &'static [&'static Entry],
}

/// Every data source; a unit test keeps it equal to `data::Source::ALL`.
pub const SOURCES: &[SourceCache] = &[
    SourceCache {
        source: "threads",
        entries: &[&DATA_THREADS],
    },
    SourceCache {
        source: "boards",
        entries: &[&FORUMS, &FORUM_STATISTICS, &BOARD_LAST_POSTS],
    },
    SourceCache {
        source: "members.recent",
        entries: &[&RECENT_MEMBERS],
    },
    SourceCache {
        source: "members.rank",
        entries: &[&DATA_MEMBERS],
    },
    SourceCache {
        source: "members.honors",
        entries: &[&HONORS, &DATA_MEMBERS],
    },
    SourceCache {
        source: "members.verified",
        entries: &[&DATA_MEMBERS],
    },
    SourceCache {
        source: "moderators",
        entries: &[&MODERATORS],
    },
    SourceCache {
        source: "site.stats",
        entries: &[&STATISTICS, &SCOPED_STATISTICS, &FORUM_STATISTICS],
    },
    SourceCache {
        source: "site.links",
        entries: &[&LINKS],
    },
    SourceCache {
        source: "thread.outline",
        entries: &[&THREAD_OUTLINE],
    },
    // The member's own data is never cached for others: the session, the member frame and live reads.
    SourceCache {
        source: "me.card",
        entries: &[&MEMBER_FRAME],
    },
    SourceCache {
        source: "me.counts",
        entries: &[],
    },
    SourceCache {
        source: "me.checkin",
        entries: &[],
    },
];

// ---- Redis only: per member (key deleted on write) ----

pub const MEMBER_FRAME: Entry = Entry {
    name: "member-frame",
    layer: Layer::Redis,
    invalidation: &[Invalidation::OnWrite],
    lifetime: Lifetime::Member,
    per_member: true,
    slots: &[],
    redis_tag: Some("frame-member-v2"),
};

// ---- In-process image bytes (checked against the binding) ----

pub const BRAND_IMAGES: Entry = Entry {
    name: "brand-images",
    layer: Layer::Process,
    invalidation: &[Invalidation::Binding],
    lifetime: Lifetime::Capacity,
    per_member: false,
    slots: &[],
    redis_tag: None,
};
pub const DISPLAY_IMAGES: Entry = Entry {
    name: "display-images",
    layer: Layer::Process,
    invalidation: &[Invalidation::Binding],
    lifetime: Lifetime::Capacity,
    per_member: false,
    slots: &[],
    redis_tag: None,
};
/// Thread list thumbnails (`media/thumbnail.rs`): stored by aid; before use, permission is decided by the
/// current state of the thread and first post.
pub const THUMBNAILS: Entry = Entry {
    name: "thumbnails",
    layer: Layer::Process,
    invalidation: &[Invalidation::Binding],
    lifetime: Lifetime::Capacity,
    per_member: false,
    slots: &[],
    redis_tag: None,
};
/// Object storage clients, stored by storage configuration id (`ClientCache` in `media/storage.rs`). Every save gets
/// a new configuration id; new attachments after a configuration change use the new id, and clients for old ids stay
/// for old attachments; the entry count is the number of historical versions of the storage configuration. The only
/// change to an existing id is new keys for its bucket, which remove its old credentials file: a client whose file is
/// gone is built again.
pub const STORAGE_CLIENTS: Entry = Entry {
    name: "storage-clients",
    layer: Layer::Process,
    invalidation: &[Invalidation::Binding],
    lifetime: Lifetime::Capacity,
    per_member: false,
    slots: &[],
    redis_tag: None,
};

/// All entries.
pub const ENTRIES: &[&Entry] = &[
    &SETTINGS,
    &POLICY,
    &BRANDING,
    &CREDIT_RULES,
    &LINKS,
    &PINS,
    &MODERATORS,
    &HONORS,
    &FORUMS,
    &DOMAINS,
    &SKINS,
    &STATISTICS,
    &SCOPED_STATISTICS,
    &RECENT_MEMBERS,
    &FORUM_STATISTICS,
    &THREAD_LIST,
    &SIDEBAR_TOPICS,
    &FORUM_PINS,
    &FEED_ITEMS,
    &FLOORS,
    &RENDERED_BODIES,
    &MEMBER_FRAME,
    &DATA_THREADS,
    &DATA_MEMBERS,
    &THREAD_OUTLINE,
    &BOARD_LAST_POSTS,
    &BRAND_IMAGES,
    &DISPLAY_IMAGES,
    &THUMBNAILS,
    &STORAGE_CLIENTS,
];

/// The entry for an in-process slot name; `None` if not registered.
pub fn slot(name: &str) -> Option<&'static Entry> {
    ENTRIES
        .iter()
        .copied()
        .find(|entry| entry.slots.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Block, CacheKey, POLICY_KINDS};
    use std::collections::BTreeSet;

    #[test]
    fn names_slots_and_tags_are_unique() {
        let mut names = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut tags = BTreeSet::new();
        for entry in ENTRIES {
            assert!(
                names.insert(entry.name),
                "repeated entry name: {}",
                entry.name
            );
            for slot in entry.slots {
                assert!(slots.insert(*slot), "repeated slot name: {slot}");
            }
            if let Some(tag) = entry.redis_tag {
                assert!(tags.insert(tag), "repeated Redis tag: {tag}");
            }
            assert!(
                !entry.invalidation.is_empty(),
                "{} names no invalidation",
                entry.name
            );
        }
    }

    #[test]
    fn layers_follow_the_rules() {
        for entry in ENTRIES {
            let uses_redis = entry.redis_tag.is_some();
            match entry.layer {
                Layer::Process => assert!(
                    !uses_redis,
                    "{} is process-only but has a Redis tag",
                    entry.name
                ),
                Layer::ProcessThenRedis => assert!(
                    uses_redis && !entry.slots.is_empty(),
                    "{} needs both layers",
                    entry.name
                ),
                Layer::Redis => assert!(
                    uses_redis && entry.slots.is_empty(),
                    "{} is Redis-only and must have no process slots",
                    entry.name
                ),
            }
            // Per-member caches only go to Redis; caches with an unbounded set of entries stay out of the process.
            if entry.per_member {
                assert_eq!(
                    entry.layer,
                    Layer::Redis,
                    "{} is per member but sits in the process cache",
                    entry.name
                );
            }
            assert_eq!(
                entry.per_member,
                entry.lifetime == Lifetime::Member,
                "{}: per-member entries, and only they, use the member lifetime (at most 300 seconds)",
                entry.name
            );
            if uses_redis {
                assert!(
                    !matches!(entry.lifetime, Lifetime::Process | Lifetime::Capacity),
                    "{} is in Redis but uses the process or capacity lifetime",
                    entry.name
                );
            }
            if entry.invalidation.contains(&Invalidation::Segmented) {
                assert!(
                    entry.lifetime != Lifetime::Process && entry.lifetime != Lifetime::Capacity,
                    "{} is segmented but has no segment length",
                    entry.name
                );
            }
            assert_eq!(
                entry.lifetime == Lifetime::Capacity,
                entry.invalidation == [Invalidation::Binding],
                "{}: only entries checked by binding are evicted by capacity",
                entry.name
            );
        }
    }

    #[test]
    fn every_block_has_an_on_write_entry_covering_its_slots() {
        let mut blocks = vec![
            Block::Settings,
            Block::Branding,
            Block::CreditRules,
            Block::Links,
            Block::Pins,
            Block::Moderators,
            Block::Honors,
            Block::Forums,
            Block::Domains,
            Block::Skins,
        ];
        blocks.extend(POLICY_KINDS.iter().map(|kind| Block::Policy(kind)));
        for block in blocks {
            // Adding a new block makes this fail to compile, as a reminder to add it to the list above.
            match block {
                Block::Settings
                | Block::Policy(_)
                | Block::Branding
                | Block::CreditRules
                | Block::Links
                | Block::Pins
                | Block::Moderators
                | Block::Honors
                | Block::Forums
                | Block::Domains
                | Block::Skins => {}
            }
            let entry = block.entry();
            assert_eq!(entry.layer, Layer::Process, "{}", block.name());
            assert_eq!(
                entry.invalidation,
                [Invalidation::OnWrite],
                "{}",
                block.name()
            );
            match block {
                Block::Policy(kind) => assert!(entry.slots.contains(&kind), "{kind}"),
                _ => assert_eq!(entry.name, block.name()),
            }
        }
    }

    #[test]
    fn every_data_source_is_registered_once() {
        let names: Vec<_> = SOURCES.iter().map(|source| source.source).collect();
        let expected: Vec<_> = crate::data::Source::ALL
            .iter()
            .map(|source| source.name())
            .collect();
        assert_eq!(names, expected);
        for source in SOURCES {
            for entry in source.entries {
                assert!(
                    ENTRIES.iter().any(|known| std::ptr::eq(*known, *entry)),
                    "{}: {} is not in ENTRIES",
                    source.source,
                    entry.name
                );
                if source.source.starts_with("me.") {
                    assert!(
                        entry.per_member,
                        "{} is personal and only reads per-member caches",
                        source.source
                    );
                }
            }
        }
    }

    #[test]
    fn only_redis_entries_make_keys() {
        for entry in ENTRIES {
            let key = CacheKey::new(entry, 1, &("fixture", 1));
            assert_eq!(key.is_ok(), entry.redis_tag.is_some(), "{}", entry.name);
        }
        let first = CacheKey::new(&THREAD_LIST, 1, &1).unwrap();
        let other = CacheKey::new(&FEED_ITEMS, 1, &1).unwrap();
        assert_ne!(
            first.redis_key("ns"),
            other.redis_key("ns"),
            "different tags make different keys"
        );
        assert!(first.redis_key("ns").starts_with("ns:cache:v2:home-v10:"));
        assert_eq!(slot("forums-members").map(|e| e.name), Some("forums"));
        assert!(slot("nope").is_none());
    }
}
