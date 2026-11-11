//! Caching: two layers, process cache and Redis cache, split by deployment shape.
//!
//! - The **process cache** ([`process`]) is the system cache: shared site-wide, not per
//!   person, with a fixed number of entries and a countable set of write points. It exists both with Redis disabled
//!   and enabled, and is the main body of caching. Blocks are cleared by the save functions after the write commits,
//!   and when Redis is enabled the change is then broadcast to other processes; the shared lifetime
//!   `settings.cache.ttl_seconds` is the backstop, 0 means off. The statistics slots switch keys by time period, and
//!   on an in-process miss they read Redis next.
//! - The **Redis cache** ([`redis`]) is the memory cache: per-person entries (uid in the key,
//!   [`member`]) and entries per page and parameters with no fixed count (anonymous lists, floors, feeds). **It
//!   exists only when `redis.enabled`**; when disabled the database is read directly, with no separate in-process
//!   copy. Keys may only use versions this request read from the database; a version read from Redis must not be
//!   used to reuse content.
//! - **Image bytes** ([`images`]) live in process and are checked against the current binding in the database before
//!   use.
//!
//! Every cache is registered in [`registry`]: name, layer, invalidation method, lifetime source and whether it is per
//! person. Redis cache keys can only be built from registry entries ([`CacheKey::new`]); when the process cache reads
//! a block, debug builds check that the slot name is registered; the debug panel shows names from the registry.
//!
//! Not cached: authentication, the write gate `GATE_SQL`, locking reads and permission rechecks inside transactions,
//! and current values read by admin-area edit pages.
//!
//! Two other uses of Redis are not caching and are not in the registry: rate limiting ([`crate::rate_limit`]) and the
//! invalidation broadcast of the process cache (channel `{namespace}:global-cache`).
pub mod images;
pub(crate) mod member;
pub mod process;
pub mod redis;
pub mod registry;

pub use self::{
    images::ImageCache,
    process::{
        Block, CHANNEL, GlobalCache, LinkDirectory, POLICY_KINDS, SettingsBlock, Unpublished,
        unpublished,
    },
    redis::{
        Cache, CacheKey, Lifetimes, TimedCache, Unavailable, protect, unavailable,
        unavailable_detail,
    },
};
