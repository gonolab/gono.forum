//! Anti-spam rules: minimum interval between threads and replies, hourly posting limit, duplicate content blocking,
//! and external link blocking for new members; external links also have a level threshold
//! `settings.growth.link_level` (both it and the observation period must be satisfied). The link rules cover
//! everything a post shows that can carry an address ([`Linked`]): the body, the thread title and a news thread's
//! source link; member bios use the same check ([`link_refusal`]).
//!
//! All are checked inside the posting transaction, after the write gate has read the settings: the author's member row
//! is already `FOR UPDATE`, so posts by the same member are serialized here and concurrent submissions cannot slip
//! through together; retries with the same request key were already replayed with the original result before this. The
//! interval and hourly limit are counted from the database rather than by rate limiting: correct across multiple
//! processes as well, independent of Redis, and only successfully published posts count. No SQL is sent when a rule is
//! off or set to 0. People who manage the target forum are exempt from these rules ([`manages`]).
use crate::db::Tx;
use std::collections::BTreeSet;

use super::writes::ContentWriteError;
use crate::{
    db::{self, Database},
    runtime::SiteSettings,
};

type Result<T> = std::result::Result<T, ContentWriteError>;

/// How many of the member's own posts of the same kind the duplicate check looks back at (newest first).
pub const DUPLICATE_SCAN_LIMIT: i64 = 20;
/// Window of the hourly posting limit (only the count is configurable, the window is a constant).
const HOUR: i64 = 3600;
/// Placeholder host for relative URLs when the site URL is unknown (command line, tests without a request).
const PLACEHOLDER_SITE: &str = "http://site.invalid/";

/// The poster: registration time and role (`adminid`) read from the member row locked in the transaction, plus the
/// target forum and its parent forum.
pub(crate) struct Poster {
    pub uid: i64,
    pub regdate: i64,
    pub role: i64,
    pub fid: i64,
    pub parent: Option<i64>,
}

/// The text a write shows readers that can carry addresses of other sites. Fields a write does not have stay empty.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Linked<'a> {
    /// A post body (Markdown) or a member bio (plain text).
    pub body: &'a str,
    /// A thread title (plain text): lists, feeds, page titles and the sitemap show it.
    pub title: &'a str,
    /// A news thread's source link, shown as a link on its activity card.
    pub source_url: &'a str,
}

impl<'a> Linked<'a> {
    /// A body alone: a reply or a bio.
    pub(crate) fn body(body: &'a str) -> Self {
        Self {
            body,
            ..Self::default()
        }
    }

    /// External URLs in all fields (full URLs after normalization, see [`OwnSite`]).
    fn external(self, site_url: Option<&str>) -> BTreeSet<String> {
        let site = OwnSite::new(site_url);
        let mut found = site.scan(self.body);
        // A title is plain text, but it is scanned like a body: a body counts plain-text addresses as well.
        found.extend(site.scan(self.title));
        let source = self.source_url.trim();
        if !source.is_empty()
            && let Some(url) = site.join(source)
        {
            found.extend(site.external(&url));
        }
        found
    }
}

/// The four rules when posting a thread (`first`), a reply or a nested reply. `post.body` is also what the duplicate
/// check compares; the link rule looks at every field of `post`. `timestamp` is when this post is stored.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn check_post(
    db: &Database,
    tx: &mut Tx<'_>,
    settings: &SiteSettings,
    poster: &Poster,
    first: bool,
    post: Linked<'_>,
    site_url: Option<&str>,
    timestamp: i64,
) -> Result<()> {
    let rules = &settings.posting;
    let interval = if first {
        rules.thread_interval_seconds
    } else {
        rules.reply_interval_seconds
    };
    let links = link_gates_apply(settings, poster.regdate, timestamp);
    if interval == 0 && rules.hourly_limit == 0 && !rules.duplicate_check && !links {
        return Ok(());
    }
    if manages(db, tx, poster.uid, poster.role, poster.fid, poster.parent).await? {
        return Ok(());
    }
    if links {
        let author = (poster.uid, poster.regdate);
        check_links(db, tx, settings, author, timestamp, post, None, site_url).await?;
    }
    if interval > 0 {
        let sql = "SELECT dateline FROM gonof_forum_post WHERE authorid = ? AND first = ? ORDER BY dateline DESC LIMIT 1";
        let last: Option<i64> = db::query_scalar(sql)
            .bind(poster.uid)
            .bind(i64::from(first))
            .fetch_optional(db.observe(&mut **tx))
            .await?;
        if let Some(last) = last.filter(|last| timestamp.saturating_sub(*last) < interval) {
            return Err(ContentWriteError::Quota(
                if first {
                    crate::i18n::current().editor_error_thread_interval()
                } else {
                    crate::i18n::current().editor_error_reply_interval()
                },
                last.saturating_add(interval).saturating_sub(timestamp),
            ));
        }
    }
    if rules.hourly_limit > 0 {
        // `first IN (0, 1)` makes threads and replies each use one index range, reading only rows from the last
        // hour; with only `authorid = ? AND dateline > ?`, 5.7 would scan all of this author's posts.
        let sql = "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid = ? AND first IN (0, 1) AND dateline > ?";
        let total: i64 = db::query_scalar(sql)
            .bind(poster.uid)
            .bind(timestamp.saturating_sub(HOUR))
            .fetch_one(db.observe(&mut **tx))
            .await?;
        if total >= rules.hourly_limit {
            // Another post fits once enough of the last hour's posts leave the window: the one at position
            // `total - limit` from the oldest is the last of them to go.
            let sql = "SELECT dateline FROM gonof_forum_post WHERE authorid = ? AND first IN (0, 1) AND dateline > ? ORDER BY dateline LIMIT 1 OFFSET ?";
            let leaving: Option<i64> = db::query_scalar(sql)
                .bind(poster.uid)
                .bind(timestamp.saturating_sub(HOUR))
                .bind(total.saturating_sub(rules.hourly_limit))
                .fetch_optional(db.observe(&mut **tx))
                .await?;
            return Err(ContentWriteError::Quota(
                crate::i18n::current().editor_error_hourly(),
                leaving.map_or(HOUR, |dateline| {
                    dateline.saturating_add(HOUR).saturating_sub(timestamp)
                }),
            ));
        }
    }
    if rules.duplicate_check {
        // Only compare posts of the same kind (threads with threads, replies and nested replies with replies),
        // taking a fixed number newest first via the index.
        let sql = "SELECT message, deleted_at FROM gonof_forum_post WHERE authorid = ? AND first = ? AND dateline >= ? ORDER BY dateline DESC LIMIT ?";
        let rows = db::query(sql)
            .bind(poster.uid)
            .bind(i64::from(first))
            .bind(timestamp.saturating_sub(rules.duplicate_window_minutes.saturating_mul(60)))
            .bind(DUPLICATE_SCAN_LIMIT)
            .fetch_all(db.observe(&mut **tx))
            .await?;
        let body = post.body.trim();
        for row in rows {
            // Posts the member withdrew do not count: withdrawing and posting elsewhere is normal use. Posts hidden
            // by moderation still count.
            if row.try_get::<i64, _>("deleted_at")? == 0
                && db::text(&row, "message")?.trim() == body
            {
                return Err(ContentWriteError::Validation(
                    crate::i18n::current().editor_error_duplicate(),
                ));
            }
        }
    }
    Ok(())
}

/// People who manage this forum are exempt from the anti-spam rules: administrators and
/// global moderators site-wide; moderators and trainee moderators in the forums they are appointed to and their child
/// forums, inherited by the rule "parent forum moderators automatically manage child forums", the same rule as
/// the moderator checks for moderation, moving, reports and so on ([`crate::organization::moderates_sql`]).
/// Read without locking: an exemption is not an authorization, so when an appointment change races with posting,
/// either side committing first is fine.
pub(crate) async fn manages(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    role: i64,
    fid: i64,
    parent: Option<i64>,
) -> Result<bool> {
    match role {
        1 | 2 => Ok(true),
        // Trainee moderators count as moderators in the forums they are appointed to; reviewers do not manage forums
        // and are not exempt.
        3 | 4 => {
            let sql =
                "SELECT fid FROM gonof_forum_moderator WHERE uid = ? AND fid IN (?, ?) LIMIT 1";
            let found: Option<i64> = db::query_scalar(sql)
                .bind(uid)
                .bind(fid)
                .bind(parent.unwrap_or(fid))
                .fetch_optional(db.observe(&mut **tx))
                .await?;
            Ok(found.is_some())
        }
        _ => Ok(false),
    }
}

/// Whether the external link rule applies to this member: it is on, and the member registered less than the
/// observation period ago.
pub(crate) fn link_rule_applies(settings: &SiteSettings, regdate: i64, timestamp: i64) -> bool {
    let rules = &settings.posting;
    rules.new_member_link_block
        && timestamp.saturating_sub(regdate) < rules.new_member_link_days.saturating_mul(86_400)
}

/// Whether either of the two external link conditions might block the member: within the observation period, or a
/// level threshold is set (whether it is reached needs growth points, read only once there really is an external
/// link).
pub(crate) fn link_gates_apply(settings: &SiteSettings, regdate: i64, timestamp: i64) -> bool {
    link_rule_applies(settings, regdate, timestamp) || settings.growth.link_level > 1
}

/// Posting external links requires having registered at least `settings.posting.new_member_link_days` days ago (when
/// the observation period is on) and reaching level `settings.growth.link_level`. When editing, the original content is
/// passed as `previous` and only newly added external links are blocked: fixing a typo is not rejected because of
/// links already there. `author` is (uid, registration time); growth points are read only when the content really has
/// a new external link and the level threshold is greater than 1.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn check_links(
    db: &Database,
    tx: &mut Tx<'_>,
    settings: &SiteSettings,
    author: (i64, i64),
    timestamp: i64,
    now: Linked<'_>,
    previous: Option<Linked<'_>>,
    site_url: Option<&str>,
) -> Result<()> {
    match link_refusal(db, tx, settings, author, timestamp, now, previous, site_url).await? {
        Some(message) => Err(ContentWriteError::Restricted(message)),
        None => Ok(()),
    }
}

/// The decision behind [`check_links`], for callers with their own error type (member bios): the message to refuse
/// with, or `None` when the external links may be posted. Exemptions are the caller's to decide.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn link_refusal(
    db: &Database,
    tx: &mut Tx<'_>,
    settings: &SiteSettings,
    (uid, regdate): (i64, i64),
    timestamp: i64,
    now: Linked<'_>,
    previous: Option<Linked<'_>>,
    site_url: Option<&str>,
) -> std::result::Result<Option<String>, sqlx::Error> {
    let days = settings.posting.new_member_link_days;
    let observation = link_rule_applies(settings, regdate, timestamp);
    if !observation && settings.growth.link_level <= 1 {
        return Ok(None);
    }
    let site = crate::runtime::site_origin(settings, site_url).ok();
    let found = now.external(site.as_deref());
    let added = match previous {
        Some(previous) => {
            let before = previous.external(site.as_deref());
            found.difference(&before).next().is_some()
        }
        None => !found.is_empty(),
    };
    if !added {
        return Ok(None);
    }
    let level =
        crate::growth::level_gate(db, tx, &settings.growth, uid, settings.growth.link_level)
            .await?;
    // Days and level are computed from the current settings, so the message stays right
    // after a settings change.
    Ok(match (observation, level) {
        (false, None) => None,
        (true, None) => Some(crate::i18n::current().editor_error_links_days(days)),
        (false, Some(level)) => Some(crate::i18n::current().editor_error_links_level(level)),
        (true, Some(level)) => {
            Some(crate::i18n::current().editor_error_links_days_level(days, level))
        }
    })
}

/// This site's address and host names, against which URLs are judged. URLs whose host is this site (ignoring case, a
/// trailing dot and a leading `www.`, including subdomains of this site) and relative on-site URLs do not count as
/// external. When the site URL is unknown, only relative URLs count as on-site.
struct OwnSite {
    base: url::Url,
    hosts: Vec<String>,
}

impl OwnSite {
    fn new(site_url: Option<&str>) -> Self {
        let base = site_url
            .and_then(|site| url::Url::parse(site).ok())
            .filter(|site| site.host_str().is_some())
            .unwrap_or_else(|| {
                url::Url::parse(PLACEHOLDER_SITE).expect("placeholder URL is valid")
            });
        // With multi-domain sites on, all enabled registered domains and forum URLs are also this site.
        let hosts = base
            .host_str()
            .map(normalized_host)
            .into_iter()
            .chain(crate::site::sibling_urls().iter().filter_map(|site| {
                url::Url::parse(site)
                    .ok()
                    .and_then(|url| url.host_str().map(normalized_host))
            }))
            .collect();
        Self { base, hosts }
    }

    /// Relative URLs resolve against this site; ones without a host such as `mailto:` are not website links.
    fn join(&self, href: &str) -> Option<url::Url> {
        self.base.join(href.trim()).ok()
    }

    /// The full URL when it points to another site.
    fn external(&self, url: &url::Url) -> Option<String> {
        let host = normalized_host(url.host_str()?);
        (!self.hosts.iter().any(|own| same_site(&host, own))).then(|| url.to_string())
    }

    /// External URLs in a body (full URLs after normalization).
    ///
    /// Every URL that appears in the body counts: links that can be clicked after rendering
    /// (Markdown links, autolinks, hand-written `<a>`, including the `//host` form), plus `http://` and `https://`
    /// URLs anywhere in the source (in plain text and in code too).
    fn scan(&self, body: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for href in crate::markdown::link_targets(body) {
            found.extend(self.join(&href).and_then(|url| self.external(&url)));
        }
        for authority in text_authorities(body) {
            if let Ok(url) = url::Url::parse(&format!("http://{authority}/")) {
                found.extend(self.external(&url));
            }
        }
        found
    }
}

/// The host part of URLs after `http://` and `https://` (case-insensitive) in the source (including user name and
/// port, which the URL parser splits off).
fn text_authorities(body: &str) -> Vec<&str> {
    // Only the case of ASCII letters changes, so byte positions match the source one to one.
    let lower = body.to_ascii_lowercase();
    let mut found = Vec::new();
    for (index, _) in lower.match_indices("http") {
        let rest = &lower[index + 4..];
        let skip = if rest.starts_with("s://") {
            8
        } else if rest.starts_with("://") {
            7
        } else {
            continue;
        };
        let start = index + skip;
        let tail = &body[start..];
        let end = tail
            .find(|c: char| {
                !(c.is_alphanumeric()
                    || matches!(c, '-' | '.' | '_' | '~' | '%' | ':' | '@' | '[' | ']'))
            })
            .unwrap_or(tail.len());
        if end > 0 {
            found.push(&tail[..end]);
        }
    }
    found
}

fn normalized_host(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match host.strip_prefix("www.") {
        Some(rest) => rest.to_owned(),
        None => host,
    }
}

/// The same host or a subdomain of this site.
fn same_site(host: &str, own: &str) -> bool {
    !own.is_empty() && (host == own || host.ends_with(&format!(".{own}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Linked, text_authorities};

    fn external_links(body: &str, site_url: Option<&str>) -> BTreeSet<String> {
        Linked::body(body).external(site_url)
    }

    fn links(body: &str) -> Vec<String> {
        external_links(body, Some("https://example.com"))
            .into_iter()
            .collect()
    }

    #[test]
    fn site_links_and_relative_paths_are_not_external() {
        // Chinese punctuation may follow a link with no space: "\u{3001}" is the ideographic comma and "\u{3002}" the
        // ideographic full stop; neither belongs to the host.
        for body in [
            "See [the rules](/rules) and [a thread](/thread-3) and [a floor](#p5)",
            "https://example.com/thread-1 and HTTPS://WWW.EXAMPLE.COM./forum-2\u{3001}https://m.example.com",
            "<https://example.com/space/4> <a href=\"/x\">hand-written</a> http://example.com:58833/x",
            "[@name](member:4) [quote](post:5) ![picture](attachment:7) [mail](mailto:a@b.example)",
            "Visit https://example.com, then visit https://example.com\u{3002}",
        ] {
            assert!(
                links(body).is_empty(),
                "{body} must not count as an external link: {:?}",
                links(body)
            );
        }
    }

    #[test]
    fn every_spelling_of_a_foreign_site_counts() {
        for (body, host) in [
            ("[ad](https://spam.example/buy)", "spam.example"),
            ("<https://spam.example>", "spam.example"),
            ("<a href=\"//spam.example/x\">click me</a>", "spam.example"),
            ("[x](//spam.example)", "spam.example"),
            (
                "Plain text http://spam.example/a counts without being a link",
                "spam.example",
            ),
            ("`curl https://spam.example/x`", "spam.example"),
            ("```\nwget HTTPS://SPAM.EXAMPLE/x\n```", "spam.example"),
            ("**https://spam.example**", "spam.example"),
            (
                "https://example.com@spam.example/posing-as-this-site",
                "spam.example",
            ),
            (
                "https://example.com.spam.example/",
                "example.com.spam.example",
            ),
            (
                "<spoiler>[hidden](https://spam.example)</spoiler>",
                "spam.example",
            ),
            ("<ftp://spam.example/file>", "spam.example"),
        ] {
            let found = links(body);
            assert!(
                found.iter().any(|url| url.contains(host)),
                "{body} must find {host}: {found:?}"
            );
        }
    }

    #[test]
    fn titles_and_source_links_count_like_bodies() {
        let site = Some("https://example.com");
        let found = Linked {
            body: "no links here",
            title: "Deals at http://spam.example today",
            source_url: " https://wire.example/story ",
        }
        .external(site);
        assert!(found.contains("https://wire.example/story"), "{found:?}");
        assert!(
            found.iter().any(|url| url.contains("spam.example")),
            "{found:?}"
        );
        let own = Linked {
            body: "",
            title: "See https://WWW.example.com/rules",
            source_url: "https://m.example.com/thread-1",
        };
        assert!(own.external(site).is_empty());
        assert!(Linked::default().external(site).is_empty());
    }

    #[test]
    fn unknown_site_only_trusts_relative_paths() {
        assert!(external_links("[in site](/rules)", None).is_empty());
        assert_eq!(external_links("https://example.com/x", None).len(), 1);
    }

    #[test]
    fn text_scan_stops_at_the_host() {
        assert_eq!(
            text_authorities("See https://a.example/p?q=1, and HTTP://B.example:8080)"),
            ["a.example", "B.example:8080"]
        );
        assert!(text_authorities("https:// and httpx://a and http:/a").is_empty());
    }
}
