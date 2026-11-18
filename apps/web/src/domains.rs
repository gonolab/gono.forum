//! Multi-domain sites: the in-memory form of the domain table (`gonof_common_site_domain` and the top-level forums it
//! shows), plus site matching in the entry layer.
//!
//! - One row per site, and the address may carry a path prefix; **the row whose address equals the forum URL
//!   (`settings.site.url`) is the main domain**, with no separate flag column.
//! - Reads go through the `domains` block of the in-process global cache ([`crate::cache::Block::Domains`]); when the
//!   switch `settings.domains.enabled` is off the entry layer does not read it at all.
//! - Scope only governs presentation, not permissions: this only answers "which site is this request, which top-level
//!   forums does it show, and where is a forum's home".
//!
//! Writes (the admin module, and the coupling with the forum URL and forums) live in [`write`]; only it writes the two
//! tables (checked by `db/source_guards.rs`).
use std::collections::BTreeSet;

use anyhow::{Context, Result};

use crate::db::{self, Database};

pub mod write;

/// The maximum number of domain table rows (including the main domain), enforced when the admin area adds one.
pub const MAX_DOMAINS: usize = 50;
/// The maximum number of link rows: each domain links at most every top-level forum, and there are at most
/// [`crate::organization::MAX_TOP_FORUMS`] of those.
pub const MAX_LINKS: usize = 20_000;

/// Reads take one row more than [`MAX_DOMAINS`] and [`MAX_LINKS`], so a table that outgrew its bound (edited in the
/// database) is an error ([`bounded`]) instead of quietly losing the rows past it.
const LOAD_SQL: &str = "SELECT domain_id,site_url,site_name,canonical,logo_aid,favicon_aid,verification,login_methods,participation,language,status,version,dateline FROM gonof_common_site_domain ORDER BY domain_id LIMIT 51";
const FORUMS_SQL: &str =
    "SELECT domain_id,fid FROM gonof_common_site_domain_forum ORDER BY domain_id,fid LIMIT 20001";

/// The check after a bounded read of `table`: more than `limit` rows came back.
pub(crate) fn bounded(table: &str, rows: usize, limit: usize) -> Result<()> {
    anyhow::ensure!(
        rows <= limit,
        crate::i18n::current().domain_error_load_bound(table, limit)
    );
    Ok(())
}

/// Where canonical points: this domain or the main domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Canonical {
    Own,
    Main,
}

impl Canonical {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "self",
            Self::Main => "main",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "self" => Some(Self::Own),
            "main" => Some(Self::Main),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Own => crate::i18n::current().domain_canonical_own(),
            Self::Main => crate::i18n::current().domain_canonical_main(),
        }
    }
}

/// One row of the domain table, together with the top-level forums it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    pub domain_id: i64,
    /// Normalized public address without a trailing slash; may carry a path prefix.
    pub site_url: String,
    /// Empty on the main domain row (the global settings apply).
    pub site_name: Option<String>,
    pub canonical: Canonical,
    pub logo_aid: Option<i64>,
    pub favicon_aid: Option<i64>,
    /// One `name=value` per line; empty on the main domain row (the global settings apply).
    pub verification: Option<String>,
    /// Extra login methods shown on the login page (a subset of [`crate::runtime::settings::LOGIN_METHODS`],
    /// comma-separated); NULL follows the site-wide `settings.login.methods`, and the main domain row is always NULL.
    pub login_methods: Option<String>,
    /// Participation requirements (a subset of [`crate::runtime::settings::PARTICIPATION`], empty for none); NULL
    /// follows the site-wide `settings.login.participation`.
    pub participation: Option<String>,
    /// The interface language of this domain; NULL follows the site language, and the main domain row is always NULL
    /// (docs/architecture.md, "Languages").
    pub language: Option<crate::i18n::Lang>,
    pub enabled: bool,
    pub version: i64,
    pub dateline: i64,
    /// Top-level forums shown; child forums follow their parent forum.
    pub forums: BTreeSet<i64>,
}

impl Domain {
    /// Host and port (`demo.com`, `127.0.0.1:58844`), already lowercase and without port 80 or 443.
    pub fn authority(&self) -> &str {
        authority_of(&self.site_url)
    }

    /// Path prefix: an empty string for the root path, otherwise e.g. `/bbs`.
    pub fn prefix(&self) -> &str {
        crate::site::prefix_of(&self.site_url)
    }

    /// Whether the request path falls under this row: the root path catches everything, sub-paths compare by segment
    /// (`/bbs` catches `/bbs` and `/bbs/…`, not `/bbsx`).
    fn covers(&self, path: &str) -> bool {
        let prefix = self.prefix();
        prefix.is_empty()
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    pub fn shows(&self, top_fid: i64) -> bool {
        self.forums.contains(&top_fid)
    }
}

/// The host and port part of a normalized address, compared with [`request_authority`]: ports 80 and 443 are dropped
/// whatever the scheme, as on the request side. URL parsing only drops the scheme's own default port, so
/// `https://demo.com:80` kept `:80` while a request for it arrives as `demo.com`, and the two never matched.
pub fn authority_of(url: &str) -> &str {
    let rest = url.find("://").map_or(url, |at| &url[at + 3..]);
    let authority = rest.find('/').map_or(rest, |slash| &rest[..slash]);
    authority
        .strip_suffix(":80")
        .or_else(|| authority.strip_suffix(":443"))
        .unwrap_or(authority)
}

/// Normalizes the request's `Host` into a form comparable with registered addresses: trims whitespace, lowercases,
/// and drops `:80` and `:443` (registered addresses carry no default port after URL parsing; the scheme is not
/// considered here, see [`DomainMap::resolve`]).
pub fn request_authority(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    for port in [":80", ":443"] {
        if let Some(bare) = host.strip_suffix(port) {
            return bare.to_owned();
        }
    }
    host
}

/// Result of the entry layer looking up a row by Host and path.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// Found an enabled row (possibly the main domain).
    Row(&'a Domain),
    /// The host is registered, but there is no root path and the path falls under no row: 404.
    Outside,
    /// The host is not registered (or has only disabled rows): treated as the main domain.
    Unregistered,
}

/// The whole domain table: all rows (including disabled ones) ordered by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomainMap {
    rows: Vec<Domain>,
}

impl DomainMap {
    pub fn new(mut rows: Vec<Domain>) -> Self {
        rows.sort_by_key(|row| row.domain_id);
        Self { rows }
    }

    /// Two queries read the whole table: domain rows and link rows, both bounded by primary key. Corrupt stored data
    /// (bad canonical or status values) is an error, and when the entry layer fails to read it, requests that need
    /// the site scope are aborted.
    pub async fn load(db: &Database) -> Result<Self> {
        let rows = db::query(LOAD_SQL).fetch_all(db.executor()).await?;
        bounded("gonof_common_site_domain", rows.len(), MAX_DOMAINS)?;
        let links = db::query(FORUMS_SQL).fetch_all(db.executor()).await?;
        bounded("gonof_common_site_domain_forum", links.len(), MAX_LINKS)?;
        let mut domains = Vec::with_capacity(rows.len());
        for row in rows {
            let canonical = db::text(&row, "canonical")?;
            let status: i64 = row.try_get("status")?;
            let language: Option<String> = row.try_get("language")?;
            let language = match language.as_deref() {
                None => None,
                Some(tag) => Some(crate::i18n::Lang::parse(tag).with_context(|| {
                    format!("domain table language is not a compiled-in language: {tag}")
                })?),
            };
            // Shown on the error page of every scoped route until the row is repaired.
            anyhow::ensure!(
                matches!(status, 0 | 1),
                crate::i18n::current().domain_error_load_status(status.to_string())
            );
            domains.push(Domain {
                domain_id: row.try_get("domain_id")?,
                site_url: db::text(&row, "site_url")?,
                site_name: row.try_get("site_name")?,
                canonical: Canonical::parse(&canonical).with_context(|| {
                    crate::i18n::current().domain_error_load_canonical(&canonical)
                })?,
                logo_aid: row.try_get("logo_aid")?,
                favicon_aid: row.try_get("favicon_aid")?,
                verification: row.try_get("verification")?,
                login_methods: row.try_get("login_methods")?,
                participation: row.try_get("participation")?,
                language,
                enabled: status == 1,
                version: row.try_get("version")?,
                dateline: row.try_get("dateline")?,
                forums: BTreeSet::new(),
            });
        }
        for link in links {
            let id: i64 = link.try_get("domain_id")?;
            if let Some(domain) = domains.iter_mut().find(|d| d.domain_id == id) {
                domain.forums.insert(link.try_get("fid")?);
            }
        }
        Ok(Self::new(domains))
    }

    pub fn rows(&self) -> &[Domain] {
        &self.rows
    }

    pub fn get(&self, domain_id: i64) -> Option<&Domain> {
        self.rows.iter().find(|row| row.domain_id == domain_id)
    }

    /// The main domain: the row whose address equals the forum URL.
    pub fn main(&self, forum_url: &str) -> Option<&Domain> {
        if forum_url.is_empty() {
            return None;
        }
        self.rows.iter().find(|row| row.site_url == forum_url)
    }

    /// Finds a row by host and path: only enabled rows count and the scheme is ignored; within one host the longest
    /// segment-wise path match wins, and the root path row is used when no sub-path matches.
    pub fn resolve(&self, authority: &str, path: &str) -> Lookup<'_> {
        let mut registered = false;
        let mut best: Option<&Domain> = None;
        for row in self
            .rows
            .iter()
            .filter(|row| row.enabled && row.authority() == authority)
        {
            registered = true;
            if row.covers(path) && best.is_none_or(|b| row.prefix().len() > b.prefix().len()) {
                best = Some(row);
            }
        }
        match best {
            Some(row) => Lookup::Row(row),
            None if registered => Lookup::Outside,
            None => Lookup::Unregistered,
        }
    }

    /// The home domain of a top-level forum: another domain that shows it, is enabled, and has canonical set to itself;
    /// otherwise the main domain (when the main domain also shows it). Saving guarantees at most one of the former.
    /// When the main domain row is missing (database changed directly), it is treated as the main domain showing all
    /// forums, returning `None` for the caller to treat as "the forum URL".
    pub fn home(&self, forum_url: &str, top_fid: i64) -> Home<'_> {
        let main = self.main(forum_url);
        if let Some(other) = self.rows.iter().find(|row| {
            row.enabled
                && row.canonical == Canonical::Own
                && Some(row.domain_id) != main.map(|m| m.domain_id)
                && row.shows(top_fid)
        }) {
            return Home::Other(other);
        }
        match main {
            Some(main) if main.shows(top_fid) => Home::Main,
            Some(_) => Home::Nowhere,
            None => Home::Main,
        }
    }

    /// Addresses of all enabled rows: the set that in-site thread cards and anti-spam recognize as "this site".
    pub fn enabled_urls(&self) -> impl Iterator<Item = &str> {
        self.rows
            .iter()
            .filter(|row| row.enabled)
            .map(|row| row.site_url.as_str())
    }
}

/// Where a forum's home is.
#[derive(Debug, PartialEq, Eq)]
pub enum Home<'a> {
    Main,
    Other(&'a Domain),
    /// No enabled domain shows it: unreachable from the front end.
    Nowhere,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn row(id: i64, url: &str, forums: &[i64]) -> Domain {
        Domain {
            domain_id: id,
            site_url: url.into(),
            site_name: (id != 1).then(|| format!("Site {id}")),
            canonical: Canonical::Own,
            logo_aid: None,
            favicon_aid: None,
            verification: None,
            login_methods: None,
            participation: None,
            language: None,
            enabled: true,
            version: 1,
            dateline: 0,
            forums: forums.iter().copied().collect(),
        }
    }

    #[test]
    fn authorities_compare_without_scheme_or_default_port() {
        assert_eq!(authority_of("https://demo.com"), "demo.com");
        assert_eq!(authority_of("https://demo.com/bbs/x"), "demo.com");
        // An explicit 80 or 443 is dropped whatever the scheme, as in requests.
        assert_eq!(authority_of("https://demo.com:80"), "demo.com");
        assert_eq!(authority_of("http://demo.com:443/bbs"), "demo.com");
        assert_eq!(authority_of("https://demo.com:8080"), "demo.com:8080");
        assert_eq!(
            authority_of("http://127.0.0.1:58844/bbs"),
            "127.0.0.1:58844"
        );
        assert_eq!(request_authority(" Demo.COM:443 "), "demo.com");
        assert_eq!(request_authority("demo.com:80"), "demo.com");
        assert_eq!(request_authority("demo.com."), "demo.com");
        assert_eq!(request_authority("127.0.0.1:58844"), "127.0.0.1:58844");
        assert_eq!(request_authority("[::1]:443"), "[::1]");
    }

    #[test]
    fn resolve_takes_the_longest_segment_match_then_the_root() {
        let map = DomainMap::new(vec![
            row(1, "https://demo.com", &[1, 2, 3]),
            row(2, "https://demo.com/feedback", &[2]),
            row(3, "https://demo.com/feedback/vip", &[3]),
            row(4, "https://demo2.com/help", &[2]),
        ]);
        let id = |lookup: Lookup<'_>| match lookup {
            Lookup::Row(row) => Some(row.domain_id),
            _ => None,
        };
        assert_eq!(id(map.resolve("demo.com", "/")), Some(1));
        assert_eq!(id(map.resolve("demo.com", "/feedbackx")), Some(1));
        assert_eq!(id(map.resolve("demo.com", "/feedback")), Some(2));
        assert_eq!(id(map.resolve("demo.com", "/feedback/thread-5")), Some(2));
        assert_eq!(id(map.resolve("demo.com", "/feedback/vip/")), Some(3));
        assert_eq!(id(map.resolve("demo2.com", "/help/")), Some(4));
        assert_eq!(map.resolve("demo2.com", "/"), Lookup::Outside);
        assert_eq!(map.resolve("demo2.com", "/helpx"), Lookup::Outside);
        assert_eq!(map.resolve("evil.com", "/"), Lookup::Unregistered);
        let mut stopped = map.clone();
        stopped.rows[3].enabled = false;
        assert_eq!(stopped.resolve("demo2.com", "/help/"), Lookup::Unregistered);
        // An address registered with an explicit 80 or 443 is found by the requests made to it, which arrive
        // without the port.
        let ported = DomainMap::new(vec![
            row(1, "https://demo.com", &[1]),
            row(2, "https://demo3.com:80", &[1]),
            row(3, "http://demo4.com:443/bbs", &[1]),
        ]);
        assert_eq!(
            id(ported.resolve(&request_authority("demo3.com:80"), "/")),
            Some(2)
        );
        assert_eq!(
            id(ported.resolve(&request_authority("demo4.com:443"), "/bbs/")),
            Some(3)
        );
    }

    #[test]
    fn home_prefers_a_self_canonical_other_domain_over_main() {
        let mut map = DomainMap::new(vec![
            row(1, "https://demo.com", &[1, 2, 3]),
            row(2, "https://demo2.com", &[2, 3]),
            row(3, "https://demo3.com", &[3]),
        ]);
        map.rows[2].canonical = Canonical::Main;
        let main = "https://demo.com";
        assert_eq!(map.home(main, 1), Home::Main);
        assert_eq!(map.home(main, 2), Home::Other(&map.rows[1]));
        assert_eq!(map.home(main, 3), Home::Other(&map.rows[1]));
        assert_eq!(map.home(main, 9), Home::Nowhere);
        // The main domain row is missing (database changed directly): every row counts as another domain, the one
        // whose canonical points to itself is the home, and forums nobody shows go to the main domain.
        assert_eq!(map.home("https://moved.com", 1), Home::Other(&map.rows[0]));
        assert_eq!(map.home("https://moved.com", 9), Home::Main);
        let mut stopped = map.clone();
        stopped.rows[1].enabled = false;
        assert_eq!(stopped.home(main, 2), Home::Main);
    }
}
