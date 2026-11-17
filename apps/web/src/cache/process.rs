//! Process cache (the system-cache layer): site-wide data that rarely
//! changes and is needed on every page, kept once in each process's memory.
//! Present both with Redis disabled and enabled. Every block and statistics slot has an entry in the registry
//! ([`super::registry`]).
//!
//! How it works (docs/settings-and-caching.md, "The process cache" and "Invalidation"):
//!
//! - Organized in **blocks**; each block is one piece of decoded data (`Arc<T>`) plus its load time and generation
//!   number. The blocks are site settings and navigation, policy documents (one slot per kind), brand images, credit
//!   reward rules (the check-in switch and amount on member cards), friend links (two slots: home page and directory
//!   page), the cross-forum pin index, the moderator roster, the honor group directory, the readable forums (two
//!   slots: anonymous and member) and the domain table.
//! - Statistics (site statistics, newest member for anonymous visitors, forum statistics) also live here, but are
//!   **not invalidated by broadcasts**: they are bucketed by their own statistics duration, the bucket number is
//!   stored in the slot, only the current bucket is kept, and a new bucket means recomputing.
//!   A process miss reads Redis next (when enabled), and only a further miss computes.
//! - All blocks share the TTL `settings.cache.ttl_seconds`; **0 means no process cache**, and every request reads the
//!   database as usual (set it to 0 when profiling SQL). The TTL itself lives in the settings block, so after it is
//!   changed the old TTL applies at most once more.
//! - **After the write transaction commits**, the save function uses the handle to synchronously clear the affected
//!   blocks in this process first (generation number plus one, current value dropped), so the next request in the
//!   same process is certain to see the new value; with Redis enabled it then runs
//!   `PUBLISH {namespace}:global-cache <block name>`, and other processes' standing subscriptions clear on receipt. A
//!   failed publish returns [`Unpublished`] (503 response stating that the data was saved).
//! - The generation is recorded before loading from the source and compared again before storing; on a mismatch the
//!   value is discarded, so a stale value read before an invalidation is never written back after it; only one request
//!   at a time loads a given block from the source; a failed load is not cached (a bad settings row errors every time,
//!   and the next request recovers once it is fixed).
//! - With Redis enabled the subscription connection is a **startup prerequisite** ([`GlobalCache::start`]); if it
//!   cannot be established, startup is refused; a disconnect at runtime is reconnected in the background, and a
//!   successful reconnect clears all blocks in this process (notifications during the outage are lost). Redis disabled
//!   means a single process: only this process is cleared, with no subscription and no publishing.
//! - The subscription connection has a **heartbeat**: every [`HEARTBEAT`] it publishes a message
//!   on this process's own heartbeat channel; if it has not come back by the next beat, reached no subscriber or could
//!   not be sent, that counts as a disconnect. When the network is half broken the connection looks alive but receives
//!   no broadcasts, and without a heartbeat it would heal only after a TTL; with the heartbeat it reconnects and clears
//!   within at most two beats (60 seconds).
//! - "Refresh caches" in the admin area ([`GlobalCache::refresh_all`]) clears all blocks in this process and
//!   broadcasts `*`; other processes clear everything on receipt as well.
//!
//! The handle lives on `AppState` and is passed explicitly into each save function, not a global `static`: each
//! test fixture's application has its own, so they never leak into each other; tests that call save
//! functions directly pass the same application's handle.
//!
//! **Not cached**: the write gate `GATE_SQL` (lock semantics), authentication, locking reads and permission rechecks
//! inside transactions, and the current values read by admin edit pages. The cache serves "display" only.
use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    activities::Rule,
    cache::{Cache, registry},
    content::{ForumStatistics, Statistics, home::RecentMember, pins::Pin},
    db::Database,
    debug::trace,
    identity::IdentityError,
    links::{HomeLink, LinkCategory, Listed},
    media::{BrandAsset, Media},
    runtime::{Document, NavigationItem, SiteSettings},
};

/// Community statistics by scope: the key is the forum IDs in the scope, the value is `(bucket, statistics)`.
type ScopedStatistics = std::collections::BTreeMap<Vec<i64>, ((i64, i64), Statistics)>;

/// Maximum number of slots kept for scoped community statistics (one per scope; the domain table has at most 50 rows,
/// with one copy each for the anonymous and member views).
const SCOPED_STATISTICS: usize = 128;

/// Suffix of the invalidation channel name: the full channel is `{redis.namespace}:global-cache`.
pub const CHANNEL: &str = "global-cache";

/// The name that means "all blocks" in a broadcast (admin "Refresh caches"). Processes on an older version do not
/// recognize it and handle it by the "clear everything if unrecognized" rule.
const EVERYTHING: &str = "*";

/// Heartbeat interval of the subscription connection.
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// The saved languages of one policy.
#[derive(Debug, Default)]
pub struct PolicyCopies(std::collections::BTreeMap<crate::i18n::Lang, Arc<Document>>);

/// Policy document kinds, one slot each (the navigation document is in the same block as the site settings).
pub const POLICY_KINDS: [&str; 6] = [
    "policy.terms",
    "policy.privacy",
    "policy.rules",
    "policy.contact",
    "policy.verified",
    "policy.links",
];

/// Blocks that can be invalidated. Broadcasts carry [`Block::name`]; several blocks invalidated together are joined
/// with commas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// Scalar settings and the navigation document.
    Settings,
    /// One policy document; see [`POLICY_KINDS`] for the values.
    Policy(&'static str),
    Branding,
    /// All rows of `gonof_common_credit_rule`.
    CreditRules,
    /// The two friend-link slots (home page and directory page), cleared together by the same write points.
    Links,
    /// Index of site-wide level-2 and level-3 pins (`tid, fid, displayorder`); see [`crate::content::pins`].
    Pins,
    /// Moderator roster (for display and read-only moderator checks, docs/queries.md, "Process caches"); see
    /// [`crate::organization::RosterEntry`].
    Moderators,
    /// Honor group directory (name, description, order and enabled state, docs/queries.md, "Process caches"); floor
    /// badges use it to match grant records to groups.
    Honors,
    /// Readable forums and parent/child relations: two slots, anonymous and member
    /// (docs/settings-and-caching.md, "The process cache"), cleared together by the same write points.
    Forums,
    /// The multi-domain sites' domain table and the forums each domain shows.
    Domains,
    /// Every skin of the three sources (docs/skins.md, "Sources"); clearing it also rescans the instance directory.
    Skins,
}

impl Block {
    /// The registry entry. A new block must be registered first.
    pub fn entry(self) -> &'static registry::Entry {
        match self {
            Self::Settings => &registry::SETTINGS,
            Self::Policy(_) => &registry::POLICY,
            Self::Branding => &registry::BRANDING,
            Self::CreditRules => &registry::CREDIT_RULES,
            Self::Links => &registry::LINKS,
            Self::Pins => &registry::PINS,
            Self::Moderators => &registry::MODERATORS,
            Self::Honors => &registry::HONORS,
            Self::Forums => &registry::FORUMS,
            Self::Domains => &registry::DOMAINS,
            Self::Skins => &registry::SKINS,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Policy(kind) => kind,
            Self::Branding => "branding",
            Self::CreditRules => "credit-rules",
            Self::Links => "links",
            Self::Pins => "pins",
            Self::Moderators => "moderators",
            Self::Honors => "honors",
            Self::Forums => "forums",
            Self::Domains => "domains",
            Self::Skins => "skins",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "settings" => Some(Self::Settings),
            "branding" => Some(Self::Branding),
            "credit-rules" => Some(Self::CreditRules),
            "links" => Some(Self::Links),
            "pins" => Some(Self::Pins),
            "moderators" => Some(Self::Moderators),
            "honors" => Some(Self::Honors),
            "forums" => Some(Self::Forums),
            "domains" => Some(Self::Domains),
            "skins" => Some(Self::Skins),
            other => POLICY_KINDS
                .iter()
                .find(|kind| **kind == other)
                .map(|kind| Self::Policy(kind)),
        }
    }

    /// The policy document kind; `None` when it is not a policy document or the kind is unknown.
    pub fn policy(kind: &str) -> Option<Self> {
        POLICY_KINDS
            .iter()
            .find(|candidate| **candidate == kind)
            .map(|kind| Self::Policy(kind))
    }
}

/// The write has committed, but other processes could not be notified. The response layer returns 503 for it and
/// states that the data was saved.
#[derive(Debug)]
pub struct Unpublished {
    /// When saving settings, the old TTL read under the lock; for other writes, the TTL this process last read, empty
    /// when unknown.
    pub ttl_seconds: Option<u64>,
    /// The raw Redis error (redacted), to help administrators tell which link broke.
    pub detail: String,
}

impl std::fmt::Display for Unpublished {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let t = crate::i18n::current();
        formatter.write_str(&t.common_cache_unpublished(self.detail.as_str()))?;
        if let Some(ttl) = self.ttl_seconds {
            formatter.write_str(&t.common_cache_unpublished_ttl(ttl))?;
        }
        formatter
            .write_str(&t.common_cache_unpublished_limit(crate::runtime::MAX_CACHE_TTL_SECONDS))
    }
}

impl std::error::Error for Unpublished {}

/// The site settings block: the result of the page frame's single SQL query, i.e. the decoded scalar settings and
/// navigation document, plus the parsed navigation entries.
#[derive(Debug)]
pub struct SettingsBlock {
    pub settings: Arc<SiteSettings>,
    pub navigation: Document,
    /// `None` when the navigation body is empty or existing data is invalid (rules tightened, database edited directly);
    /// the header then falls back to the default forum navigation.
    pub navigation_entries: Option<Vec<NavigationItem>>,
    /// The site's skin choice (docs/skins.md, "Choosing a skin"); `None` for the default skin.
    pub layout: Option<crate::skins::selection::Layout>,
    /// Each domain's override of it, by domain id.
    pub domain_layouts: Vec<(i64, crate::skins::selection::Layout)>,
    /// The places the site's document sets, read leniently once per load (docs/skins.md, "Places and blocks").
    pub places: Option<crate::layout::Places>,
    /// The places each domain's override sets, by domain id.
    pub domain_places: Vec<(i64, crate::layout::Places)>,
}

/// Friend links for the directory page, grouped by category.
pub type LinkDirectory = Vec<(LinkCategory, Vec<Listed>)>;

struct Slot<T> {
    state: Mutex<SlotState<T>>,
    /// Load coalescing: only one request at a time loads a block from the source; the others queue and first check
    /// whether it has already been loaded.
    fill: tokio::sync::Mutex<()>,
}

struct SlotState<T> {
    generation: u64,
    value: Option<(Arc<T>, Instant)>,
}

impl<T> Slot<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                generation: 0,
                value: None,
            }),
            fill: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState<T>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn clear(&self) {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1);
        state.value = None;
    }

    fn fresh(&self, ttl_of: &impl Fn(&T) -> u64) -> Option<(Arc<T>, Instant)> {
        let state = self.lock();
        let (value, at) = state.value.as_ref()?;
        (at.elapsed() < Duration::from_secs(ttl_of(value))).then(|| (value.clone(), *at))
    }

    /// Reads a block: uses the in-memory value if it has not expired, otherwise loads from the source. `ttl_of` gives
    /// this value's TTL (seconds), 0 meaning no caching; when `bypass` is true it neither queues nor reads or writes
    /// memory (the normal state with a TTL of 0).
    async fn get<E, F, Fut>(
        &self,
        name: &str,
        bypass: bool,
        ttl_of: impl Fn(&T) -> u64,
        fill: F,
    ) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        if !bypass && let Some((value, at)) = self.fresh(&ttl_of) {
            record(name, trace::BlockOutcome::Hit, Some(at));
            return Ok(value);
        }
        let _turn = if bypass {
            None
        } else {
            let turn = self.fill.lock().await;
            if let Some((value, at)) = self.fresh(&ttl_of) {
                record(name, trace::BlockOutcome::Hit, Some(at));
                return Ok(value);
            }
            Some(turn)
        };
        let generation = self.lock().generation;
        let value = Arc::new(fill().await?);
        let cacheable = ttl_of(&value) > 0;
        if cacheable {
            let mut state = self.lock();
            // Someone cleared this block during the load: what was read may be the stale value from before the
            // invalidation, so it is not written back.
            if state.generation == generation {
                state.value = Some((value.clone(), Instant::now()));
            }
        }
        record(
            name,
            if cacheable {
                trace::BlockOutcome::Filled
            } else {
                trace::BlockOutcome::Uncached
            },
            None,
        );
        Ok(value)
    }
}

impl<K: PartialEq + Clone, V: Clone> Slot<(K, V)> {
    /// A slot cached by bucket: memory holds `(bucket, value)`; the same bucket is a hit, a different one recomputes
    /// and replaces it (only the current bucket is kept). `bypass` is as in [`Slot::get`].
    async fn segment<E, F, Fut>(
        &self,
        name: &str,
        bypass: bool,
        segment: K,
        fill: F,
    ) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<V, E>>,
    {
        let key = segment.clone();
        let value = self
            .get(
                name,
                bypass,
                move |(stored, _): &(K, V)| if *stored == key { u64::MAX } else { 0 },
                || async move { Ok((segment, fill().await?)) },
            )
            .await?;
        Ok(value.1.clone())
    }
}

fn record(name: &str, outcome: trace::BlockOutcome, loaded: Option<Instant>) {
    debug_assert!(
        registry::slot(name).is_some(),
        "process cache slot {name} is not registered (cache/registry.rs)"
    );
    if let Some(profile) = trace::current() {
        profile.record_block(name, outcome, loaded.map(|at| at.elapsed()));
    }
}

/// What a copy of the boards' latest threads depends on: the list version (site version and new-post watermark) and
/// the boards, with whether each is top-level.
pub type BoardPostsKey = ((i64, i64), Vec<(i64, bool)>, Vec<i64>);
/// The latest thread of each board that has one, by `fid`.
pub type BoardPosts = Vec<(i64, crate::content::Thread)>;

struct Inner {
    redis: Cache,
    settings: Slot<SettingsBlock>,
    /// Every saved language of each policy (one slot per kind; saving any language clears it).
    policies: [Slot<PolicyCopies>; 6],
    branding: Slot<Vec<BrandAsset>>,
    credit_rules: Slot<Vec<Rule>>,
    home_links: Slot<Vec<HomeLink>>,
    link_directory: Slot<LinkDirectory>,
    pins: Slot<Vec<Pin>>,
    moderators: Slot<Vec<crate::organization::RosterEntry>>,
    honors: Slot<Vec<crate::honors::Honor>>,
    /// Readable forums: `[anonymous, member]`, each the result of the `organization::forums` SQL query.
    forums: [Slot<Vec<crate::organization::ForumRecord>>; 2],
    /// The board stylesheet compiled from the member copy of the readable forums, cleared with them.
    board_style: Slot<crate::organization::style::BoardStyle>,
    /// The latest thread of each board (data source `boards`): `[anonymous, members]`, keyed by the list version and
    /// the boards the view may read.
    board_last_posts: [Slot<(BoardPostsKey, BoardPosts)>; 2],
    /// The multi-domain sites' domain table (not read when the switch is off).
    domains: Slot<crate::domains::DomainMap>,
    /// Every skin: built in, imported and in the instance directory.
    skins: Slot<crate::skins::Catalog>,
    /// The last scan of the instance directory; the catalog reuses it until [`Inner::rescan`] asks for another.
    skin_scan: Mutex<Option<Arc<Vec<crate::skins::Listed>>>>,
    /// The instance directory is read at start and again after the skins block is cleared (import, delete,
    /// "Rescan", a broadcast of any of them), not on every reload of the block.
    rescan: AtomicBool,
    /// Site statistics: the bucket is `(statistics_bucket, site time zone offset)`.
    statistics: Slot<((i64, i64), Statistics)>,
    /// Scoped community statistics (multi-domain sites): the key is the forum
    /// IDs in the scope and the value records the bucket; only the current bucket is kept, and when the slot count
    /// exceeds [`SCOPED_STATISTICS`] the whole map is cleared and rebuilt.
    scoped_statistics: Mutex<ScopedStatistics>,
    /// Newest member for anonymous visitors: the current MySQL content version, the statistics bucket and the member
    /// count.
    recent_members: Slot<((i64, i64, i64), Vec<RecentMember>)>,
    /// Forum statistics: the bucket is `(forum_statistics_bucket, site day number, site time zone offset)`.
    forum_statistics: Slot<((i64, i64, i64), ForumStatistics)>,
    /// The `settings.cache.ttl_seconds` this process last decoded; `u64::MAX` means not read yet.
    /// The settings block's own TTL is known only once the value is read; at 0 there is no queueing, see
    /// [`GlobalCache::settings`].
    last_ttl: AtomicU64,
    /// The subscription task ends with the handle: it is cancelled when the last handle is dropped, leaving no standing
    /// connection.
    shutdown: CancellationToken,
    /// For tests: makes the subscription task disconnect once, to verify clearing after the reconnect. Kept in its own
    /// `Arc` so the subscription task need not hold the handle.
    reset: Arc<tokio::sync::Notify>,
    /// For tests: makes the subscription task act as if the next heartbeat never came back (effective once), to verify
    /// that a heartbeat timeout counts as a disconnect.
    mute_heartbeat: Arc<AtomicBool>,
    reconnects: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Inner {
    fn clear(&self, block: Block) {
        match block {
            Block::Settings => self.settings.clear(),
            Block::Policy(kind) => {
                if let Some(index) = POLICY_KINDS.iter().position(|k| *k == kind) {
                    self.policies[index].clear();
                }
            }
            Block::Branding => self.branding.clear(),
            Block::CreditRules => self.credit_rules.clear(),
            Block::Links => {
                self.home_links.clear();
                self.link_directory.clear();
            }
            Block::Pins => self.pins.clear(),
            Block::Moderators => self.moderators.clear(),
            Block::Honors => self.honors.clear(),
            Block::Forums => {
                for slot in &self.forums {
                    slot.clear();
                }
                self.board_style.clear();
            }
            Block::Domains => self.domains.clear(),
            Block::Skins => {
                self.rescan.store(true, Ordering::Relaxed);
                self.skins.clear();
            }
        }
    }

    /// For reconnects, unrecognized broadcasts and tests: clears everything, statistics slots included (recomputed by
    /// the current bucket next time).
    fn clear_all(&self) {
        self.settings.clear();
        for slot in &self.policies {
            slot.clear();
        }
        self.branding.clear();
        self.credit_rules.clear();
        self.home_links.clear();
        self.link_directory.clear();
        self.pins.clear();
        self.moderators.clear();
        self.honors.clear();
        for slot in &self.forums {
            slot.clear();
        }
        self.board_style.clear();
        for slot in &self.board_last_posts {
            slot.clear();
        }
        self.domains.clear();
        self.rescan.store(true, Ordering::Relaxed);
        self.skins.clear();
        self.statistics.clear();
        self.scoped_statistics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.recent_members.clear();
        self.forum_statistics.clear();
    }

    /// On a broadcast: clears block by block. Any unrecognized block name (a newer version added a block during a
    /// rolling deploy) clears everything; one extra database read is the safer trade.
    fn apply(&self, message: &str) {
        if message == EVERYTHING {
            self.clear_all();
            return;
        }
        for name in message.split(',') {
            match Block::parse(name) {
                Some(block) => self.clear(block),
                None => {
                    self.clear_all();
                    return;
                }
            }
        }
    }

    fn ttl(&self) -> Option<u64> {
        match self.last_ttl.load(Ordering::Relaxed) {
            u64::MAX => None,
            ttl => Some(ttl),
        }
    }
}

/// Handle to the in-process global cache. Cloning copies just one `Arc`.
#[derive(Clone)]
pub struct GlobalCache {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for GlobalCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GlobalCache")
            .field("redis_enabled", &self.inner.redis.enabled())
            .finish_non_exhaustive()
    }
}

impl GlobalCache {
    /// A handle without a subscription: invalidation clears only this process, and with Redis enabled it still
    /// publishes. Single applications in route tests use it; production startup goes through
    /// [`GlobalCache::start`].
    pub fn new(redis: Cache) -> Self {
        Self {
            inner: Arc::new(Inner {
                redis,
                settings: Slot::new(),
                policies: std::array::from_fn(|_| Slot::new()),
                branding: Slot::new(),
                credit_rules: Slot::new(),
                home_links: Slot::new(),
                link_directory: Slot::new(),
                pins: Slot::new(),
                moderators: Slot::new(),
                honors: Slot::new(),
                forums: [Slot::new(), Slot::new()],
                board_style: Slot::new(),
                board_last_posts: [Slot::new(), Slot::new()],
                domains: Slot::new(),
                skins: Slot::new(),
                skin_scan: Mutex::new(None),
                rescan: AtomicBool::new(true),
                statistics: Slot::new(),
                scoped_statistics: Mutex::new(std::collections::BTreeMap::new()),
                recent_members: Slot::new(),
                forum_statistics: Slot::new(),
                last_ttl: AtomicU64::new(u64::MAX),
                shutdown: CancellationToken::new(),
                reset: Arc::new(tokio::sync::Notify::new()),
                mute_heartbeat: Arc::new(AtomicBool::new(false)),
                reconnects: AtomicU64::new(0),
            }),
        }
    }

    /// For production startup: with Redis enabled, establishes the subscription connection first and returns an error
    /// if it cannot, so the caller refuses to start.
    /// With Redis disabled it is a single process and no subscription is created.
    pub async fn start(redis: Cache) -> anyhow::Result<Self> {
        Self::start_with_heartbeat(redis, HEARTBEAT).await
    }

    /// Same as [`GlobalCache::start`], with a separate heartbeat interval (short intervals for tests).
    #[doc(hidden)]
    pub async fn start_with_heartbeat(redis: Cache, interval: Duration) -> anyhow::Result<Self> {
        let global = Self::new(redis);
        if global.inner.redis.enabled() {
            let channel = global.channel();
            // One heartbeat channel per handle, subscribed only by itself: a heartbeat that does not come back means
            // this subscription connection is broken.
            let heartbeat = format!("{channel}:heartbeat:{}", crate::identity::random_token());
            let subscription = global
                .inner
                .redis
                .subscribe(&[&channel, &heartbeat])
                .await
                .with_context(|| {
                    format!("cannot subscribe to the global cache invalidation channel {channel}: with Redis enabled the subscription is required at startup; check the Redis service and the account permissions (SUBSCRIBE is needed)")
                })?;
            tokio::spawn(listen(
                Arc::downgrade(&global.inner),
                Signals {
                    shutdown: global.inner.shutdown.clone(),
                    reset: global.inner.reset.clone(),
                    mute_heartbeat: global.inner.mute_heartbeat.clone(),
                },
                global.inner.redis.clone(),
                (channel, heartbeat, interval),
                subscription,
            ));
        }
        Ok(global)
    }

    /// The Redis cache inside the handle (all reads miss when Redis is disabled). A background job needs only a
    /// process cache handle.
    pub fn redis(&self) -> &Cache {
        &self.inner.redis
    }

    pub fn channel(&self) -> String {
        format!("{}:{CHANNEL}", self.inner.redis.namespace())
    }

    /// The process cache TTL (seconds) this process last read; unknown right after a cold start.
    pub fn ttl_seconds(&self) -> Option<u64> {
        self.inner.ttl()
    }

    /// Called after the write commits: clears this process first, then publishes when Redis is enabled. If publishing
    /// fails, this process has already been cleared and [`Unpublished`] is returned; the caller converts it into its
    /// own error type, and the response layer returns 503.
    pub async fn changed(&self, blocks: &[Block]) -> Result<(), Unpublished> {
        let ttl_seconds = self.inner.ttl();
        for block in blocks {
            self.inner.clear(*block);
        }
        if !self.inner.redis.enabled() || blocks.is_empty() {
            return Ok(());
        }
        let message = blocks
            .iter()
            .map(|block| block.name())
            .collect::<Vec<_>>()
            .join(",");
        self.inner
            .redis
            .publish(&self.channel(), &message)
            .await
            .map(drop)
            .map_err(|error| Unpublished {
                ttl_seconds,
                detail: crate::debug::visible_error(&error),
            })
    }

    /// Same as [`GlobalCache::changed`], with the result converted to `anyhow`: each service's `Internal(anyhow)` can
    /// hold it directly, and the response layer recognizes it as [`Unpublished`].
    pub async fn announce(&self, blocks: &[Block]) -> anyhow::Result<()> {
        self.changed(blocks).await.map_err(anyhow::Error::new)
    }

    /// For tests and after editing the database directly: clears all blocks in this process, without publishing.
    pub fn clear_local(&self) {
        self.inner.clear_all();
    }

    /// Admin "Refresh caches": clears all blocks and statistics slots in
    /// this process and, with Redis enabled, broadcasts `*` so other processes clear everything too. Use it after
    /// editing the database by hand or changing settings from the command line. Publish failures are handled as in
    /// [`GlobalCache::changed`].
    pub async fn refresh_all(&self) -> Result<(), Unpublished> {
        let ttl_seconds = self.inner.ttl();
        self.inner.clear_all();
        if !self.inner.redis.enabled() {
            return Ok(());
        }
        self.inner
            .redis
            .publish(&self.channel(), EVERYTHING)
            .await
            .map(drop)
            .map_err(|error| Unpublished {
                ttl_seconds,
                detail: crate::debug::visible_error(&error),
            })
    }

    /// Site settings and navigation. The page frame, robots, sitemap, feeds and member API all read from here; the
    /// write gate does not.
    pub async fn settings(&self, db: &Database) -> Result<Arc<SettingsBlock>, IdentityError> {
        // With a TTL of 0 nothing is cached and nothing queues: each request reads on its own as before. Before the
        // first read the TTL is unknown, so it is treated as "queue", which at most merges concurrent requests at a cold
        // start into one load.
        let bypass = self.inner.last_ttl.load(Ordering::Relaxed) == 0;
        let inner = &self.inner;
        self.inner
            .settings
            .get(
                "settings",
                bypass,
                |block: &SettingsBlock| block.settings.cache_ttl(),
                || async {
                    let crate::runtime::FrameRows {
                        settings,
                        navigation,
                        layout,
                        domain_layouts,
                    } = crate::runtime::read_with_navigation(db).await?;
                    inner.last_ttl.store(settings.cache_ttl(), Ordering::Relaxed);
                    let navigation_entries = if navigation.body.trim().is_empty() {
                        None
                    } else {
                        // Validated when saved in the admin area; existing data may still be invalid. Navigation only
                        // affects the header and must not bring down every page, including /manage/navigation, which
                        // is used to fix it.
                        crate::runtime::parse_navigation(&navigation.body)
                            .inspect_err(|error| {
                                tracing::warn!(updated_at = navigation.updated_at, %error, "navigation document is invalid; the header fell back to the default forum navigation; save it again at /manage/navigation");
                            })
                            .ok()
                    };
                    let places = layout.as_ref().and_then(|layout| layout.places("the site"));
                    let domain_places = domain_layouts
                        .iter()
                        .filter_map(|(id, layout)| {
                            layout
                                .places(&format!("domain {id}"))
                                .map(|places| (*id, places))
                        })
                        .collect();
                    Ok(SettingsBlock {
                        settings: Arc::new(settings),
                        navigation,
                        navigation_entries,
                        layout,
                        domain_layouts,
                        places,
                        domain_places,
                    })
                },
            )
            .await
    }

    /// One policy document (`/terms`, `/privacy`, `/rules`, `/contact`, and the descriptions of `/verified` and
    /// `/links`) in this request's language (docs/architecture.md, "Languages"). A language without its own copy gets
    /// an empty document with `updated_at` 0, and the page uses its built-in text.
    pub async fn policy(
        &self,
        db: &Database,
        kind: &str,
        ttl: u64,
    ) -> Result<Arc<Document>, IdentityError> {
        let Some(index) = POLICY_KINDS.iter().position(|k| *k == kind) else {
            return Err(IdentityError::NotFound);
        };
        let copies = self.inner.policies[index]
            .get(
                POLICY_KINDS[index],
                ttl == 0,
                |_| ttl,
                || async {
                    crate::runtime::policy_copies(db, POLICY_KINDS[index])
                        .await
                        .map(|copies| {
                            PolicyCopies(
                                copies
                                    .into_iter()
                                    .map(|(lang, doc)| (lang, Arc::new(doc)))
                                    .collect(),
                            )
                        })
                },
            )
            .await?;
        Ok(copies
            .0
            .get(&crate::i18n::current())
            .cloned()
            .unwrap_or_default())
    }

    /// Every skin of the three sources. `dir` is the instance directory (`$GONOF_HOME/skins`), read again only after
    /// the block was cleared; the built-in skins are checked once per process and the imported ones on every load.
    pub async fn skins(
        &self,
        db: &Database,
        dir: &std::path::Path,
        ttl: u64,
    ) -> anyhow::Result<Arc<crate::skins::Catalog>> {
        let inner = &self.inner;
        self.inner
            .skins
            .get(
                "skins",
                ttl == 0,
                |_| ttl,
                || async {
                    let imported = crate::skins::store::listed(db).await?;
                    let scanned = if inner.rescan.swap(false, Ordering::Relaxed) {
                        None
                    } else {
                        inner
                            .skin_scan
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                    };
                    let scanned = match scanned {
                        Some(scanned) => scanned,
                        None => {
                            let dir = dir.to_owned();
                            let scanned = Arc::new(
                                tokio::task::spawn_blocking(move || {
                                    crate::skins::catalog::scan(&dir)
                                })
                                .await?,
                            );
                            *inner
                                .skin_scan
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(scanned.clone());
                            scanned
                        }
                    };
                    let mut listed = crate::skins::catalog::builtin();
                    listed.extend(imported);
                    listed.extend(scanned.iter().cloned());
                    Ok(crate::skins::Catalog::build(listed))
                },
            )
            .await
    }

    pub async fn branding(&self, media: &Media, ttl: u64) -> anyhow::Result<Arc<Vec<BrandAsset>>> {
        self.inner
            .branding
            .get(
                "branding",
                ttl == 0,
                |_| ttl,
                || async { media.brand_assets().await.map_err(anyhow::Error::new) },
            )
            .await
    }

    /// All credit reward rule rows. For display only (the check-in switch and amount on member cards, the rules page);
    /// reward transactions still read the database with locks.
    pub async fn credit_rules(&self, db: &Database, ttl: u64) -> anyhow::Result<Arc<Vec<Rule>>> {
        self.inner
            .credit_rules
            .get(
                "credit-rules",
                ttl == 0,
                |_| ttl,
                || crate::activities::rules(db),
            )
            .await
    }

    pub async fn home_links(&self, db: &Database, ttl: u64) -> anyhow::Result<Arc<Vec<HomeLink>>> {
        self.inner
            .home_links
            .get(
                "links.home",
                ttl == 0,
                |_| ttl,
                || async { crate::links::home(db).await.map_err(anyhow::Error::new) },
            )
            .await
    }

    pub async fn link_directory(
        &self,
        db: &Database,
        ttl: u64,
    ) -> anyhow::Result<Arc<LinkDirectory>> {
        self.inner
            .link_directory
            .get(
                "links.directory",
                ttl == 0,
                |_| ttl,
                || async {
                    crate::links::directory(db)
                        .await
                        .map_err(anyhow::Error::new)
                },
            )
            .await
    }

    /// Index of site-wide level-2 and level-3 pins. Only used to pick the thread IDs
    /// merged into page 1 of a forum; when rows are fetched every condition is rechecked for the visitor, see
    /// [`crate::content::pins`].
    pub async fn pins(&self, db: &Database, ttl: u64) -> anyhow::Result<Arc<Vec<Pin>>> {
        self.inner
            .pins
            .get(
                "pins",
                ttl == 0,
                |_| ttl,
                || crate::content::pins::index(db),
            )
            .await
    }

    /// Honor group directory (there are few groups, one copy of the whole table): floor and profile badges read it,
    /// and grant records are read per member from their own table (docs/queries.md, "Process caches").
    /// Cleared after groups are saved in the admin area.
    pub async fn honors(
        &self,
        db: &Database,
        ttl: u64,
    ) -> anyhow::Result<Arc<Vec<crate::honors::Honor>>> {
        self.inner
            .honors
            .get(
                "honors",
                ttl == 0,
                |_| ttl,
                || async {
                    crate::honors::honors(db)
                        .await
                        .map_err(|error| anyhow::anyhow!("{error}"))
                },
            )
            .await
    }

    /// Moderator roster (docs/queries.md, "Process caches"): used for display in forum headers and the admin forum
    /// list, by the thread page's moderation entry and by the read-only check for moderators downloading hidden
    /// attachments. Authorization inside write transactions still locks the appointment rows.
    pub async fn moderators(
        &self,
        db: &Database,
        ttl: u64,
    ) -> anyhow::Result<Arc<Vec<crate::organization::RosterEntry>>> {
        self.inner
            .moderators
            .get(
                "moderators",
                ttl == 0,
                |_| ttl,
                || crate::organization::moderator_roster(db),
            )
            .await
    }

    /// The forums this visitor can read (docs/settings-and-caching.md, "The process cache"): `members` false is the
    /// anonymous copy (`visibility='public'`), true is the signed-in member copy (`IN ('public','members')`); a child
    /// forum counts only when its parent is readable too, judged in code. The page frame's navigation, forum bar,
    /// child forum tabs and breadcrumbs, the forum picker when posting, the member API's forum list, `/feeds` and the
    /// sitemap all read it; admin edit pages and the forum path locks inside write transactions still read the
    /// database.
    pub async fn forums(
        &self,
        db: &Database,
        members: bool,
        ttl: u64,
    ) -> anyhow::Result<Arc<Vec<crate::organization::ForumRecord>>> {
        let (slot, name) = if members {
            (&self.inner.forums[1], "forums-members")
        } else {
            (&self.inner.forums[0], "forums-public")
        };
        slot.get(
            name,
            ttl == 0,
            |_| ttl,
            || crate::organization::readable_forums(db, members),
        )
        .await
    }

    /// The board stylesheet (docs/skins.md, "Board colors and glyphs"), compiled from the forums signed-in members
    /// can read and kept until the forums block is cleared.
    pub async fn board_style(
        &self,
        db: &Database,
        ttl: u64,
    ) -> anyhow::Result<Arc<crate::organization::style::BoardStyle>> {
        self.inner
            .board_style
            .get(
                "board-style",
                ttl == 0,
                |_| ttl,
                || async {
                    let forums = self.forums(db, true, ttl).await?;
                    Ok(crate::organization::style::compile(&forums))
                },
            )
            .await
    }

    /// The latest thread of each board for the `boards` data source (docs/data.md): kept per view (`members` or
    /// anonymous) while the list version and the boards are the same; a new version reads again.
    pub async fn board_last_posts<F, Fut>(
        &self,
        members: bool,
        key: BoardPostsKey,
        fill: F,
    ) -> anyhow::Result<Vec<(i64, crate::content::Thread)>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Vec<(i64, crate::content::Thread)>>>,
    {
        let (slot, name) = if members {
            (&self.inner.board_last_posts[1], "board-last-posts-members")
        } else {
            (&self.inner.board_last_posts[0], "board-last-posts-public")
        };
        slot.segment(name, self.statistics_bypass(), key, fill)
            .await
    }

    /// The multi-domain sites' domain table: the entry layer reads it once per request
    /// when the switch is on.
    pub async fn domains(
        &self,
        db: &Database,
        ttl: u64,
    ) -> anyhow::Result<Arc<crate::domains::DomainMap>> {
        self.inner
            .domains
            .get(
                "domains",
                ttl == 0,
                |_| ttl,
                || crate::domains::DomainMap::load(db),
            )
            .await
    }

    /// Scoped community statistics: cached in process by `(scope, bucket)`; a miss goes to `fill` (Redis layer and
    /// database). Loads are not coalesced: there are very few scopes, and an occasional simultaneous recompute is
    /// acceptable.
    pub async fn scoped_statistics<F, Fut>(
        &self,
        scope: &[i64],
        segment: (i64, i64),
        fill: F,
    ) -> anyhow::Result<Statistics>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Statistics>>,
    {
        let bypass = self.statistics_bypass();
        let lock = || {
            self.inner
                .scoped_statistics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if !bypass
            && let Some((stored, value)) = lock().get(scope)
            && *stored == segment
        {
            record("scoped-statistics", trace::BlockOutcome::Hit, None);
            return Ok(value.clone());
        }
        let value = fill().await?;
        if !bypass {
            let mut map = lock();
            if map.len() >= SCOPED_STATISTICS && !map.contains_key(scope) {
                map.clear();
            }
            map.insert(scope.to_vec(), (segment, value.clone()));
        }
        record(
            "scoped-statistics",
            if bypass {
                trace::BlockOutcome::Uncached
            } else {
                trace::BlockOutcome::Filled
            },
            None,
        );
        Ok(value)
    }

    /// Statistics slots are not bound by `settings.cache.ttl_seconds` and change by bucket; they are bypassed only when
    /// the TTL is set to 0 (SQL profiling).
    pub(crate) fn statistics_bypass(&self) -> bool {
        self.inner.last_ttl.load(Ordering::Relaxed) == 0
    }

    /// The three site statistics numbers: cached in process by `(bucket, site time zone offset)`; a miss goes to `fill`
    /// (Redis layer and database).
    pub async fn statistics<F, Fut>(
        &self,
        segment: (i64, i64),
        fill: F,
    ) -> anyhow::Result<Statistics>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Statistics>>,
    {
        self.inner
            .statistics
            .segment("statistics", self.statistics_bypass(), segment, fill)
            .await
    }

    /// Newest member for anonymous visitors, bucketed like the site statistics; for signed-in members
    /// it is still queried live, not through here.
    pub async fn recent_members<F, Fut>(
        &self,
        segment: (i64, i64, i64),
        fill: F,
    ) -> anyhow::Result<Vec<RecentMember>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Vec<RecentMember>>>,
    {
        self.inner
            .recent_members
            .segment("recent-members", self.statistics_bypass(), segment, fill)
            .await
    }

    /// Forum statistics: cached by `(bucket, site day number, time zone offset)`, switching
    /// after midnight.
    pub async fn forum_statistics<F, Fut>(
        &self,
        segment: (i64, i64, i64),
        fill: F,
    ) -> anyhow::Result<ForumStatistics>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<ForumStatistics>>,
    {
        self.inner
            .forum_statistics
            .segment("forum-statistics", self.statistics_bypass(), segment, fill)
            .await
    }

    /// For tests: makes the subscription task disconnect and reconnect once; returns the reconnect count before the
    /// call.
    #[doc(hidden)]
    pub fn drop_subscription_for_test(&self) -> u64 {
        let before = self.inner.reconnects.load(Ordering::Acquire);
        self.inner.reset.notify_one();
        before
    }

    /// For tests: makes the subscription task act as if the next heartbeat never came back (effective once), so it
    /// reconnects as after a disconnect once the heartbeat times out.
    #[doc(hidden)]
    pub fn mute_heartbeat_once_for_test(&self) -> u64 {
        let before = self.inner.reconnects.load(Ordering::Acquire);
        self.inner.mute_heartbeat.store(true, Ordering::Release);
        before
    }

    /// For tests: how many times the subscription task has reconnected successfully.
    #[doc(hidden)]
    pub fn reconnects(&self) -> u64 {
        self.inner.reconnects.load(Ordering::Acquire)
    }
}

/// The signals the subscription task listens for.
struct Signals {
    shutdown: CancellationToken,
    reset: Arc<tokio::sync::Notify>,
    mute_heartbeat: Arc<AtomicBool>,
}

/// Standing subscription: clears on receipt; after a disconnect (heartbeat timeouts included) it reconnects with
/// 1, 2, 4…30 second backoff, and a successful reconnect clears all blocks in this process.
async fn listen(
    inner: Weak<Inner>,
    signals: Signals,
    redis: Cache,
    (channel, heartbeat, interval): (String, String, Duration),
    mut subscription: redis::aio::PubSub,
) {
    let Signals {
        shutdown,
        reset,
        mute_heartbeat,
    } = signals;
    loop {
        {
            let mut stream = subscription.into_on_message();
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // A heartbeat has been sent and has not come back yet.
            let mut awaiting = false;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = reset.notified() => break,
                    _ = ticker.tick() => {
                        if awaiting {
                            mute_heartbeat.store(false, Ordering::Release);
                            tracing::warn!(%channel, "global cache subscription heartbeat did not come back within one beat; treating it as a disconnect");
                            break;
                        }
                        match redis.publish(&heartbeat, "ping").await {
                            Ok(0) => {
                                tracing::warn!(%channel, "global cache subscription heartbeat reached no subscriber; treating it as a disconnect");
                                break;
                            }
                            Ok(_) => awaiting = true,
                            Err(error) => {
                                tracing::warn!(%channel, error = %crate::debug::visible_error(&error), "global cache subscription heartbeat could not be sent; treating it as a disconnect");
                                break;
                            }
                        }
                    }
                    message = stream.next() => match message {
                        Some(message) if message.get_channel_name() == heartbeat => {
                            if !mute_heartbeat.load(Ordering::Acquire) {
                                awaiting = false;
                            }
                        }
                        Some(message) => {
                            let Some(inner) = inner.upgrade() else { return };
                            match message.get_payload::<String>() {
                                Ok(payload) => inner.apply(&payload),
                                Err(_) => inner.clear_all(),
                            }
                        }
                        None => break,
                    },
                }
            }
        }
        tracing::warn!(%channel, "global cache subscription disconnected, reconnecting in the background; until then, saves by other processes take effect only after at most one TTL");
        let mut delay = Duration::from_secs(1);
        subscription = loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            match redis.subscribe(&[&channel, &heartbeat]).await {
                Ok(subscription) => break subscription,
                Err(error) => {
                    tracing::warn!(error = %crate::debug::visible_error(&error), "global cache subscription reconnect failed; will retry later");
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        };
        let Some(strong) = inner.upgrade() else {
            return;
        };
        strong.clear_all();
        strong.reconnects.fetch_add(1, Ordering::AcqRel);
        tracing::info!(%channel, "global cache subscription reconnected; cleared all blocks in this process");
    }
}

/// For the response layer: returns 503 when the save committed but the broadcast failed, with a body stating that the
/// data was saved and the staleness bound.
pub fn unpublished(error: &anyhow::Error) -> Option<&Unpublished> {
    error.downcast_ref::<Unpublished>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_names_round_trip() {
        for block in [
            Block::Settings,
            Block::Branding,
            Block::CreditRules,
            Block::Links,
            Block::Pins,
            Block::Moderators,
            Block::Forums,
            Block::Domains,
            Block::Policy("policy.terms"),
            Block::Policy("policy.links"),
        ] {
            assert_eq!(Block::parse(block.name()), Some(block));
        }
        assert_eq!(Block::parse("navigation"), None);
        assert_eq!(
            Block::policy("policy.rules"),
            Some(Block::Policy("policy.rules"))
        );
        assert_eq!(Block::policy("navigation"), None);
    }

    #[tokio::test]
    async fn slots_cache_until_cleared_and_never_write_back_stale_fills() {
        let slot: Slot<i64> = Slot::new();
        let calls = std::sync::atomic::AtomicI64::new(0);
        let fill = || async {
            Ok::<_, std::convert::Infallible>(calls.fetch_add(1, Ordering::SeqCst) + 1)
        };
        assert_eq!(*slot.get("settings", false, |_| 30, fill).await.unwrap(), 1);
        assert_eq!(
            *slot.get("settings", false, |_| 30, fill).await.unwrap(),
            1,
            "a hit within the lifetime"
        );
        slot.clear();
        assert_eq!(
            *slot.get("settings", false, |_| 30, fill).await.unwrap(),
            2,
            "after a clear it loads from the source again"
        );
        // TTL of 0: every call loads from the source, and nothing stays in memory.
        assert_eq!(*slot.get("settings", true, |_| 0, fill).await.unwrap(), 3);
        assert_eq!(*slot.get("settings", true, |_| 0, fill).await.unwrap(), 4);
        assert_eq!(
            *slot.get("settings", false, |_| 30, fill).await.unwrap(),
            2,
            "a bypass leaves the value in memory alone"
        );
        // Cleared during the load: this result is still returned, but not written back.
        slot.clear();
        let stale = slot
            .get(
                "settings",
                false,
                |_| 30,
                || async {
                    slot.clear();
                    Ok::<_, std::convert::Infallible>(100)
                },
            )
            .await
            .unwrap();
        assert_eq!(*stale, 100);
        assert_eq!(
            *slot.get("settings", false, |_| 30, fill).await.unwrap(),
            5,
            "the stale value is not written back"
        );
        // A failed load is not cached.
        slot.clear();
        assert!(
            slot.get(
                "settings",
                false,
                |_| 30,
                || async { Err::<i64, _>("broken") }
            )
            .await
            .is_err()
        );
        assert_eq!(*slot.get("settings", false, |_| 30, fill).await.unwrap(), 6);
    }

    #[test]
    fn broadcasts_clear_named_blocks_and_unknown_names_clear_everything() {
        let config = crate::config::RedisConfig {
            enabled: false,
            url: String::new(),
            ..crate::config::RedisConfig::default()
        };
        let global = GlobalCache::new(Cache::new(&config).unwrap());
        let inner = &global.inner;
        let fill = |inner: &Inner| {
            for slot in &inner.policies {
                slot.lock().value = Some((Arc::new(PolicyCopies::default()), Instant::now()));
            }
            inner.home_links.lock().value = Some((Arc::new(Vec::new()), Instant::now()));
            inner.link_directory.lock().value = Some((Arc::new(Vec::new()), Instant::now()));
            inner.credit_rules.lock().value = Some((Arc::new(Vec::new()), Instant::now()));
        };
        fill(inner);
        inner.apply("links");
        assert!(
            inner.home_links.lock().value.is_none() && inner.link_directory.lock().value.is_none()
        );
        assert!(
            inner.credit_rules.lock().value.is_some(),
            "only the named blocks are cleared"
        );
        inner.apply("policy.terms,credit-rules");
        assert!(inner.policies[0].lock().value.is_none());
        assert!(
            inner.policies[1].lock().value.is_some(),
            "other policy documents are untouched"
        );
        assert!(inner.credit_rules.lock().value.is_none());
        // Both forum slots are cleared together; other blocks are untouched.
        for slot in &inner.forums {
            slot.lock().value = Some((Arc::new(Vec::new()), Instant::now()));
        }
        inner.apply("forums");
        assert!(inner.forums.iter().all(|slot| slot.lock().value.is_none()));
        assert!(inner.policies[1].lock().value.is_some());
        fill(inner);
        inner.apply("not-a-block");
        assert!(
            inner
                .policies
                .iter()
                .all(|slot| slot.lock().value.is_none()),
            "an unknown block name clears everything"
        );
        assert!(inner.credit_rules.lock().value.is_none());
        assert_eq!(
            global.ttl_seconds(),
            None,
            "the cache lifetime has not been read yet"
        );
    }
}
