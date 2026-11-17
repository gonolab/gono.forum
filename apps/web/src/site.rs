//! This request's site: the forum URL and path prefix (sub-path deployment).
//!
//! The entry layer [`entry`] wraps the whole router: it resolves the site first; a path exactly equal to the prefix
//! gets a 301 to the home page with a trailing slash, paths starting with `{prefix}/` have the prefix stripped before
//! going to the router, and everything else is a 404 except `/healthz`, which answers at the root as well. The result goes into the request extensions and task-local
//! storage; in-site addresses on pages, redirects, cookie paths and public absolute addresses all take the prefix from
//! here and never read `settings.site.url` directly (item 4 of the coupling decision).
//!
//! With multi-domain sites off there is only the main site: the prefix is the path part of the forum URL. When on, a
//! row is looked up in the domain table by Host and path, and this request's site also carries [`Domains`]: scope, home
//! domains and redirects of main-site-only addresses all come from there.
//!
//! Outside a request (background workers, the command line, unit tests calling handlers directly) the prefix is empty
//! and behavior is the same as a root deployment.
use std::{collections::BTreeSet, fmt, future::Future, sync::Arc};

use axum::{
    extract::{OriginalUri, Request, State},
    http::{StatusCode, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::domains::{Canonical, Domain, DomainMap, Home, Lookup};

/// The resolved site.
#[derive(Clone, Debug)]
pub struct Site {
    /// Path prefix: an empty string for a root deployment, otherwise starting with `/` and not ending with `/`, e.g.
    /// `/bbs`.
    pub prefix: Arc<str>,
    /// This site's address (origin plus prefix, no trailing slash). `None` when the admin area has no forum URL, or
    /// when multi-domain is on and the host is not registered; the page frame then derives the origin from this
    /// request.
    pub url: Option<Arc<str>>,
    /// Multi-domain on and host not registered: the site address is "the origin derived from this request + the main
    /// domain's prefix" (not the forum URL).
    pub derived: bool,
    /// With multi-domain on, the domain table and which row this request is on; `None` when off, and everything is
    /// handled as the main site only.
    pub domains: Option<Arc<Domains>>,
    /// The participation requirements in effect for this request: another domain's row uses its own value when it has
    /// one, otherwise the site-wide setting. The entry layer computes it from the same settings as the page frame, and
    /// write transactions and pages take it from here; outside a request it uses the pre-change rule (verified email
    /// only).
    pub participation: crate::login::Participation,
    /// The interface language of this request (docs/architecture.md, "Languages"); the process default outside a
    /// request ([`crate::i18n::default_language`]).
    pub language: crate::i18n::Lang,
}

impl Default for Site {
    fn default() -> Self {
        Self {
            prefix: Arc::default(),
            url: None,
            derived: false,
            domains: None,
            participation: crate::login::Participation::default(),
            language: crate::i18n::default_language(),
        }
    }
}

/// The domain table as seen by this request when multi-domain is on.
#[derive(Debug)]
pub struct Domains {
    pub map: Arc<DomainMap>,
    /// Forum URL: the address of the main domain row.
    pub forum_url: Arc<str>,
    /// Which row this request falls on; `None` when the host is not registered (treated as the main domain).
    pub current: Option<i64>,
}

impl Domains {
    /// The row this request falls on (possibly the main domain).
    pub fn row(&self) -> Option<&Domain> {
        self.current.and_then(|id| self.map.get(id))
    }

    pub fn main(&self) -> Option<&Domain> {
        self.map.main(&self.forum_url)
    }

    /// The row when this request falls on **another domain**; `None` for the main domain and unregistered Hosts.
    pub fn other(&self) -> Option<&Domain> {
        let main = self.main().map(|main| main.domain_id);
        self.row().filter(|row| Some(row.domain_id) != main)
    }

    /// Top-level forums this site shows; `None` means unrestricted (treated as showing all when the main domain row is
    /// missing).
    pub fn scope(&self) -> Option<&BTreeSet<i64>> {
        match self.other() {
            Some(row) => Some(&row.forums),
            None => self.main().map(|main| &main.forums),
        }
    }

    pub fn shows(&self, top_fid: i64) -> bool {
        self.scope().is_none_or(|scope| scope.contains(&top_fid))
    }
}

/// Where an out-of-scope forum should go.
#[derive(Debug, PartialEq, Eq)]
pub enum Elsewhere {
    /// 301 to the same path under this site address.
    Moved(String),
    /// No domain shows it: 404.
    Gone,
}

tokio::task_local! {
    static SITE: Site;
}

/// Runs async code under the given site.
pub async fn scope<F: Future>(site: Site, future: F) -> F::Output {
    SITE.scope(site, future).await
}

/// Runs synchronous code under the given site: for unit tests that render outside a request (the language, the
/// prefix).
pub fn sync_scope<R>(site: Site, f: impl FnOnce() -> R) -> R {
    SITE.sync_scope(site, f)
}

/// This request's site; outside a request, the empty site of a root deployment.
pub fn current() -> Site {
    SITE.try_with(Clone::clone).unwrap_or_default()
}

/// Site snapshot at registration. Multi-domain mode only recognizes registered rows; a single site without a configured
/// forum URL keeps the normalized request origin. Groups keep the port and deployment path, and HTTP/HTTPS share one
/// group.
pub(crate) fn registration_address(origin: Option<&str>) -> String {
    let site = current();
    let address = match &site.domains {
        Some(domains) => domains.row().map(|row| row.site_url.as_str()),
        None => site.url.as_deref().or(origin),
    };
    address
        .and_then(|value| value.split_once("://"))
        .map_or_else(String::new, |(_, address)| address.to_owned())
}

/// This request's interface language; the base language outside a request.
pub fn language() -> crate::i18n::Lang {
    request_language().unwrap_or_else(crate::i18n::default_language)
}

/// This request's interface language; `None` outside a request (background work uses the site language).
pub fn request_language() -> Option<crate::i18n::Lang> {
    SITE.try_with(|site| site.language).ok()
}

/// This request's path prefix; an empty string for a root deployment.
pub fn prefix() -> Arc<str> {
    SITE.try_with(|site| site.prefix.clone())
        .unwrap_or_default()
}

/// This request's domain table; `None` when multi-domain is off or outside a request.
pub fn domains() -> Option<Arc<Domains>> {
    SITE.try_with(|site| site.domains.clone()).ok().flatten()
}

/// When this request is on another domain, does something with that row; `None` for the main domain, unregistered
/// Hosts, and when multi-domain is off.
pub fn other<R>(f: impl FnOnce(&Domain) -> R) -> Option<R> {
    let domains = domains()?;
    domains.other().map(f)
}

/// Whether top-level forum `top_fid` is shown on this site; always shown when multi-domain is off.
pub fn shows(top_fid: i64) -> bool {
    domains().is_none_or(|domains| domains.shows(top_fid))
}

/// For an out-of-scope top-level forum: the site address to redirect to, or 404; `None` when in scope.
pub fn elsewhere(top_fid: i64) -> Option<Elsewhere> {
    let domains = domains()?;
    if domains.shows(top_fid) {
        return None;
    }
    Some(match domains.map.home(&domains.forum_url, top_fid) {
        Home::Other(row) => Elsewhere::Moved(row.site_url.clone()),
        Home::Main => Elsewhere::Moved(domains.forum_url.to_string()),
        Home::Nowhere => Elsewhere::Gone,
    })
}

/// Returns another site address when canonical should point there; `None` means use this site's address.
///
/// Other domains whose canonical is set to "the main domain": every page points to the main domain. Otherwise: forum
/// pages and thread pages (`top_fid` is the containing top-level forum) point to that forum's home domain; when the
/// home is the main domain and this request is on the main domain or an unregistered Host, this site's address is used.
pub fn canonical_base(top_fid: Option<i64>) -> Option<String> {
    let domains = domains()?;
    let other = domains.other();
    if other.is_some_and(|row| row.canonical == Canonical::Main) {
        return Some(domains.forum_url.to_string());
    }
    match domains.map.home(&domains.forum_url, top_fid?) {
        Home::Other(row) if Some(row.domain_id) != domains.current => Some(row.site_url.clone()),
        Home::Main if other.is_some() => Some(domains.forum_url.to_string()),
        _ => None,
    }
}

/// Addresses that in-site thread cards and anti-spam recognize as "this site": with multi-domain on, all enabled
/// registered domains plus the forum URL; the caller adds this request's own site address.
pub fn sibling_urls() -> Vec<String> {
    let Some(domains) = domains() else {
        return Vec::new();
    };
    let mut urls: Vec<String> = domains.map.enabled_urls().map(str::to_owned).collect();
    if !urls.iter().any(|url| **url == *domains.forum_url) {
        urls.push(domains.forum_url.to_string());
    }
    urls
}

/// This site's name: other domains use their own site name, the rest use the global setting. Outputs that bypass the
/// page frame (feeds, manifest) use it; on pages the page frame has already swapped it.
pub fn site_name(settings: &crate::runtime::SiteSettings) -> String {
    other(|row| row.site_name.clone())
        .flatten()
        .unwrap_or_else(|| settings.site.display_name().to_owned())
}

/// Filters forums by this request's site scope (forums whose top-level forum is in scope stay): returns the in-scope
/// forums and the out-of-scope forum ids (ascending). The second item is empty when multi-domain is off or the scope
/// is unrestricted.
pub fn partition_forums(
    forums: &[crate::organization::ForumRecord],
) -> (Vec<&crate::organization::ForumRecord>, Vec<i64>) {
    let (inside, outside): (Vec<_>, Vec<_>) = forums
        .iter()
        .partition(|forum| shows(forum.parent_fid.unwrap_or(forum.fid)));
    let mut excluded: Vec<i64> = outside.iter().map(|forum| forum.fid).collect();
    excluded.sort_unstable();
    (inside, excluded)
}

/// Main-site-only addresses: on other domains they 301 back to the main domain.
pub fn main_only(path: &str) -> bool {
    ["/manage", "/ranks", "/verified", "/links"]
        .iter()
        .any(|root| {
            path.strip_prefix(root)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
}

/// An address that already carries the path prefix, such as a data-source record's `href` (docs/data.md), returned as
/// it is: templates write it through here so that every address in them names how it gets its prefix.
pub fn prefixed(href: &str) -> &str {
    href
}

/// Put in front of in-site addresses hard-coded in templates: `href="{{ crate::site::base() }}/rules"`.
pub fn base() -> Base {
    Base
}

/// Writes out this request's prefix without allocating. The prefix only contains lowercase letters, digits, `-` and
/// `/`, so escaping does not change it.
pub struct Base;

impl fmt::Display for Base {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        SITE.try_with(|site| f.write_str(&site.prefix))
            .unwrap_or(Ok(()))
    }
}

/// Adds this request's prefix to an in-site root-relative address; external addresses, protocol-relative addresses
/// (`//host`), anchors, query strings and empty strings are returned unchanged.
///
/// For addresses generated in Rust and handed to templates or redirect headers. These are always root-relative and
/// never already carry the prefix.
pub fn href(path: impl AsRef<str>) -> String {
    let path = path.as_ref();
    if local(path) {
        let prefix = prefix();
        let mut out = String::with_capacity(prefix.len() + path.len());
        out.push_str(&prefix);
        out.push_str(path);
        out
    } else {
        path.to_owned()
    }
}

/// An in-site address under a given prefix: used to redirect under the new prefix after the forum URL's path changes;
/// otherwise use [`href`].
pub fn href_under(prefix: &str, path: &str) -> String {
    if local(path) {
        format!("{prefix}{path}")
    } else {
        path.to_owned()
    }
}

/// Root-relative addresses written in member post bodies: adds the prefix, but not again when the address already
/// equals the prefix or starts with `{prefix}/`.
pub fn content_href(path: &str) -> String {
    let prefix = prefix();
    if !local(path) || prefix.is_empty() || within(&prefix, path) {
        return path.to_owned();
    }
    format!("{prefix}{path}")
}

/// Strips the prefix from an address under this request's prefix (exactly equal to the prefix counts as the home page
/// `/`); returned unchanged when not under the prefix or for a root deployment.
pub fn unprefixed(path: &str) -> std::borrow::Cow<'_, str> {
    let prefix = prefix();
    if prefix.is_empty() || !within(&prefix, path) {
        return std::borrow::Cow::Borrowed(path);
    }
    let rest = &path[prefix.len()..];
    if rest.starts_with('/') {
        std::borrow::Cow::Owned(rest.to_owned())
    } else {
        std::borrow::Cow::Owned(format!("/{rest}"))
    }
}

/// A root-relative address that is not protocol-relative or written with backslashes.
fn local(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\")
}

/// Whether `path` (may carry a query string and anchor) is already under the prefix: equal to the prefix, or the prefix
/// immediately followed by `/`, `?` or `#`.
fn within(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

/// The path prefix of a normalized forum URL: `https://demo.com/bbs` → `/bbs`, root deployment → empty string.
pub fn prefix_of(url: &str) -> &str {
    let Some(scheme_end) = url.find("://") else {
        return "";
    };
    let rest = &url[scheme_end + 3..];
    rest.find('/').map_or("", |slash| &rest[slash..])
}

/// The path with the prefix stripped. Returned unchanged when the prefix is empty; `None` when the path exactly equals
/// the prefix (home page without the slash) or is not under the prefix.
pub fn strip<'a>(prefix: &str, path: &'a str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    path.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('/'))
}

/// The result of site resolution.
enum Resolved {
    Site(Site),
    /// The host is registered, but the path is not under any of its rows.
    Outside,
    /// The known path prefix and the reason reading failed. Business requests must stop at the entry, and must not
    /// treat an unknown domain scope as the whole site.
    Failed(Site, crate::identity::IdentityError),
}

/// Site resolution. With multi-domain off there is only the main site: the forum URL is `settings.site.url` and the
/// prefix is its path part. When on, a row is looked up in the domain table by Host and path, and an unregistered host
/// is treated as the main domain.
///
/// When the settings or the domain table cannot be read, the error is kept; the main-site prefix obtained before
/// reading the domain table can still be used for health checks and static assets.
async fn resolve(state: &crate::router::AppState, host: Option<&str>, path: &str) -> Resolved {
    let block = match state.global.settings(&state.db).await {
        Ok(block) => block,
        Err(error) => return Resolved::Failed(Site::default(), error),
    };
    let settings = &block.settings;
    let participation = crate::login::Participation::parse(&settings.login.participation);
    let language = settings.site.lang();
    let url = settings.site.url.as_str();
    if url.is_empty() {
        return Resolved::Site(Site {
            participation,
            language,
            ..Site::default()
        });
    }
    let main = Site {
        prefix: prefix_of(url).into(),
        url: Some(url.into()),
        participation,
        language,
        ..Site::default()
    };
    // The switch being on with an empty forum URL can only come from editing the database directly: saving blocks it,
    // so here it is handled as the main site only.
    if !settings.domains.enabled {
        return Resolved::Site(main);
    }
    let map = match state.global.domains(&state.db, settings.cache_ttl()).await {
        Ok(map) => map,
        Err(error) => return Resolved::Failed(main, error.into()),
    };
    if map.main(url).is_none() {
        tracing::warn!(
            forum_url = url,
            "multi-domain sites are on, but no domain table row has an address equal to the forum URL; the main domain and unregistered Hosts show all forums"
        );
    }
    // Without a Host (HTTP/1.0-style requests) no site address can be derived, so it is treated as the main domain.
    let Some(host) = host else {
        let current = map.main(url).map(|main| main.domain_id);
        return Resolved::Site(Site {
            domains: Some(Arc::new(Domains {
                map: map.clone(),
                forum_url: url.into(),
                current,
            })),
            ..main
        });
    };
    let authority = crate::domains::request_authority(host);
    let domains = |current: Option<i64>| {
        Some(Arc::new(Domains {
            map: map.clone(),
            forum_url: url.into(),
            current,
        }))
    };
    match map.resolve(&authority, path) {
        Lookup::Row(row) => Resolved::Site(Site {
            prefix: row.prefix().into(),
            url: Some(row.site_url.as_str().into()),
            derived: false,
            domains: domains(Some(row.domain_id)),
            // The main domain row's column is always NULL, following the site-wide setting.
            participation: row
                .participation
                .as_deref()
                .filter(|_| row.site_url != url)
                .map_or(participation, crate::login::Participation::parse),
            // Same rule for the language: another domain's own column, otherwise the site language.
            language: row
                .language
                .filter(|_| row.site_url != url)
                .unwrap_or(language),
        }),
        Lookup::Outside => Resolved::Outside,
        Lookup::Unregistered => Resolved::Site(Site {
            prefix: main.prefix,
            url: None,
            derived: true,
            domains: domains(None),
            participation,
            language,
        }),
    }
}

/// Entry layer: wraps the whole router (`router::build_with_global`) and strips the prefix before routing.
///
/// When stripping the prefix, `OriginalUri` is also replaced with the stripped address: axum only records it when the
/// request does not have one yet, so normalization checks in handlers, the session and debug middleware, the
/// sensitive-page list and access-log redaction all see root-relative paths, the same as a root deployment.
pub(crate) async fn entry(
    State(state): State<Arc<crate::router::AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(|a| a.as_str().to_owned()));
    let site = match resolve(&state, host.as_deref(), request.uri().path()).await {
        Resolved::Site(site) => site,
        Resolved::Outside => {
            return (
                StatusCode::NOT_FOUND,
                format!("{}\n", crate::i18n::current().common_error_page_not_found()),
            )
                .into_response();
        }
        Resolved::Failed(site, error) => {
            let path = strip(&site.prefix, request.uri().path())
                .or_else(|| (request.uri().path() == "/healthz").then_some("/healthz"))
                .unwrap_or("");
            // The health check bounds its own dependency checks; the static directory only serves files, including the
            // error page styles. Every other entry needs to know the site scope.
            if !(path == "/healthz" || path == "/static" || path.starts_with("/static/")) {
                return scope(site, async {
                    crate::router::site_error(&state, path, error)
                })
                .await;
            }
            site
        }
    };
    if !site.prefix.is_empty() {
        let path = request.uri().path();
        if path == &*site.prefix {
            // The home page is `/bbs/`; without the slash it gets a 301 there, keeping the query string.
            let location = match request.uri().query() {
                Some(query) => format!("{}/?{query}", site.prefix),
                None => format!("{}/", site.prefix),
            };
            return (
                StatusCode::MOVED_PERMANENTLY,
                [(header::LOCATION, location)],
            )
                .into_response();
        }
        // `/healthz` also answers outside the prefix, so that a check on the machine itself (forum.install.sh after
        // an upgrade) works without the forum address, which is only in the database.
        let rest = match strip(&site.prefix, path) {
            Some(rest) => rest,
            None if path == "/healthz" => path,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("{}\n", crate::i18n::current().common_error_page_not_found()),
                )
                    .into_response();
            }
        };
        let stripped = match request.uri().query() {
            Some(query) => format!("{rest}?{query}"),
            None => rest.to_owned(),
        };
        // Only path and query string are replaced; scheme and host stay (with HTTP/2 lacking `Host`, the origin is
        // derived from `:authority`).
        let mut parts = request.uri().clone().into_parts();
        let Ok(path_and_query) = stripped.parse() else {
            return (
                StatusCode::BAD_REQUEST,
                format!("{}\n", crate::i18n::current().common_error_bad_address()),
            )
                .into_response();
        };
        parts.path_and_query = Some(path_and_query);
        let Ok(uri) = Uri::from_parts(parts) else {
            return (
                StatusCode::BAD_REQUEST,
                format!("{}\n", crate::i18n::current().common_error_bad_address()),
            )
                .into_response();
        };
        *request.uri_mut() = uri.clone();
        request.extensions_mut().insert(OriginalUri(uri));
    }
    // Main-site-only addresses on other domains go back to the main domain: only GET and HEAD are redirected, so
    // form submissions are not turned into GET by the browser.
    if let Some(domains) = &site.domains
        && domains.other().is_some()
        && main_only(request.uri().path())
    {
        let method = request.method();
        if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
            return (
                StatusCode::NOT_FOUND,
                format!("{}\n", crate::i18n::current().common_error_page_not_found()),
            )
                .into_response();
        }
        let location = match request.uri().query() {
            Some(query) => format!("{}{}?{query}", domains.forum_url, request.uri().path()),
            None => format!("{}{}", domains.forum_url, request.uri().path()),
        };
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, location)],
        )
            .into_response();
    }
    request.extensions_mut().insert(site.clone());
    scope(site, next.run(request)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_of_reads_the_path_part() {
        assert_eq!(prefix_of("https://demo.com"), "");
        assert_eq!(prefix_of("https://demo.com/bbs"), "/bbs");
        assert_eq!(prefix_of("http://192.0.2.10:8080/a/b"), "/a/b");
        assert_eq!(prefix_of(""), "");
    }

    #[test]
    fn strip_only_accepts_paths_under_the_prefix() {
        assert_eq!(strip("", "/thread-5"), Some("/thread-5"));
        assert_eq!(strip("/bbs", "/bbs/"), Some("/"));
        assert_eq!(strip("/bbs", "/bbs/thread-5"), Some("/thread-5"));
        assert_eq!(strip("/bbs", "/bbs"), None);
        assert_eq!(strip("/bbs", "/bbsx/thread-5"), None);
        assert_eq!(strip("/bbs", "/thread-5"), None);
    }

    #[tokio::test]
    async fn single_site_registration_uses_configured_address_then_origin() {
        let origin = Some("http://visit.test:8080");
        assert_eq!(registration_address(origin), "visit.test:8080");
        assert_eq!(registration_address(None), "");
        let site = Site {
            url: Some("https://forum.test/bbs".into()),
            ..Site::default()
        };
        scope(site, async {
            assert_eq!(registration_address(origin), "forum.test/bbs");
        })
        .await;
    }

    #[tokio::test]
    async fn hrefs_follow_the_request_prefix() {
        assert_eq!(href("/rules"), "/rules");
        assert_eq!(base().to_string(), "");
        let site = Site {
            prefix: "/bbs".into(),
            url: Some("https://demo.com/bbs".into()),
            ..Site::default()
        };
        scope(site, async {
            assert_eq!(href("/"), "/bbs/");
            assert_eq!(
                href("/thread-5?page=2#floor-3"),
                "/bbs/thread-5?page=2#floor-3"
            );
            for kept in [
                "https://x.com/a",
                "//cdn.x.com/a",
                "/\\x",
                "#main",
                "?q=1",
                "",
            ] {
                assert_eq!(href(kept), kept);
            }
            assert_eq!(base().to_string(), "/bbs");
            assert_eq!(content_href("/rules"), "/bbs/rules");
            assert_eq!(content_href("/bbs"), "/bbs");
            assert_eq!(content_href("/bbs/thread-5"), "/bbs/thread-5");
            assert_eq!(content_href("/bbs?x"), "/bbs?x");
            assert_eq!(content_href("/bbsx"), "/bbs/bbsx");
            assert_eq!(content_href("//x.com"), "//x.com");
            assert_eq!(unprefixed("/bbs/thread-5"), "/thread-5");
            assert_eq!(unprefixed("/bbs"), "/");
            assert_eq!(unprefixed("/bbs?x=1"), "/?x=1");
            assert_eq!(unprefixed("/thread-5"), "/thread-5");
            assert_eq!(unprefixed("/bbsx"), "/bbsx");
        })
        .await;
    }

    /// Outputs always take the prefix from the request (checked by scanning the source): templates do not hard-code the
    /// root path, redirect headers go through `href`, mail does not replace paths wholesale, and only files on the
    /// allow list read the forum URL directly.
    #[test]
    fn exits_take_the_prefix_from_the_request() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut problems = Vec::new();
        for (path, text) in files(&root.join("templates"), "html") {
            for attr in ["href", "src", "action", "hx-post", "hx-get", "formaction"] {
                let needle = format!("{attr}=\"");
                for (at, _) in text.match_indices(&needle) {
                    let before = text[..at].chars().next_back();
                    if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                        continue;
                    }
                    let value = &text[at + needle.len()..];
                    let interpolated = value.starts_with("{{") || value.starts_with("{%");
                    if (value.starts_with('/') || interpolated)
                        && !value.starts_with("{{ crate::site::")
                    {
                        let line = text[..at].lines().count();
                        problems.push(format!(
                            "{path}:{line} {attr} does not get its prefix through crate::site"
                        ));
                    }
                }
            }
        }
        let allowed_site_url = [
            "src/site.rs",
            "src/runtime.rs",
            "src/runtime/settings.rs",
            "src/runtime/kv.rs",
            "src/config.rs",
            "src/mail.rs",
            "src/router/manage_settings.rs",
            // Multi-domain sites identify the main domain row by the forum URL; not an output.
            "src/domains/write.rs",
            "src/router/domains.rs",
            "src/bin/gono-forum-cli.rs",
            // The setup wizard runs before there is any site: its finished page sends the administrator to the
            // saved forum address.
            "src/setup.rs",
            "src/test_support.rs",
        ];
        for (path, text) in files(&root.join("src"), "rs") {
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                let writes_header = [
                    "header::LOCATION",
                    "\"HX-Redirect\"",
                    "\"hx-redirect\"",
                    "\"HX-Push-Url\"",
                ]
                .iter()
                .any(|name| code.contains(name))
                    && !code.contains("contains_key")
                    && !code.contains(".get(");
                // The header name and its value may be split across adjacent lines (the value is computed before the
                // response is assembled).
                let prefixed = lines[index.saturating_sub(4)..(index + 2).min(lines.len())]
                    .iter()
                    .any(|near| near.contains("site::href"));
                if writes_header && !prefixed && path != "src/site.rs" && !in_tests(&lines, index) {
                    problems.push(format!(
                        "{path}:{} a redirect header does not go through crate::site::href",
                        index + 1
                    ));
                }
                if path.starts_with("src/mail") && code.contains(".set_path(") {
                    problems.push(format!(
                        "{path}:{} mail links must not use set_path (it drops the path prefix)",
                        index + 1
                    ));
                }
                if code.contains("site.url")
                    && !allowed_site_url.contains(&path.as_str())
                    && !in_tests(&lines, index)
                {
                    problems.push(format!(
                        "{path}:{} an output reads the forum URL directly; take it from the request's site",
                        index + 1
                    ));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Whether this line comes after `#[cfg(test)]` (each file's unit tests sit at the end).
    fn in_tests(lines: &[&str], index: usize) -> bool {
        lines[..index]
            .iter()
            .any(|line| line.trim() == "#[cfg(test)]")
    }

    fn files(dir: &std::path::Path, extension: &str) -> Vec<(String, String)> {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == extension) {
                    let name = path
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.push((name, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        out
    }
}
