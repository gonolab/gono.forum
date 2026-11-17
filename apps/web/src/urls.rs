use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Latest,
    Created,
    Pinned,
    Featured,
}

impl Sort {
    pub fn value(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Created => "created",
            Self::Pinned => "pinned",
            Self::Featured => "featured",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Latest => crate::i18n::current().home_sort_latest(),
            Self::Created => crate::i18n::current().home_sort_created(),
            Self::Pinned => crate::i18n::current().common_pinned(),
            Self::Featured => crate::i18n::current().common_featured(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageRoute {
    Home { page: usize },
    Forum { slug: String, page: usize },
    Thread { id: u64, page: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberView {
    Overview,
    Threads,
    Posts,
}

impl MemberView {
    pub fn value(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Threads => "threads",
            Self::Posts => "posts",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => crate::i18n::current().profile_view_overview(),
            Self::Threads => crate::i18n::current().profile_view_threads(),
            Self::Posts => crate::i18n::current().profile_view_replies(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberRoute {
    pub uid: i64,
    pub view: MemberView,
    pub page: usize,
}

impl MemberRoute {
    pub fn parse(path: &str) -> Option<Self> {
        let rest = path.strip_prefix("/space/")?;
        let (uid, collection) = rest
            .split_once('/')
            .map_or((rest, None), |(uid, collection)| (uid, Some(collection)));
        let uid = positive(uid)?.try_into().ok()?;
        let (view, page) = match collection {
            None => (MemberView::Overview, 1),
            Some(collection) => {
                let (kind, page) = collection.split_once('-').unwrap_or((collection, "1"));
                let view = match kind {
                    "threads" => MemberView::Threads,
                    "posts" => MemberView::Posts,
                    _ => return None,
                };
                (view, positive(page)?.try_into().ok()?)
            }
        };
        Some(Self { uid, view, page })
    }

    pub fn path(&self) -> String {
        let space = space(self.uid);
        match (self.view, self.page) {
            (MemberView::Overview, _) => space,
            (view, 1) => format!("{space}/{}", view.value()),
            (view, page) => format!("{space}/{}-{page}", view.value()),
        }
    }

    pub fn with_page(&self, page: usize) -> Self {
        Self { page, ..*self }
    }
}

/// First path segments under the root that the system occupies. Forum home pages live at `/{slug}`, and static routes
/// match first, so a forum with the same name would be hidden by the system entry; hence this one list is used both for
/// validating forums in the configuration at startup and when forums are created or renamed in the admin area.
///
/// Contains all current top-level routes (checked one by one by a unit test), plus a few old entry names from before
/// the move to `/my`.
/// Landing pages for e-mail links with tokens: the URL itself is the credential, so it stays out of the access log.
pub(crate) fn token_page(path: &str) -> bool {
    matches!(
        path,
        "/verify-email"
            | "/reset-password"
            | "/confirm-email-change"
            | "/appeals/access"
            | "/data/access"
    )
}

/// Sensitive pages: token landing pages, data requests, API tokens and appeals. They show no debug entry, and the debug
/// middleware neither collects data nor injects the panel (SQL bind values may contain token digests or account data).
/// Both places share this one list, so a new sensitive page is added only here.
pub(crate) fn sensitive_page(path: &str) -> bool {
    token_page(path)
        || path == "/data"
        || path.starts_with("/data/")
        || path.starts_with("/my/data")
        || path.starts_with("/my/api")
        || path.starts_with("/my/feed")
        || path.starts_with(PRIVATE_FEED_PREFIX)
        || path == "/appeals"
        || path.starts_with("/appeals/")
}

pub(crate) const RESERVED_SEGMENTS: &[&str] = &[
    "api",
    "appeals",
    "auth",
    "avatar",
    "boards.css",
    "brand",
    "confirm-email-change",
    "contact",
    "credits",
    "data",
    "edit",
    "favorites",
    "feeds",
    "following",
    "forgot-password",
    "forum-icon",
    "forums",
    "healthz",
    "links",
    "login",
    "logout",
    "manage",
    "media",
    "messages",
    "my",
    "new",
    "notifications",
    "posts",
    "privacy",
    "ranks",
    "register",
    "reports",
    "reset-password",
    "rules",
    "search",
    "settings",
    "skin",
    "space",
    "static",
    "terms",
    "threads",
    "thumb",
    "verified",
    "verify-email",
];

/// Prefixes taken by list pagination and thread URLs.
const RESERVED_PREFIXES: &[&str] = &["page-", "thread-"];

/// Whether a forum slug conflicts with a system URL. Format, length and a `-digits` ending are validated separately by
/// each caller.
pub(crate) fn reserved_segment(slug: &str) -> bool {
    RESERVED_SEGMENTS.contains(&slug) || RESERVED_PREFIXES.iter().any(|p| slug.starts_with(p))
}

/// Personal spaces are addressed by uid: renaming does not affect the URL, and usernames need no path encoding.
pub fn space(uid: i64) -> String {
    format!("/space/{uid}")
}

fn positive(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|n| *n > 0)
}

impl PageRoute {
    pub fn parse(segment: &str, slugs: &[&str]) -> Option<Self> {
        if segment.is_empty() {
            return Some(Self::Home { page: 1 });
        }
        if let Some(page) = segment.strip_prefix("page-") {
            return Some(Self::Home {
                page: positive(page)?.try_into().ok()?,
            });
        }
        if let Some(thread) = segment.strip_prefix("thread-") {
            let (id, page) = thread.split_once('-').unwrap_or((thread, "1"));
            return Some(Self::Thread {
                id: positive(id)?,
                page: positive(page)?.try_into().ok()?,
            });
        }
        if slugs.contains(&segment) {
            return Some(Self::Forum {
                slug: segment.into(),
                page: 1,
            });
        }
        let (slug, page) = segment.rsplit_once('-')?;
        slugs.contains(&slug).then_some(Self::Forum {
            slug: slug.into(),
            page: positive(page)?.try_into().ok()?,
        })
    }

    pub fn path(&self) -> String {
        match self {
            Self::Home { page: 1 } => "/".into(),
            Self::Home { page } => format!("/page-{page}"),
            Self::Forum { slug, page: 1 } => format!("/{slug}"),
            Self::Forum { slug, page } => format!("/{slug}-{page}"),
            Self::Thread { id, page: 1 } => format!("/thread-{id}"),
            Self::Thread { id, page } => format!("/thread-{id}-{page}"),
        }
    }
    pub fn with_page(&self, page: usize) -> Self {
        match self {
            Self::Home { .. } => Self::Home { page },
            Self::Forum { slug, .. } => Self::Forum {
                slug: slug.clone(),
                page,
            },
            Self::Thread { id, .. } => Self::Thread { id: *id, page },
        }
    }
    pub fn page(&self) -> usize {
        match self {
            Self::Home { page } | Self::Forum { page, .. } | Self::Thread { page, .. } => *page,
        }
    }
}

pub fn listing(route: &PageRoute, sort: Sort) -> String {
    let path = route.path();
    if sort == Sort::Latest {
        path
    } else {
        format!("{path}?sort={}", sort.value())
    }
}

/// A list link generated on the site for a given visitor: their default sort omits `sort`, others are written out.
///
/// When the default sort is "Latest replies" this is the same as [`listing`]; when it is "Newest threads", the bare URL
/// is latest threads, and switching back to latest reply needs an explicit `?sort=latest`. canonical still uses
/// [`listing`], independent of the visitor.
pub fn listing_for(route: &PageRoute, sort: Sort, default: Sort) -> String {
    let path = route.path();
    if sort == default {
        path
    } else {
        format!("{path}?sort={}", sort.value())
    }
}

pub fn thread(id: u64, page: usize, flat: bool) -> String {
    let path = PageRoute::Thread { id, page }.path();
    if flat {
        format!("{path}?view=flat")
    } else {
        path
    }
}

/// Floor pagination: floors per page and nested replies per page, from the admin settings
/// `settings.display.replies_per_page` (10–100) and `sub_replies_per_page` (5–50). Page numbers, `/thread-N-P`
/// URLs, floor positioning and floor cache keys all derive from it; after a change, old links with page numbers point
/// to the wrong place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Paging {
    pub replies: usize,
    pub sub_replies: usize,
}

impl Paging {
    pub fn from_settings(settings: &crate::runtime::SiteSettings) -> Self {
        let size = |value: i64| usize::try_from(value).unwrap_or(1).max(1);
        Self {
            replies: size(settings.display.replies_per_page),
            sub_replies: size(settings.display.sub_replies_per_page),
        }
    }
}

/// Resolve a permanent floor using current, access-filtered thread data.
/// Tombstones retain pagination positions; inaccessible threads must be rejected by callers.
pub fn post_location(
    thread: &crate::content::Thread,
    pid: i64,
    flat: bool,
    paging: Paging,
) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    if pid == thread.first_pid {
        return Some(format!("{}#floor-1", self::thread(thread.id, 1, flat)));
    }
    let post = thread.posts.iter().find(|post| post.pid as i64 == pid)?;
    Some(position_location(
        thread.id,
        post.position,
        post.root_pid,
        post.root_position,
        post.sub_position,
        flat,
        paging,
    ))
}

pub fn position_location(
    tid: u64,
    position: usize,
    root_pid: Option<u64>,
    root_position: usize,
    sub_position: usize,
    flat: bool,
    paging: Paging,
) -> String {
    let path = if position == 1 {
        self::thread(tid, 1, flat)
    } else if flat {
        self::thread(tid, position.saturating_sub(2) / paging.replies + 1, true)
    } else {
        let path = self::thread(
            tid,
            root_position.saturating_sub(1) / paging.replies + 1,
            false,
        );
        if let Some(root) = root_pid {
            format!(
                "{path}?sub_root={root}&sub_page={}",
                sub_position.saturating_sub(1) / paging.sub_replies + 1
            )
        } else {
            path
        }
    };
    format!("{path}#floor-{position}")
}

/// Feed formats. The site-wide RSS is `/feed.xml`, Atom uses `/atom.xml`; a forum's two feeds live under the forum URL:
/// `/{slug}/feed.xml`, `/{slug}/atom.xml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFormat {
    Rss,
    Atom,
}

impl FeedFormat {
    pub fn file(self) -> &'static str {
        match self {
            Self::Rss => "feed.xml",
            Self::Atom => "atom.xml",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Rss => "RSS",
            Self::Atom => "Atom",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Self::Rss => "application/rss+xml",
            Self::Atom => "application/atom+xml",
        }
    }
}

/// Feed URL. An empty `slug` means site-wide.
pub fn feed(slug: &str, format: FeedFormat) -> String {
    if slug.is_empty() {
        format!("/{}", format.file())
    } else {
        format!("/{slug}/{}", format.file())
    }
}

/// Prefix of personal feed URLs; the following segment is the plaintext token.
pub const PRIVATE_FEED_PREFIX: &str = "/feeds/private/";

/// Personal feed URL. An empty `slug` means the site-wide one. The URL itself is the credential, handed to the page
/// only once, when generated.
pub fn private_feed(token: &str, slug: &str, format: FeedFormat) -> String {
    if slug.is_empty() {
        format!("{PRIVATE_FEED_PREFIX}{token}/{}", format.file())
    } else {
        format!("{PRIVATE_FEED_PREFIX}{token}/{slug}/{}", format.file())
    }
}

/// Personal feed URLs: like public feeds, the session middleware lets them straight through, and identity comes only
/// from the token in the URL.
pub(crate) fn private_feed_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(PRIVATE_FEED_PREFIX) else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let file = match segments.as_slice() {
        [token, file] | [token, _, file] if !token.is_empty() => *file,
        _ => return false,
    };
    matches!(file, "feed.xml" | "atom.xml")
}

/// The path written to the access log: the token segment of personal feed URLs is replaced with `…`, the rest is kept.
pub(crate) fn loggable_path(path: &str) -> std::borrow::Cow<'_, str> {
    match path.strip_prefix(PRIVATE_FEED_PREFIX) {
        Some(rest) => {
            let tail = rest.split_once('/').map_or("", |(_, tail)| tail);
            std::borrow::Cow::Owned(format!("{PRIVATE_FEED_PREFIX}…/{tail}"))
        }
        None => std::borrow::Cow::Borrowed(path),
    }
}

/// Feed URLs: the session middleware lets them straight through, without checking the session or issuing a CSRF cookie.
///
/// The forum segment only accepts values that can be a slug and are not in the reserved list: reserved segments are all
/// static routes, and letting them through would send `/my/feed.xml` and the like to handlers that need a session.
pub(crate) fn feed_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    let (slug_ok, file) = match rest.split_once('/') {
        None => (true, rest),
        Some((slug, file)) => (
            !slug.is_empty()
                && slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !reserved_segment(slug),
            file,
        ),
    };
    slug_ok && matches!(file, "feed.xml" | "atom.xml")
}

pub fn search(query: &str, sort: Sort, page: usize) -> String {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("q", query);
    if sort != Sort::Latest {
        params.append_pair("sort", sort.value());
    }
    if page > 1 {
        params.append_pair("page", &page.to_string());
    }
    format!("/search?{}", params.finish())
}

/// User search: `/search/users?q=…`, with `page` added from the second page on. Written the same way as thread search
/// `/search?q=…`.
pub fn member_search(query: &str, page: usize) -> String {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("q", query);
    if page > 1 {
        params.append_pair("page", &page.to_string());
    }
    format!("/search/users?{}", params.finish())
}

#[cfg(test)]
mod tests {
    /// Scans the source for `.route("/…")` and `.nest_service("/…")`; every top-level segment that could serve as a
    /// forum slug must be in the reserved list. A new top-level route that was not registered makes this fail.
    #[test]
    fn every_top_level_route_segment_is_reserved_for_forum_slugs() {
        fn visit(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, files);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        visit(&root, &mut files);
        let mut segments = std::collections::BTreeSet::new();
        for file in files {
            // API routes are all nested under `/api/v1`; the `/threads` and `/me` inside are not top-level URLs.
            // Likewise the asset routes of `static_files.rs` (`/i18n/{file}`) are nested under `/static`.
            if file.ends_with("router/api.rs") || file.ends_with("static_files.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for marker in [".route(", ".nest_service("] {
                for (at, _) in text.match_indices(marker) {
                    let rest = text[at + marker.len()..].trim_start();
                    let Some(path) = rest.strip_prefix("\"/") else {
                        continue;
                    };
                    let segment = path.split(['/', '"']).next().unwrap_or_default();
                    if !segment.is_empty() && !segment.starts_with('{') {
                        segments.insert(segment.to_owned());
                    }
                }
            }
        }
        assert!(
            segments.len() > 20,
            "too few segments found, the scan may be broken: {segments:?}"
        );
        for segment in &segments {
            let slug_shaped = segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            assert!(
                !slug_shaped || super::reserved_segment(segment),
                "the top-level route /{segment} is not in RESERVED_SEGMENTS"
            );
        }
    }

    use super::*;

    #[test]
    fn feed_paths_bypass_only_feed_routes() {
        assert_eq!(feed("", FeedFormat::Rss), "/feed.xml");
        assert_eq!(feed("", FeedFormat::Atom), "/atom.xml");
        assert_eq!(feed("daily", FeedFormat::Rss), "/daily/feed.xml");
        assert_eq!(feed("daily", FeedFormat::Atom), "/daily/atom.xml");
        for path in [
            "/feed.xml",
            "/atom.xml",
            "/daily/feed.xml",
            "/ai-2x/atom.xml",
        ] {
            assert!(feed_path(path), "{path}");
        }
        for path in [
            "feed.xml",
            "/",
            "/feeds",
            "/rss.xml",
            "//feed.xml",
            "/daily/",
            "/daily/feed.xml/",
            "/a/b/feed.xml",
            "/Daily/feed.xml",
            "/my/feed.xml",
            "/space/atom.xml",
            "/thread-1/feed.xml",
            "/static/feed.xml",
        ] {
            assert!(!feed_path(path), "{path}");
        }
    }

    #[test]
    fn private_feed_paths_bypass_sessions_and_hide_tokens_in_logs() {
        let token = "a".repeat(64);
        assert_eq!(
            private_feed(&token, "", FeedFormat::Rss),
            format!("/feeds/private/{token}/feed.xml")
        );
        assert_eq!(
            private_feed(&token, "ai", FeedFormat::Atom),
            format!("/feeds/private/{token}/ai/atom.xml")
        );
        for path in [
            format!("/feeds/private/{token}/feed.xml"),
            format!("/feeds/private/{token}/atom.xml"),
            format!("/feeds/private/{token}/daily/feed.xml"),
        ] {
            assert!(private_feed_path(&path), "{path}");
            assert!(sensitive_page(&path), "{path}");
            let logged = loggable_path(&path);
            assert!(!logged.contains(&token), "{logged}");
            assert!(logged.starts_with("/feeds/private/…/"), "{logged}");
        }
        for path in [
            "/feeds",
            "/feeds/private/",
            "/feeds/private//feed.xml",
            "/feeds/private/x/y/z/feed.xml",
            "/feeds/private/x/index.html",
        ] {
            assert!(!private_feed_path(path), "{path}");
        }
        assert_eq!(loggable_path("/feeds"), "/feeds");
        assert!(sensitive_page("/my/feed") && sensitive_page("/my/feed/rotate"));
    }

    #[test]
    fn member_paths_normalize_uid_and_collection_pages() {
        for (input, expected) in [
            ("/space/12", "/space/12"),
            ("/space/012/threads-01", "/space/12/threads"),
            ("/space/12/posts-002", "/space/12/posts-2"),
            ("/space/12/threads-2", "/space/12/threads-2"),
        ] {
            assert_eq!(MemberRoute::parse(input).unwrap().path(), expected);
        }
        for input in [
            "/space",
            "/space/",
            "/space/0",
            "/space/-1",
            "/space/+1",
            "/space/1a",
            "/space/9223372036854775808",
            "/space/12/",
            "/space/12/threads-0",
            "/space/12/posts--1",
            "/space/12/posts-+1",
            "/space/12/posts-18446744073709551616",
            "/space/12/posts-1-2",
            "/space/12/threads/more",
            "/space/12/unknown",
            "/user-alice",
        ] {
            assert!(MemberRoute::parse(input).is_none(), "{input}");
        }
    }

    #[test]
    fn permanent_floors_share_first_post_and_child_pagination() {
        let mut thread = crate::content::fixtures()
            .into_iter()
            .find(|thread| thread.id == 103)
            .unwrap();
        thread.first_pid = 999;
        let paging = Paging {
            replies: 50,
            sub_replies: 20,
        };
        assert_eq!(
            post_location(&thread, 999, false, paging).unwrap(),
            "/thread-103#floor-1"
        );
        assert_eq!(
            post_location(&thread, 999, true, paging).unwrap(),
            "/thread-103?view=flat#floor-1"
        );
        assert_eq!(
            post_location(&thread, 103076, false, paging).unwrap(),
            "/thread-103?sub_root=103002&sub_page=2#floor-76"
        );
        assert_eq!(
            post_location(&thread, 103076, true, paging).unwrap(),
            "/thread-103-2?view=flat#floor-76"
        );
        assert!(post_location(&thread, 0, false, paging).is_none());
        assert!(post_location(&thread, i64::MAX, false, paging).is_none());
    }

    #[test]
    fn canonicalizes_first_pages_and_leading_zeroes() {
        for (input, expected) in [
            ("page-01", "/"),
            ("thread-0008-01", "/thread-8"),
            ("daily-01", "/daily"),
            ("thread-8-002", "/thread-8-2"),
        ] {
            assert_eq!(
                PageRoute::parse(input, &["daily"]).unwrap().path(),
                expected
            );
        }
    }

    #[test]
    fn rejects_invalid_or_overflowing_routes() {
        for input in [
            "page-0",
            "page-+1",
            "thread-1-0",
            "thread-1-2-3",
            "thread-18446744073709551616",
            "daily-0",
            "missing",
            "daily%252fadmin",
        ] {
            assert!(PageRoute::parse(input, &["daily"]).is_none(), "{input}");
        }
    }
}
