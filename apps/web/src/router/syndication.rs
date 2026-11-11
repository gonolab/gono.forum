//! Syndication: public RSS/Atom feeds, the feed help page, page auto-discovery links and thread structured data.
//!
//! Feeds are anonymous public resources: the session middleware lets them straight through (see
//! [`crate::urls::feed_path`]) and the handlers never look at the visitor, so responses can be cached as
//! `public` and are revalidated with an ETag every time.
use super::extract::Page;
use super::*;
use crate::{content::feeds::FeedItem, organization::ForumRecord, urls::FeedFormat};
use axum::{
    extract::Path,
    http::{HeaderMap, HeaderName},
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/feed.xml", get(site_rss))
        .route("/atom.xml", get(site_atom))
        .route("/{segment}/feed.xml", get(forum_rss))
        .route("/{segment}/atom.xml", get(forum_atom))
        .route("/feeds", get(feeds_page))
        .route("/feeds/private/{token}/feed.xml", get(private_site_rss))
        .route("/feeds/private/{token}/atom.xml", get(private_site_atom))
        .route(
            "/feeds/private/{token}/{segment}/feed.xml",
            get(private_forum_rss),
        )
        .route(
            "/feeds/private/{token}/{segment}/atom.xml",
            get(private_forum_atom),
        )
}

async fn private_site_rss(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    private_feed(&state, &token, "", FeedFormat::Rss, (&headers, origin)).await
}
async fn private_site_atom(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    private_feed(&state, &token, "", FeedFormat::Atom, (&headers, origin)).await
}
async fn private_forum_rss(
    State(state): State<Arc<AppState>>,
    Path((token, segment)): Path<(String, String)>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    private_feed(
        &state,
        &token,
        &segment,
        FeedFormat::Rss,
        (&headers, origin),
    )
    .await
}
async fn private_forum_atom(
    State(state): State<Arc<AppState>>,
    Path((token, segment)): Path<(String, String)>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    private_feed(
        &state,
        &token,
        &segment,
        FeedFormat::Atom,
        (&headers, origin),
    )
    .await
}

async fn site_rss(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    feed(&state, "", FeedFormat::Rss, (&headers, origin)).await
}
async fn site_atom(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    feed(&state, "", FeedFormat::Atom, (&headers, origin)).await
}
async fn forum_rss(
    State(state): State<Arc<AppState>>,
    Path(segment): Path<String>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    feed(&state, &segment, FeedFormat::Rss, (&headers, origin)).await
}
async fn forum_atom(
    State(state): State<Arc<AppState>>,
    Path(segment): Path<String>,
    headers: HeaderMap,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    feed(&state, &segment, FeedFormat::Atom, (&headers, origin)).await
}

/// Description of the channel itself: the whole site or one forum.
struct Channel {
    title: String,
    description: String,
    /// The HTML page the channel corresponds to.
    page: String,
    /// The feed's own URL.
    feed: String,
}

async fn feed(
    state: &AppState,
    slug: &str,
    format: FeedFormat,
    (headers, origin): (&HeaderMap, Option<String>),
) -> Response {
    let settings = match state
        .global
        .settings(&state.db)
        .await
        .map(|block| block.settings.clone())
    {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(state, error),
    };
    // The forum must be readable by anonymous visitors: enabled and public, with its parent also enabled and
    // public. Members-only and private forums are always 404. A top-level forum's feed aggregates its child
    // forums that anonymous visitors can read. Readable forums come from the anonymous copy of the process forum
    // cache (docs/settings-and-caching.md, "The process cache"), which this process refreshes immediately after
    // admin-area saves, reordering, deletion and merges are committed.
    // Which forums' threads are included is computed in code from this forum cache; the site-wide feed reads it
    // too.
    let forums = match state
        .global
        .forums(&state.db, false, settings.cache_ttl())
        .await
    {
        Ok(forums) => forums,
        Err(error) => return database_error(state, error),
    };
    // The site-wide feed includes forums within this site's scope; feeds of forums outside the scope redirect
    // to the canonical domain (multi-domain sites).
    let (forum, scope, listed) = if slug.is_empty() {
        (
            None,
            content::ListScope::All,
            super::frame::listable_forums_of(&forums),
        )
    } else {
        match forums.iter().find(|f| f.slug == slug) {
            Some(forum) => {
                if let Some(target) = crate::site::elsewhere(forum.parent_fid.unwrap_or(forum.fid))
                {
                    return moved(target, &urls::feed(&forum.slug, format));
                }
                // A top-level forum together with its child forums readable by anonymous visitors.
                let scope = content::ListScope::of_forum(forum.fid, forum.parent_fid);
                let listed = family_of(forum, &forums, |_| true);
                (Some(forum.clone()), scope, listed)
            }
            None => return missing(),
        }
    };
    // Feeds bypass the page frame: the Redis cache TTL and the number of entries both follow the settings read
    // this time (the frame's placeholder values cannot be used).
    let cache = state
        .cache
        .timed(crate::cache::Lifetimes::from_settings(&settings));
    let limit = usize::try_from(settings.feed.items).unwrap_or(0);
    let mut items =
        match content::feeds::cached_items(&state.db, &cache, &scope, &listed, limit).await {
            Ok(items) => items,
            Err(error) => return database_error(state, error),
        };
    // Forum names and URLs are taken by `fid` from this anonymous forum cache (thread rows do not copy them):
    // entries include only threads in these forums.
    for item in &mut items {
        item.forum_name = content::forum_name(&forums, item.fid);
        item.forum_slug = content::forum_slug(&forums, item.fid);
    }
    let base = match crate::runtime::site_origin(&settings, origin.as_deref()) {
        Ok(base) => base,
        Err(error) => return database_error(state, anyhow::Error::new(error)),
    };
    let base = base.as_str();
    let site_name = crate::site::site_name(&settings);
    let channel = match &forum {
        None => Channel {
            title: site_name.clone(),
            description: settings.site.description_in(crate::i18n::current()),
            page: format!("{base}/"),
            feed: format!("{base}{}", urls::feed("", format)),
        },
        Some(forum) => Channel {
            title: format!("{} - {site_name}", forum.name),
            description: if forum.description.trim().is_empty() {
                settings.site.description_in(crate::i18n::current())
            } else {
                forum.description.clone()
            },
            page: format!("{base}/{}", forum.slug),
            feed: format!("{base}{}", urls::feed(&forum.slug, format)),
        },
    };
    let body = match format {
        FeedFormat::Rss => rss(&channel, &items, base),
        FeedFormat::Atom => atom(&channel, &items, base),
    };
    respond(format, body, settings.seo.indexing, headers)
}

/// Personal feeds: identity comes only from the token in the URL. A wrong token, one that has been regenerated
/// or disabled, or an owner who is frozen or deleted all give the same 404 as a nonexistent forum. The
/// site-wide feed drops the forums the owner has hidden or blocked; the single-forum feed works as usual for a
/// hidden forum and is 404 for a blocked one.
/// Only the owner can get the URL, and the content does not go into the shared cache.
async fn private_feed(
    state: &AppState,
    token: &str,
    slug: &str,
    format: FeedFormat,
    (headers, origin): (&HeaderMap, Option<String>),
) -> Response {
    let owner = match crate::content::feed_tokens::resolve(&state.db, token).await {
        Ok(Some(owner)) => owner,
        Ok(None) => return missing(),
        Err(error) => return database_error(state, error),
    };
    let settings = match state
        .global
        .settings(&state.db)
        .await
        .map(|block| block.settings.clone())
    {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(state, error),
    };
    // Without members-only threads, only forums anonymous visitors can read count, and the single-forum feed of
    // a members-only forum is 404 like the public feed.
    // The process forum cache is taken from the owner's point of view: readable forums depend
    // only on whether one is signed in, not on the uid, so when the owner includes members-only threads the
    // member copy is used, otherwise the anonymous copy. The owner's identity (token, freeze, deletion) and
    // their hidden and blocked forums are still read from the database every time.
    let forums = match state
        .global
        .forums(&state.db, owner.include_members, settings.cache_ttl())
        .await
    {
        Ok(forums) => forums,
        Err(error) => return database_error(state, error),
    };
    let filters = match crate::interaction::forum_filter::ForumFilters::load(
        &state.db,
        owner.uid,
        forums.iter().map(|forum| (forum.fid, forum.parent_fid)),
    )
    .await
    {
        Ok(filters) => filters,
        Err(error) => return database_error(state, error),
    };
    // Multi-domain sites: a forum outside the scope is 404 like a blocked one, and the site-wide feed leaves out
    // forums outside the scope.
    let (_, out_of_scope) = crate::site::partition_forums(&forums);
    let filters = filters.with_scope(out_of_scope);
    let forum = if slug.is_empty() {
        None
    } else {
        match forums.iter().find(|f| f.slug == slug) {
            Some(forum)
                if !filters.is_blocked(forum.fid)
                    && crate::site::shows(forum.parent_fid.unwrap_or(forum.fid)) =>
            {
                Some(forum)
            }
            _ => return missing(),
        }
    };
    // Single-forum feed: a top-level forum aggregates its child forums, leaving out child
    // forums the owner has blocked. The site-wide feed covers the whole site minus the forums the owner has
    // hidden or blocked (including those outside the scope, see above).
    let (scope, listed) = match forum {
        Some(forum) => (
            content::ListScope::of_forum(forum.fid, forum.parent_fid),
            family_of(forum, &forums, |child| !filters.is_blocked(child.fid)),
        ),
        None => {
            let excluded = filters.timeline_excluded();
            (
                content::ListScope::All,
                forums
                    .iter()
                    .map(|forum| forum.fid)
                    .filter(|fid| !excluded.contains(fid))
                    .collect(),
            )
        }
    };
    let blocked = match content::blocked_authors(&state.db, owner.uid).await {
        Ok(blocked) => blocked,
        Err(error) => return database_error(state, error),
    };
    let member = content::feeds::MemberScope {
        uid: owner.uid,
        include_members: owner.include_members,
        forums: &listed,
        blocked_uids: &blocked,
    };
    let limit = usize::try_from(settings.feed.items).unwrap_or(0);
    let mut items = match content::feeds::member_items(&state.db, &scope, &member, limit).await {
        Ok(items) => items,
        Err(error) => return database_error(state, error),
    };
    // As in the public feeds: forum names and URLs by `fid`, from the owner's view of the forum cache.
    for item in &mut items {
        item.forum_name = content::forum_name(&forums, item.fid);
        item.forum_slug = content::forum_slug(&forums, item.fid);
    }
    let base = match crate::runtime::site_origin(&settings, origin.as_deref()) {
        Ok(base) => base,
        Err(error) => return database_error(state, anyhow::Error::new(error)),
    };
    let base = base.as_str();
    let site_name = crate::site::site_name(&settings);
    let channel = match forum {
        None => Channel {
            title: crate::i18n::current().site_feeds_private_title(site_name.as_str()),
            description: settings.site.description_in(crate::i18n::current()),
            page: format!("{base}/"),
            feed: format!("{base}{}", urls::private_feed(token, "", format)),
        },
        Some(forum) => Channel {
            title: crate::i18n::current()
                .site_feeds_private_forum_title(forum.name.as_str(), site_name.as_str()),
            description: if forum.description.trim().is_empty() {
                settings.site.description_in(crate::i18n::current())
            } else {
                forum.description.clone()
            },
            page: format!("{base}/{}", forum.slug),
            feed: format!("{base}{}", urls::private_feed(token, &forum.slug, format)),
        },
    };
    let body = match format {
        FeedFormat::Rss => rss(&channel, &items, base),
        FeedFormat::Atom => atom(&channel, &items, base),
    };
    let mut response = respond(format, body, false, headers);
    let headers = response.headers_mut();
    // The content differs per person, so proxies must not keep a copy; the URL is the credential and is not
    // passed on in redirects.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// Feed of a forum outside the scope: 301 to the same feed on the canonical domain, or the same 404 as a
/// nonexistent forum.
fn moved(target: crate::site::Elsewhere, path: &str) -> Response {
    match target {
        // Cross-site absolute URL that already carries the target site's prefix; it does not go through
        // `site::href` again.
        crate::site::Elsewhere::Moved(base) => (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, format!("{base}{path}"))],
        )
            .into_response(),
        crate::site::Elsewhere::Gone => missing(),
    }
}

fn missing() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        format!("{}\n", crate::i18n::current().site_feeds_not_found()),
    )
        .into_response()
}

/// The content is the same for everyone: `public` lets proxies and readers keep a copy; `no-cache` requires
/// revalidation with the ETag every time, so withdrawn or hidden content does not keep showing (content state
/// changes must reach readers within 60 seconds).
fn respond(format: FeedFormat, body: String, indexable: bool, headers: &HeaderMap) -> Response {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(body.as_bytes());
    let etag = format!(
        "\"{}\"",
        digest
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let matched = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == etag || tag == "*");
    let mut response = if matched {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        body.into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("{}; charset=utf-8", format.mime()))
            .expect("fixed media type"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, no-cache"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    // With indexing turned off (test site) this stays consistent with robots.txt and the pages' noindex.
    if !indexable {
        headers.insert(
            HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("noindex, nofollow"),
        );
    }
    response
}

/// XML 1.0 text and attribute values. Escapes the five special characters and removes characters XML does
/// not allow (C0 control characters other than tab, newline and carriage return, plus U+FFFE and U+FFFF);
/// Rust strings contain no surrogates, and non-BMP characters are kept as they are.
fn xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c < ' ' || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

fn utc(timestamp: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(timestamp, 0).unwrap_or_default()
}

/// RFC 3339, UTC. Used by Atom and JSON-LD.
fn rfc3339(timestamp: i64) -> String {
    utc(timestamp).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// RFC 822 date (four-digit year), used by RSS.
fn rfc822(timestamp: i64) -> String {
    utc(timestamp)
        .format("%a, %d %b %Y %H:%M:%S +0000")
        .to_string()
}

fn rss(channel: &Channel, items: &[FeedItem], base: &str) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n<channel>\n",
    );
    out.push_str(&format!(
        "<title>{}</title>\n<link>{}</link>\n<description>{}</description>\n<language>{}</language>\n<atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>\n",
        xml(&channel.title),
        xml(&channel.page),
        xml(&channel.description),
        // The language of the domain the feed was requested on, like the pages there.
        crate::i18n::current().tag(),
        xml(&channel.feed),
    ));
    if let Some(latest) = items.iter().map(|item| item.updated).max() {
        out.push_str(&format!(
            "<lastBuildDate>{}</lastBuildDate>\n",
            rfc822(latest)
        ));
    }
    for item in items {
        let link = xml(&format!("{base}/thread-{}", item.tid));
        out.push_str(&format!(
            "<item>\n<title>{}</title>\n<link>{link}</link>\n<guid isPermaLink=\"true\">{link}</guid>\n<pubDate>{}</pubDate>\n<dc:creator>{}</dc:creator>\n<category>{}</category>\n<description>{}</description>\n</item>\n",
            xml(&item.title),
            rfc822(item.published),
            xml(&item.author),
            xml(&item.forum_name),
            xml(&item.summary),
        ));
    }
    out.push_str("</channel>\n</rss>\n");
    out
}

fn atom(channel: &Channel, items: &[FeedItem], base: &str) -> String {
    // An empty feed has no entry to take a time from, and `updated` is required, so the epoch is used.
    let updated = items.iter().map(|item| item.updated).max().unwrap_or(0);
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"{}\">\n",
        crate::i18n::current().tag()
    );
    out.push_str(&format!(
        "<id>{}</id>\n<title>{}</title>\n<subtitle>{}</subtitle>\n<link rel=\"self\" type=\"application/atom+xml\" href=\"{}\"/>\n<link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n<updated>{}</updated>\n",
        xml(&channel.feed),
        xml(&channel.title),
        xml(&channel.description),
        xml(&channel.feed),
        xml(&channel.page),
        rfc3339(updated),
    ));
    for item in items {
        let link = xml(&format!("{base}/thread-{}", item.tid));
        out.push_str(&format!(
            "<entry>\n<id>{link}</id>\n<title type=\"text\">{}</title>\n<link rel=\"alternate\" type=\"text/html\" href=\"{link}\"/>\n<published>{}</published>\n<updated>{}</updated>\n<author><name>{}</name></author>\n<category term=\"{}\" label=\"{}\"/>\n<summary type=\"text\">{}</summary>\n</entry>\n",
            xml(&item.title),
            rfc3339(item.published),
            rfc3339(item.updated),
            xml(&item.author),
            xml(&item.forum_slug),
            xml(&item.forum_name),
            xml(&item.summary),
        ));
    }
    out.push_str("</feed>\n");
    out
}

/// Forums a forum feed includes: the forum itself; a top-level forum additionally gets its child forums in
/// `forums` (those satisfying `keep`).
fn family_of(
    forum: &ForumRecord,
    forums: &[ForumRecord],
    keep: impl Fn(&ForumRecord) -> bool,
) -> Vec<i64> {
    std::iter::once(forum.fid)
        .chain(
            forums
                .iter()
                .filter(|child| {
                    forum.parent_fid.is_none() && child.parent_fid == Some(forum.fid) && keep(child)
                })
                .map(|child| child.fid),
        )
        .collect()
}

/// Whether a forum record already fetched is visible to anonymous visitors: public itself and, when it has a
/// parent, the parent is public too.
///
/// `forums` are the forums the page frame of this request fetched for the visitor (all enabled, with readable
/// parents); a signed-in member's frame also has members-only forums, so it has to be checked again. A
/// missing parent counts as not public.
pub(super) fn forum_public(forum: &ForumRecord, forums: &[ForumRecord]) -> bool {
    forum.visibility == "public"
        && forum.parent_fid.is_none_or(|parent| {
            forums
                .iter()
                .any(|f| f.fid == parent && f.visibility == "public")
        })
}

/// One feed for `<link rel="alternate">` auto-discovery.
pub(super) struct FeedLink {
    pub(super) title: String,
    pub(super) href: String,
    pub(super) mime: &'static str,
}

/// The home page list gets the site-wide feed; a forum list gets the forum feed only when the forum is visible
/// to anonymous visitors, otherwise the feed itself is 404.
pub(super) fn feed_links(state: &AppState, forum: Option<&ForumRecord>) -> Vec<FeedLink> {
    let base = state.site_url().trim_end_matches('/');
    let site = &state.frame.settings.site.name;
    let (slug, title) = match forum {
        None => ("", site.clone()),
        Some(forum) if forum_public(forum, &state.frame.forums) => {
            (forum.slug.as_str(), format!("{} - {site}", forum.name))
        }
        Some(_) => return Vec::new(),
    };
    [FeedFormat::Rss, FeedFormat::Atom]
        .into_iter()
        .map(|format| FeedLink {
            title: format!("{title} {}", format.label()),
            href: format!("{base}{}", urls::feed(slug, format)),
            mime: format.mime(),
        })
        .collect()
}

/// The schema.org `DiscussionForumPosting` of a thread page, with only fields anonymous visitors can also see:
/// title, canonical URL, publication time, author display name and reply count, plus the excerpt `text` that
/// is already public in the page description, the first-post edit time `dateModified` and the forum
/// `isPartOf`. Callers output it only for threads visible to anonymous
/// visitors and not withdrawn. Without an excerpt (the first post has only images) there is no `text`, and
/// without an edit there is no `dateModified`.
///
/// The result goes straight into `<script type="application/ld+json">`. Such a data block is not executed,
/// the site CSP's `script-src 'self'` does not block it, and there is no need to allow inline scripts or add
/// a nonce for it; but a `</script>` in the content would end the element early, so `<`, `>` and `&` are
/// always written as JSON escapes such as `<`, which does not change the parsed result.
/// `forum` is the name and URL of the forum (taken by `fid` from the frame's forum cache).
pub(super) fn thread_json_ld(
    thread: &Thread,
    url: &str,
    text: &str,
    (forum_name, forum_url): (&str, &str),
) -> String {
    let mut value = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "DiscussionForumPosting",
        "headline": thread.title,
        "url": url,
        "mainEntityOfPage": url,
        "datePublished": rfc3339(i64::try_from(thread.created).unwrap_or_default()),
        "author": {"@type": "Person", "name": thread.author},
        "interactionStatistic": {
            "@type": "InteractionCounter",
            "interactionType": "https://schema.org/CommentAction",
            "userInteractionCount": thread.reply_count,
        },
        "isPartOf": {"@type": "WebPage", "name": forum_name, "url": forum_url},
    });
    if !text.is_empty() {
        value["text"] = text.into();
    }
    if thread.edited_at > 0 {
        value["dateModified"] = rfc3339(thread.edited_at).into();
    }
    script_json(&value)
}

/// The schema.org `BreadcrumbList` of the breadcrumbs. `trail` holds the items of the breadcrumb row on the page
/// as `(name, in-site path)`, with the first item "Home" replaced by the site
/// name; the current page `current` is added at the end without a URL, so the list is the page's items plus the
/// current page. Callers output it only when the page is visible to anonymous visitors.
/// `base` is the site URL without a trailing slash.
pub(super) fn breadcrumb_json_ld(
    base: &str,
    site_name: &str,
    trail: &[(String, String)],
    current: &str,
) -> String {
    let mut items: Vec<serde_json::Value> = trail
        .iter()
        .enumerate()
        .map(|(index, (name, path))| {
            serde_json::json!({
                "@type": "ListItem",
                "position": index + 1,
                "name": if index == 0 { site_name } else { name.as_str() },
                "item": format!("{base}{path}"),
            })
        })
        .collect();
    items.push(serde_json::json!({
        "@type": "ListItem",
        "position": trail.len() + 1,
        "name": current,
    }));
    script_json(&serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": items,
    }))
}

/// The schema.org `ProfilePage` of a profile page; callers output it only on
/// indexable profile and collection pages. All fields are data already shown on the page, with no new
/// queries: username, UID, profile URL (collection pages also write the profile `/space/N`), avatar, plain-text
/// bio, registration time, and this page's thread and reply counts — when indexable the visitor is not signed
/// in, so these counts cover only public content. Fields are omitted when there is no avatar or the bio is
/// empty; the "Frozen" and "Being deleted" labels do not go into structured data. `image` is the avatar's absolute URL
/// or an empty string.
pub(super) fn profile_json_ld(
    member: &content::PublicMember,
    url: &str,
    image: &str,
    stats: &content::MemberStats,
) -> String {
    let counter = |action: &str, count: usize| {
        serde_json::json!({
            "@type": "InteractionCounter",
            "interactionType": format!("https://schema.org/{action}"),
            "userInteractionCount": count,
        })
    };
    let mut person = serde_json::json!({
        "@type": "Person",
        "name": member.username,
        "identifier": member.uid.to_string(),
        "url": url,
        // Threads count as "writing" and replies as "comments", consistent with the thread page's
        // `interactionStatistic` counting replies with `CommentAction`.
        "agentInteractionStatistic": [
            counter("WriteAction", stats.threads),
            counter("CommentAction", stats.replies),
        ],
    });
    if !image.is_empty() {
        person["image"] = image.into();
    }
    let bio = member.bio.trim();
    if !bio.is_empty() {
        person["description"] = bio.into();
    }
    script_json(&serde_json::json!({
        "@context": "https://schema.org",
        "@type": "ProfilePage",
        "url": url,
        "dateCreated": rfc3339(member.regdate),
        "mainEntity": person,
    }))
}

fn script_json(value: &serde_json::Value) -> String {
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[derive(Template)]
#[template(path = "site/feeds.html")]
struct FeedsTemplate<'a> {
    p: PageMeta<'a>,
    /// Links use absolute URLs and the text shows only the path: that is what fits three columns on a phone.
    base: String,
    rows: Vec<FeedRow>,
    limit: usize,
    summary_chars: usize,
}

struct FeedRow {
    name: String,
    rss: String,
    atom: String,
}

/// Feed help page: lists the two feeds of the whole site and of every forum visible to anonymous visitors.
async fn feeds_page(Page(state): Page) -> Response {
    let row = |name: String, slug: &str| FeedRow {
        name,
        rss: urls::feed(slug, FeedFormat::Rss),
        atom: urls::feed(slug, FeedFormat::Atom),
    };
    let mut rows = vec![row(crate::i18n::current().site_feeds_site().into(), "")];
    // Grouped by parent and child, child forums with an indent prefix; a top-level forum's feed includes its child
    // forums.
    let public: Vec<ForumRecord> = state
        .frame
        .forums
        .iter()
        .filter(|forum| forum_public(forum, &state.frame.forums))
        .cloned()
        .collect();
    rows.extend(
        crate::organization::grouped_by_parent(&public, |f| (f.fid, f.parent_fid))
            .into_iter()
            .map(|(forum, child)| {
                let prefix = if child {
                    crate::organization::CHILD_PREFIX
                } else {
                    ""
                };
                row(format!("{prefix}{}", forum.name), &forum.slug)
            }),
    );
    let mut p = meta(
        &state,
        crate::i18n::current().site_feeds().into(),
        crate::i18n::current().site_feeds_description().into(),
        "/feeds",
        "",
        "",
    );
    p.feeds = feed_links(&state, None);
    render(&FeedsTemplate {
        base: state.site_url().trim_end_matches('/').to_owned(),
        p,
        rows,
        limit: usize::try_from(state.frame.settings.feed.items).unwrap_or(0),
        summary_chars: content::feeds::SUMMARY_CHARS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escapes_markup_and_drops_forbidden_characters() {
        // U+20BB7 (a rare Chinese character) and the emoji are four-byte characters outside the BMP.
        assert_eq!(
            xml("A<B & \"C\" 'D'> \u{20bb7}\u{1f600}"),
            "A&lt;B &amp; &quot;C&quot; &apos;D&apos;&gt; \u{20bb7}\u{1f600}"
        );
        assert_eq!(xml("a\u{0}b\u{1}c\u{1f}d\te\nf\r"), "abcd\te\nf\r");
        assert_eq!(xml("x\u{fffe}\u{ffff}y\u{fffd}"), "xy\u{fffd}");
    }

    #[test]
    fn dates_use_utc_formats() {
        assert_eq!(rfc3339(1_788_825_600), "2026-09-08T00:00:00Z");
        assert_eq!(rfc822(1_788_825_600), "Tue, 08 Sep 2026 00:00:00 +0000");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn script_json_cannot_close_the_script_element() {
        let value = serde_json::json!({"headline": "</script><script>alert(1)</script> & <!--"});
        let text = script_json(&value);
        assert!(!text.contains('<') && !text.contains('>') && !text.contains('&'));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, value);
    }
}
