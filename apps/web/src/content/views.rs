//! Thread view counts: which requests count, who the visitor is, the 30-minute deduplication and the buffered write
//! of `gonof_forum_thread.views` (docs/queries.md, "Views"; docs/settings-and-caching.md, "View counting").
//!
//! - **What counts.** A `GET` of a thread page that answers 200 with the thread itself, opened directly or by an
//!   HTMX boosted navigation. Not `HEAD`, not other HTMX requests, not browser prefetches, not requests without a
//!   `User-Agent` or from one that names itself a crawler ([`counted_request`]). The router checks the status and
//!   records the view after the page has been built ([`Views::record`]).
//! - **Who.** A member by uid; anyone else by a digest of the client address, the `User-Agent` and a random key that
//!   changes every UTC day. The key lives in this process, or in Redis when Redis is on so that every process derives
//!   the same digest; it is never written to the database, and once it is gone a digest cannot be traced back to an
//!   address.
//! - **Once per 30 minutes.** With Redis on, `SET {namespace}:view:<tid>:<visitor> 1 NX EX 1800` decides across every
//!   process; with Redis off, each process keeps its own bounded table ([`Seen`]), so a site running several
//!   processes without Redis may count a visitor once per process.
//! - **Buffered.** A new view only adds one to an in-process buffer (tid → count). [`Views::run`] writes the buffer
//!   every [`FLUSH_PERIOD`] in one write transaction, one primary-key `UPDATE` per thread, and once more when the
//!   process shuts down; a crash loses at most one period. The statement leaves `cache_revision` alone, so lists and
//!   cached pages show the new number when their own cache expires: views are approximate.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::Result;
use axum::http::{HeaderMap, Method, header};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{cache::Cache, db::Database};

/// One visitor counts once per thread in this many seconds.
pub const WINDOW_SECONDS: i64 = 1800;
/// How often the buffered counts are written.
pub const FLUSH_PERIOD: Duration = Duration::from_secs(60);
/// Visitor and thread pairs a process remembers when Redis is off. A full table forgets its oldest pairs first, so a
/// flood of new visitors can at worst let an earlier visitor count again before the window ends.
const SEEN_CAPACITY: usize = 100_000;
/// The daily key stays in Redis this long after its day has ended, for processes whose clocks run a little behind.
const KEY_GRACE_SECONDS: i64 = 300;
/// Lowercase fragments of `User-Agent` values that name crawlers, link previewers and scripted clients. A plain
/// keyword check, not a list of known bots: a crawler that pretends to be a browser counts like one.
const CRAWLER_MARKERS: &[&str] = &[
    "bot",
    "crawl",
    "spider",
    "slurp",
    "headless",
    "curl",
    "wget",
    "python",
    "http-client",
    "httpclient",
    "libwww",
    "java/",
];
/// The single statement that writes `views`, a primary-key update (docs/queries.md, "Views"). It does not raise
/// `cache_revision`; a thread that no longer exists matches no row, which is not an error.
pub const ADD_VIEWS: &str = "UPDATE gonof_forum_thread SET views=views+? WHERE tid=?";

/// Whether a thread page request may count as a view, judged from its method and headers. The caller still requires
/// a 200 that showed the thread.
pub fn counted_request(method: &Method, headers: &HeaderMap) -> bool {
    if method != Method::GET {
        return false;
    }
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .trim()
    };
    // An HTMX request that is not a boosted navigation swaps part of a page the reader already has open.
    if value("hx-request").eq_ignore_ascii_case("true")
        && !value("hx-boosted").eq_ignore_ascii_case("true")
    {
        return false;
    }
    // Chromium sends `Sec-Purpose: prefetch` (`prefetch;prerender` for prerendering), older browsers `Purpose`.
    for name in ["sec-purpose", "purpose"] {
        if value(name).to_ascii_lowercase().contains("prefetch") {
            return false;
        }
    }
    let agent = headers
        .get(header::USER_AGENT)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_ascii_lowercase())
        .unwrap_or_default();
    !agent.trim().is_empty() && !CRAWLER_MARKERS.iter().any(|marker| agent.contains(marker))
}

/// Who opened the thread.
#[derive(Clone, Copy, Debug)]
pub enum Visitor<'a> {
    Member(i64),
    /// Not signed in: the client address as the router resolved it (trusted proxy included) and the `User-Agent`.
    Guest {
        ip: &'a str,
        user_agent: &'a str,
    },
}

/// The deduplication identity: a uid, or the first 16 bytes of the guest digest. Nothing in it names an address.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Who {
    Member(i64),
    Guest([u8; 16]),
}

impl Who {
    fn key_part(self) -> String {
        match self {
            Self::Member(uid) => format!("u{uid}"),
            Self::Guest(digest) => format!("g{}", hex(&digest)),
        }
    }
}

/// The view counter of one process: shared by the router, which records views, and the task that writes them.
#[derive(Clone)]
pub struct Views {
    inner: Arc<Inner>,
}

struct Inner {
    cache: Cache,
    /// tid → views not yet written.
    pending: Mutex<HashMap<u64, i64>>,
    /// Deduplication with Redis off.
    seen: Mutex<Seen>,
    /// The day (UTC days since the epoch) and its key, as hex text.
    key: Mutex<Option<(i64, String)>>,
}

impl Views {
    pub fn new(cache: Cache) -> Self {
        Self::with_capacity(cache, SEEN_CAPACITY)
    }

    fn with_capacity(cache: Cache, capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                cache,
                pending: Mutex::default(),
                seen: Mutex::new(Seen::new(capacity)),
                key: Mutex::default(),
            }),
        }
    }

    /// Counts a view of `tid` unless this visitor already counted one in the last [`WINDOW_SECONDS`]; returns whether
    /// it counted. The count waits in the buffer until the next [`Views::flush`]. With Redis on, a failed command is
    /// an error carrying [`crate::cache::Unavailable`], as everywhere else.
    pub async fn record(&self, tid: u64, visitor: Visitor<'_>, now: i64) -> Result<bool> {
        let who = match visitor {
            Visitor::Member(uid) => Who::Member(uid),
            Visitor::Guest { ip, user_agent } => {
                let key = self.daily_key(now).await?;
                let mut hasher = Sha256::new();
                for part in [key.as_bytes(), ip.as_bytes(), user_agent.as_bytes()] {
                    hasher.update(part);
                    hasher.update([0]);
                }
                let mut digest = [0; 16];
                digest.copy_from_slice(&hasher.finalize()[..16]);
                Who::Guest(digest)
            }
        };
        let cache = &self.inner.cache;
        let first = if cache.enabled() {
            let key = format!("{}:view:{tid}:{}", cache.namespace(), who.key_part());
            cache.claim(&key, WINDOW_SECONDS.unsigned_abs()).await?
        } else {
            lock(&self.inner.seen).first(tid, who, now)
        };
        if first {
            *lock(&self.inner.pending).entry(tid).or_default() += 1;
        }
        Ok(first)
    }

    /// The key that guest digests are made with on `now`'s UTC day. A new day gets a new random key; with Redis on, the
    /// first process to ask stores it (`SET … NX GET`) and the others take that one. The Redis copy expires shortly
    /// after its day; this process forgets its copy when the day changes.
    async fn daily_key(&self, now: i64) -> Result<String> {
        let day = now.div_euclid(86_400);
        let cache = &self.inner.cache;
        {
            let mut slot = lock(&self.inner.key);
            if let Some((current, key)) = slot.as_ref()
                && *current == day
            {
                return Ok(key.clone());
            }
            if !cache.enabled() {
                let key = hex(&rand::random::<[u8; 32]>());
                *slot = Some((day, key.clone()));
                return Ok(key);
            }
        }
        let fresh = hex(&rand::random::<[u8; 32]>());
        let lifetime = (day + 1) * 86_400 - now + KEY_GRACE_SECONDS;
        let key = cache
            .claim_or_get(
                &format!("{}:view-key:{day}", cache.namespace()),
                &fresh,
                lifetime.unsigned_abs(),
            )
            .await?
            .unwrap_or(fresh);
        *lock(&self.inner.key) = Some((day, key.clone()));
        Ok(key)
    }

    /// Views counted and not written yet, for tests and logs.
    pub fn pending(&self) -> i64 {
        lock(&self.inner.pending).values().sum()
    }

    /// Writes the buffer in one write transaction and returns how many threads it touched. Threads are updated in
    /// `tid` order, as in every process, so two flushes queue instead of deadlocking. When the transaction fails, its
    /// counts go back into the buffer for the next flush.
    pub async fn flush(&self, db: &Database) -> Result<usize> {
        let mut batch: Vec<(u64, i64)> = std::mem::take(&mut *lock(&self.inner.pending))
            .into_iter()
            .collect();
        if batch.is_empty() {
            return Ok(0);
        }
        batch.sort_unstable();
        if let Err(error) = write(db, &batch).await {
            let mut pending = lock(&self.inner.pending);
            for (tid, count) in batch {
                *pending.entry(tid).or_default() += count;
            }
            return Err(error);
        }
        Ok(batch.len())
    }

    /// The writing task: flushes every `period` until `shutdown`, then once more, so a normal shutdown loses nothing.
    /// It only needs the database, so it keeps running while Redis is unreachable (no views are recorded then). Start
    /// it before serving and cancel it after the server has stopped taking requests.
    pub async fn run(self, db: Database, period: Duration, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }
            if let Err(error) = self.flush(&db).await {
                tracing::warn!(%error, pending = self.pending(), "thread views not written; will retry");
            }
        }
        if let Err(error) = self.flush(&db).await {
            tracing::warn!(%error, lost = self.pending(), "thread views not written before shutdown");
        }
    }
}

async fn write(db: &Database, batch: &[(u64, i64)]) -> Result<()> {
    let mut tx = db.begin_write().await?;
    for (tid, count) in batch {
        crate::db::query(ADD_VIEWS)
            .bind(*count)
            .bind(i64::try_from(*tid)?)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Visitor and thread pairs seen in the last window, for a process without Redis. Bounded: when full, the pairs that
/// were added first are forgotten first.
struct Seen {
    capacity: usize,
    until: HashMap<(u64, Who), i64>,
    /// Pairs in the order they were added, with the expiry they were added with; an entry whose pair has since been
    /// added again with a later expiry is skipped when it comes up.
    order: VecDeque<(i64, u64, Who)>,
}

impl Seen {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            until: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Whether `who` opens `tid` for the first time in the window; if so, remembers it.
    fn first(&mut self, tid: u64, who: Who, now: i64) -> bool {
        while self.order.front().is_some_and(|(until, ..)| *until <= now) {
            self.forget_oldest();
        }
        if self
            .until
            .get(&(tid, who))
            .is_some_and(|until| *until > now)
        {
            return false;
        }
        while self.order.len() >= self.capacity {
            self.forget_oldest();
        }
        let until = now + WINDOW_SECONDS;
        self.until.insert((tid, who), until);
        self.order.push_back((until, tid, who));
        true
    }

    fn forget_oldest(&mut self) {
        if let Some((until, tid, who)) = self.order.pop_front()
            && self.until.get(&(tid, who)) == Some(&until)
        {
            self.until.remove(&(tid, who));
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, Method};

    use super::{Seen, Views, WINDOW_SECONDS, Who, counted_request};
    use crate::{cache::Cache, config::RedisConfig};

    const BROWSER: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        headers
    }

    fn redis_off() -> Cache {
        Cache::new(&RedisConfig {
            enabled: false,
            ..RedisConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn only_full_page_gets_from_browsers_count() {
        assert!(counted_request(
            &Method::GET,
            &headers(&[("user-agent", BROWSER)])
        ));
        assert!(counted_request(
            &Method::GET,
            &headers(&[
                ("user-agent", BROWSER),
                ("hx-request", "true"),
                ("hx-boosted", "true")
            ])
        ));
        assert!(counted_request(
            &Method::GET,
            &headers(&[("user-agent", BROWSER), ("hx-request", "false")])
        ));
        assert!(!counted_request(
            &Method::HEAD,
            &headers(&[("user-agent", BROWSER)])
        ));
        assert!(!counted_request(
            &Method::POST,
            &headers(&[("user-agent", BROWSER)])
        ));
        assert!(!counted_request(
            &Method::GET,
            &headers(&[("user-agent", BROWSER), ("hx-request", "true")])
        ));
        for (name, value) in [
            ("sec-purpose", "prefetch"),
            ("sec-purpose", "prefetch;prerender"),
            ("purpose", "prefetch"),
        ] {
            assert!(
                !counted_request(
                    &Method::GET,
                    &headers(&[("user-agent", BROWSER), (name, value)])
                ),
                "{name}: {value}"
            );
        }
        assert!(!counted_request(&Method::GET, &headers(&[])));
        assert!(!counted_request(
            &Method::GET,
            &headers(&[("user-agent", "  ")])
        ));
        for agent in [
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Mozilla/5.0 (compatible; Baiduspider/2.0)",
            "Sogou web spider/4.0",
            "Mozilla/5.0 (compatible; Yahoo! Slurp)",
            "SomeCrawler/1.0",
            "curl/8.7.1",
            "python-requests/2.32",
            "Go-http-client/1.1",
            "Mozilla/5.0 HeadlessChrome/131.0.0.0",
        ] {
            assert!(
                !counted_request(&Method::GET, &headers(&[("user-agent", agent)])),
                "{agent}"
            );
        }
    }

    #[test]
    fn the_local_table_counts_once_per_window_and_forgets_its_oldest_pairs_when_full() {
        let mut seen = Seen::new(3);
        let now = 1_000_000;
        assert!(seen.first(1, Who::Member(7), now));
        assert!(!seen.first(1, Who::Member(7), now + WINDOW_SECONDS - 1));
        assert!(seen.first(2, Who::Member(7), now));
        assert!(seen.first(1, Who::Member(8), now));
        // The window has passed: the pair counts again, and its old entry no longer forgets the new one.
        assert!(seen.first(1, Who::Member(7), now + WINDOW_SECONDS));
        assert!(!seen.first(1, Who::Member(7), now + WINDOW_SECONDS + 10));
        // Full (three pairs): a new pair forgets the oldest live one, which may then count again.
        let later = now + WINDOW_SECONDS + 20;
        assert!(seen.first(3, Who::Guest([1; 16]), later));
        assert!(seen.order.len() <= 3 && seen.until.len() <= 3);
        assert!(!seen.first(3, Who::Guest([1; 16]), later + 1));
    }

    #[tokio::test]
    async fn guests_differ_by_address_and_browser_and_members_by_uid() {
        let views = Views::new(redis_off());
        let now = 1_000_000;
        let guest = |ip, user_agent| super::Visitor::Guest { ip, user_agent };
        assert!(
            views
                .record(5, guest("192.0.2.1", BROWSER), now)
                .await
                .unwrap()
        );
        assert!(
            !views
                .record(5, guest("192.0.2.1", BROWSER), now + 60)
                .await
                .unwrap()
        );
        assert!(
            views
                .record(5, guest("192.0.2.2", BROWSER), now)
                .await
                .unwrap()
        );
        assert!(
            views
                .record(5, guest("192.0.2.1", "Firefox"), now)
                .await
                .unwrap()
        );
        assert!(
            views
                .record(5, super::Visitor::Member(3), now)
                .await
                .unwrap()
        );
        assert!(
            !views
                .record(5, super::Visitor::Member(3), now + 1)
                .await
                .unwrap()
        );
        assert!(
            views
                .record(6, super::Visitor::Member(3), now)
                .await
                .unwrap()
        );
        assert_eq!(views.pending(), 5);
        // A new day has a new key, so the same guest is a new visitor (docs/settings-and-caching.md, "View counting").
        assert!(
            views
                .record(5, guest("192.0.2.1", BROWSER), now + 86_400)
                .await
                .unwrap()
        );
    }

    #[test]
    fn the_capacity_is_never_below_one() {
        let views = Views::with_capacity(redis_off(), 0);
        assert_eq!(super::lock(&views.inner.seen).capacity, 1);
    }
}
