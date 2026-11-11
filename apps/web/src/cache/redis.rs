//! Redis cache: exists only when `redis.enabled`; when disabled every read misses and every
//! write is dropped, and callers read the database directly.
//! Stores only rebuildable data; versions and permissions must be read from MySQL first. Keys can only be built from
//! registry entries ([`CacheKey::new`]).
//!
//! Member-private data may be cached, but **the uid must be encoded into the key** (the `variants` of `CacheKey::new`).
//! One hard exception: call sites that fold the authentication check into the query are never cached — caching them
//! means caching the authentication verdict, and frozen accounts or accounts whose credentials were just rotated
//! would keep passing.
//!
//! The lifetimes of the cache kinds are site settings (`settings.cache.page_ttl_seconds`, `list_ttl_seconds`,
//! `statistics_seconds`, `forum_statistics_seconds`; global-settings Z1, made settings as a whole group by global
//! cache Q1 of 2026-11-03); the caller takes them from this request's settings and passes them in through
//! [`Lifetimes`]. The deployment config no longer has lifetime fields.
//!
//! The process cache (site settings, policy documents, brand images, etc.) is not here, see [`super::process`];
//! its cross-process invalidation broadcast goes through this module's [`Cache::publish`] and [`Cache::subscribe`].
use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use redis::{FromRedisValue, aio::MultiplexedConnection};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::registry::{Entry, Lifetime};
use crate::{config::RedisConfig, debug::trace};

#[derive(Clone)]
pub struct Cache {
    inner: Arc<CacheInner>,
}

struct CacheInner {
    /// No client is built when disabled: local profiling does not require redis.url to parse.
    client: Option<redis::Client>,
    connection: Mutex<ConnectionSlot>,
    config: RedisConfig,
    /// Keys currently being filled from the source: within one process only one request at a time fills the same key,
    /// see [`Cache::get_or_fill_json`].
    fills: FillRegistry,
}

type FillRegistry = std::sync::Mutex<std::collections::HashMap<String, FillEntry>>;

#[derive(Default)]
struct FillEntry {
    gate: Arc<Mutex<()>>,
    users: usize,
}

/// Each request that is filling or waiting holds one. It must be returned even when the request is cancelled, so the
/// lock of a cold key does not stay in the table.
struct FillRegistration<'a> {
    registry: &'a FillRegistry,
    digest: &'a str,
    gate: Arc<Mutex<()>>,
}

impl<'a> FillRegistration<'a> {
    fn new(registry: &'a FillRegistry, digest: &'a str) -> Self {
        let mut fills = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = fills.entry(digest.to_owned()).or_default();
        entry.users += 1;
        Self {
            registry,
            digest,
            gate: entry.gate.clone(),
        }
    }
}

impl Drop for FillRegistration<'_> {
    fn drop(&mut self) {
        let mut fills = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = fills
            .get_mut(self.digest)
            .expect("registered fill lock still in the table");
        entry.users -= 1;
        if entry.users == 0 {
            fills.remove(self.digest);
        }
    }
}

#[derive(Default)]
struct ConnectionSlot {
    generation: u64,
    connection: Option<MultiplexedConnection>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Cache")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

/// Upper bound (seconds) on the lifetime of member page frame Redis keys. It shares `settings.cache.ttl_seconds` with
/// the process cache, but a failed key deletion or a concurrent read/write collision leaves it stale for one lifetime,
/// so it is not lengthened along with it.
pub const MEMBER_MAX_SECONDS: u64 = 300;

/// Lifetimes (seconds) of the Redis data cache kinds, taken from the site settings this request read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lifetimes {
    pub page: u64,
    /// Member page frame: `settings.cache.ttl_seconds`, capped at [`MEMBER_MAX_SECONDS`]; 0 means read the database
    /// directly.
    pub member: u64,
    pub list: u64,
    /// Site-wide statistics and the newest member shown to anonymous visitors are bucketed by this duration; the key
    /// carries the bucket number and the lifetime equals the bucket length.
    pub statistics: u64,
    /// Bucket length for forum statistics, used the same way.
    pub forum_statistics: u64,
}

impl Lifetimes {
    pub fn from_settings(settings: &crate::runtime::SiteSettings) -> Self {
        let seconds = |value: i64| u64::try_from(value).unwrap_or(1).max(1);
        Self {
            member: settings.cache_ttl().min(MEMBER_MAX_SECONDS),
            page: seconds(settings.cache.page_ttl_seconds),
            list: seconds(settings.cache.list_ttl_seconds),
            statistics: seconds(settings.cache.statistics_seconds),
            forum_statistics: seconds(settings.cache.forum_statistics_seconds),
        }
    }

    /// 0 means not cached. Process cache blocks and entries evicted by capacity do not go into Redis; they get 0 here.
    fn of(self, lifetime: Lifetime) -> u64 {
        match lifetime {
            Lifetime::Member => self.member,
            Lifetime::Process => 0,
            Lifetime::Page => self.page,
            Lifetime::List => self.list,
            Lifetime::Statistics => self.statistics,
            Lifetime::ForumStatistics => self.forum_statistics,
            Lifetime::Capacity => 0,
        }
    }

    /// Statistics bucket number: `now / bucket length`. Changing the length naturally changes the key.
    pub fn statistics_bucket(self, now: i64) -> i64 {
        now.div_euclid(i64::try_from(self.statistics).unwrap_or(i64::MAX))
    }

    /// Forum statistics bucket number: `now / bucket length`.
    pub fn forum_statistics_bucket(self, now: i64) -> i64 {
        now.div_euclid(i64::try_from(self.forum_statistics).unwrap_or(i64::MAX))
    }
}

/// Cache handle carrying this request's lifetimes: the content layer's cache functions all take it, so lifetimes need
/// not be passed separately all the way down.
#[derive(Clone, Debug)]
pub struct TimedCache {
    cache: Cache,
    lifetimes: Lifetimes,
}

impl TimedCache {
    pub fn lifetimes(&self) -> Lifetimes {
        self.lifetimes
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &CacheKey) -> Result<Option<T>> {
        self.cache.get_json(key).await
    }

    pub async fn set_json<T: Serialize>(&self, key: &CacheKey, value: &T) -> Result<()> {
        self.cache.set_json(key, value, self.lifetimes).await
    }

    pub async fn get_or_fill_json<T, F, Fut>(&self, key: &CacheKey, fill: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        self.cache.get_or_fill_json(key, self.lifetimes, fill).await
    }
}

/// Only versions this request has already read from MySQL may be used; never read a version from Redis and reuse
/// content with it.
#[derive(Clone, Debug)]
pub struct CacheKey {
    lifetime: Lifetime,
    tag: &'static str,
    digest: String,
}

impl CacheKey {
    /// `entry` must be a registry entry that goes into Redis (has a tag). The tag is written into the key name in plain
    /// text (so the debug panel and `redis-cli` show which cache it is); the version and the other distinguishing parts
    /// are digested (so no uid leaks).
    pub fn new(
        entry: &'static Entry,
        sql_revision: i64,
        variants: &impl Serialize,
    ) -> Result<Self> {
        let Some(tag) = entry.redis_tag else {
            anyhow::bail!(
                "cache {} is not stored in Redis and has no Redis key",
                entry.name
            );
        };
        ensure!(sql_revision >= 0, "cache revisions cannot be negative");
        let bytes = serde_json::to_vec(&(tag, sql_revision, variants))?;
        Ok(Self {
            lifetime: entry.lifetime,
            tag,
            digest: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    pub(crate) fn redis_key(&self, namespace: &str) -> String {
        format!("{namespace}:cache:v2:{}:{}", self.tag, self.digest)
    }
}

/// Marker for Redis failures. Attached to the anyhow error chain so the response layer returns 503 instead of a
/// generic 500.
#[derive(Debug)]
pub struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(crate::i18n::current().common_error_redis())
    }
}

impl std::error::Error for Unavailable {}

impl Cache {
    pub fn new(config: &RedisConfig) -> Result<Self> {
        config.validate()?;
        // A `rediss://` connection takes rustls's process-default crypto provider; see
        // [`crate::install_crypto_provider`].
        crate::install_crypto_provider();
        let client = config
            .enabled
            .then(|| {
                redis::Client::open(config.url.as_str()).map_err(|_| {
                    anyhow::anyhow!("cannot parse the Redis connection settings; check redis.url")
                })
            })
            .transpose()?;
        Ok(Self {
            inner: Arc::new(CacheInner {
                client,
                connection: Mutex::new(ConnectionSlot::default()),
                config: config.clone(),
                fills: std::sync::Mutex::default(),
            }),
        })
    }

    /// Must be called at startup, so an app with an old or unavailable Redis never enters service.
    ///
    /// Skipped when `redis.enabled = false` — single-machine single-process deployments and local profiling both take
    /// this path, see [`Cache::enabled`].
    pub async fn connect(config: &RedisConfig) -> Result<Self> {
        let cache = Self::new(config)?;
        cache.ensure_available().await?;
        Ok(cache)
    }

    /// Whether Redis takes part in this run.
    ///
    /// When disabled there is no connection; every cache read misses and every write is dropped, so business logic
    /// always takes the full MySQL path; rate limiting uses in-process counters.
    ///
    /// **This is a deployment-shape switch**: off for single-machine single-process, on for multi-process or
    /// multi-machine multi-process. Without cross-process state, the in-process cache and in-process rate limiting are
    /// correct implementations; turning it off locally to count SQL queries and time them is a special case of the
    /// same shape. When enabled, rate limiting uses a Redis sliding log ([`crate::rate_limit`]), so write requests
    /// depend on it too.
    ///
    /// **Once enabled, Redis is a hard dependency**: the default is true, the connection and version are verified at
    /// startup, and if it becomes unavailable at runtime the response is 503 — no degrading, no fall-through.
    pub fn enabled(&self) -> bool {
        self.inner.client.is_some()
    }

    pub fn namespace(&self) -> &str {
        &self.inner.config.namespace
    }

    /// Attaches this request's lifetimes (taken from the site settings).
    pub fn timed(&self, lifetimes: Lifetimes) -> TimedCache {
        TimedCache {
            cache: self.clone(),
            lifetimes,
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.inner.config.timeout_milliseconds)
    }

    async fn connection(&self) -> Result<(u64, MultiplexedConnection)> {
        // A failed generation is rebuilt by only one request. Waiting for the lock, connecting, authenticating and
        // checking the version are all bounded by the caller's timeout.
        let mut slot = self.inner.connection.lock().await;
        if let Some(connection) = &slot.connection {
            return Ok((slot.generation, connection.clone()));
        }
        let client = self
            .inner
            .client
            .as_ref()
            .context("Redis is disabled; no connection should be opened here")?;
        // Keep the original error in the chain: the response layer and the logs take the full text from the chain,
        // otherwise all that is left is the single "connection failed" line.
        let mut connection = client.get_multiplexed_async_connection().await.context(
            "Redis connection failed; check the service, authentication and network settings",
        )?;
        let info: String = redis::cmd("INFO")
            .arg("server")
            .query_async(&mut connection)
            .await
            .context(
                "cannot read the Redis version (the connection needs permission for INFO server)",
            )?;
        validate_version(&info)?;
        slot.generation = slot.generation.wrapping_add(1);
        slot.connection = Some(connection.clone());
        Ok((slot.generation, connection))
    }

    async fn invalidate_connection(&self, generation: u64) {
        let mut slot = self.inner.connection.lock().await;
        // An error from a concurrent older request must not discard a healthy connection another request just
        // established.
        if slot.generation == generation {
            slot.connection = None;
        }
    }

    async fn command<T: FromRedisValue>(&self, command: &redis::Cmd) -> Result<T> {
        crate::metrics::count_redis();
        let profile = crate::debug::trace::current();
        let started = std::time::Instant::now();
        let result = self.dispatch(command).await;
        if let Some(profile) = profile {
            let (name, key) = describe(command);
            let outcome = match &result {
                Ok(_) => trace::RedisOutcome::Ok,
                // The whole error chain, with the password in the connection string redacted.
                Err(error) => trace::RedisOutcome::Failed(crate::debug::visible_error(error)),
            };
            profile.record_redis(&name, &key, started.elapsed(), outcome);
        }
        // Connection, timeout and command failures are all marked here; the response layer only recognizes this one
        // marker to return 503.
        result.map_err(|error| error.context(Unavailable))
    }

    async fn dispatch<T: FromRedisValue>(&self, command: &redis::Cmd) -> Result<T> {
        let (generation, mut connection) = tokio::time::timeout(self.timeout(), self.connection())
            .await
            .map_err(|_| anyhow::anyhow!("Redis connection timed out"))??;
        match tokio::time::timeout(self.timeout(), command.query_async(&mut connection)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                if connection_broken(&error) {
                    self.invalidate_connection(generation).await;
                }
                Err(anyhow::Error::new(error).context("Redis command failed"))
            }
            Err(_) => {
                // A timed-out request may still be stuck on this multiplexed connection; switch to a new one.
                self.invalidate_connection(generation).await;
                Err(anyhow::anyhow!("Redis request timed out"))
            }
        }
    }

    /// Checked at startup and before each background batch; after a failure, business must not fall back to MySQL and
    /// continue. The request path does not call it, see [`protect`].
    ///
    /// The only exception is `redis.enabled = false`: that is an explicit switch-off, not unavailability.
    pub async fn ensure_available(&self) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let result: String = self.command(&redis::cmd("PING")).await?;
        ensure!(result == "PONG", "unexpected Redis health check answer");
        Ok(())
    }

    fn namespaced_key(&self, key: &CacheKey) -> String {
        key.redis_key(self.namespace())
    }

    /// Returns an error carrying the [`Unavailable`] marker when Redis itself fails; a value that cannot be decoded is
    /// treated as a miss.
    pub async fn get_json<T: DeserializeOwned>(&self, key: &CacheKey) -> Result<Option<T>> {
        if !self.enabled() {
            // Always a miss, so the caller fills from the source as usual; together with set_json dropping writes this
            // gives a full fall-through.
            return Ok(None);
        }
        let name = self.namespaced_key(key);
        let value: Option<Vec<u8>> = self.command(redis::cmd("GET").arg(&name)).await?;
        let decoded = value.map(|bytes| serde_json::from_slice::<T>(&bytes));
        if let Some(profile) = trace::current() {
            profile.refine_last_redis(if matches!(decoded, Some(Ok(_))) {
                trace::RedisOutcome::Hit
            } else {
                trace::RedisOutcome::Miss
            });
        }
        match decoded {
            None => Ok(None),
            Some(Ok(value)) => Ok(Some(value)),
            // The deployment changed the cache structure, or old and new processes wrote to each other's keys during a
            // rolling release, so the key holds an old format that cannot be decoded. The cache is rebuildable: treat
            // it as a miss, fill from the source and delete the key, rather than let anonymous pages return 500 until
            // the TTL expires.
            Some(Err(error)) => {
                tracing::warn!(%error, key = %name, tag = key.tag, "Redis cache value does not match the current format; treating it as a miss and deleting the key");
                self.command::<u64>(redis::cmd("UNLINK").arg(&name)).await?;
                Ok(None)
            }
        }
    }

    pub(crate) async fn delete(&self, key: &CacheKey) -> Result<()> {
        if self.enabled() {
            self.command::<u64>(redis::cmd("DEL").arg(self.namespaced_key(key)))
                .await?;
        }
        Ok(())
    }

    pub async fn set_json<T: Serialize>(
        &self,
        key: &CacheKey,
        value: &T,
        lifetimes: Lifetimes,
    ) -> Result<()> {
        if !self.enabled() || lifetimes.of(key.lifetime) == 0 {
            return Ok(());
        }
        let bytes = serde_json::to_vec(value).context("cannot serialize cache data")?;
        self.command::<()>(
            redis::cmd("SET")
                .arg(self.namespaced_key(key))
                .arg(bytes)
                .arg("EX")
                .arg(lifetimes.of(key.lifetime)),
        )
        .await
    }

    /// Reads the cache first; on a miss, fills from the source and writes back.
    ///
    /// At the moment a cache entry expires, requests arriving together all miss and all run the source query together
    /// (cache stampede). Here only one request per key in the same process fills from the source; the others queue and
    /// read the cache again once it has written.
    /// With several processes, each process fills once. A failed fill does not write the cache; the next queued request
    /// tries again itself.
    ///
    /// With Redis disabled there is no cache to read, and queueing would only serialize requests, so it fills directly.
    pub async fn get_or_fill_json<T, F, Fut>(
        &self,
        key: &CacheKey,
        lifetimes: Lifetimes,
        fill: F,
    ) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if !self.enabled() || lifetimes.of(key.lifetime) == 0 {
            return fill().await;
        }
        if let Some(value) = self.get_json(key).await? {
            return Ok(value);
        }
        let registration = FillRegistration::new(&self.inner.fills, &key.digest);
        let _turn = registration.gate.lock().await;
        if let Some(value) = self.get_json(key).await? {
            return Ok(value);
        }
        let value = fill().await?;
        self.set_json(key, &value, lifetimes).await?;
        Ok(value)
    }

    /// Runs a Lua script (rate limiting, [`crate::rate_limit`]). Uses the same connection and timeout as cache reads
    /// and writes, and counts toward this request's Redis count and the debug panel; failures likewise carry the
    /// [`Unavailable`] marker and the response layer turns them into 503.
    ///
    /// Always sends the full `EVAL` text rather than `EVALSHA`: the script is a few hundred bytes, which buys a single
    /// round trip and no `NOSCRIPT` resend handling.
    pub async fn eval<T: FromRedisValue>(
        &self,
        script: &str,
        keys: &[String],
        args: &[String],
    ) -> Result<T> {
        ensure!(self.enabled(), "Redis is disabled; no script should run");
        let mut command = redis::cmd("EVAL");
        command.arg(script).arg(keys.len()).arg(keys).arg(args);
        self.command(&command).await
    }

    /// Invalidation broadcast for the process cache ([`super::process`]). Counts toward this request's Redis count and
    /// the debug panel.
    pub async fn publish(&self, channel: &str, message: &str) -> Result<u64> {
        ensure!(
            self.enabled(),
            "Redis is disabled; nothing should be published"
        );
        self.command(redis::cmd("PUBLISH").arg(channel).arg(message))
            .await
    }

    /// Opens a separate subscription connection and subscribes to `channel`. Connecting, version checking and
    /// subscribing are all bounded by `timeout_milliseconds`.
    /// The subscription connection is not part of any request and is not counted in the footer or the debug panel.
    /// One subscription connection subscribes to several channels (the process cache invalidation channel and this
    /// process's heartbeat channel).
    pub async fn subscribe(&self, channels: &[&str]) -> Result<redis::aio::PubSub> {
        let client = self
            .inner
            .client
            .as_ref()
            .context("Redis is disabled; no subscription connection should be opened")?;
        let timeout = self.timeout();
        let subscribe = async {
            let mut pubsub = client
                .get_async_pubsub()
                .await
                .context("Redis subscription connection failed; check the service, authentication and network settings")?;
            for channel in channels {
                pubsub
                    .subscribe(*channel)
                    .await
                    .context("Redis SUBSCRIBE failed")?;
            }
            anyhow::Ok(pubsub)
        };
        tokio::time::timeout(timeout, subscribe)
            .await
            .map_err(|_| anyhow::anyhow!("Redis subscription connection timed out"))?
    }

    /// Test cleanup scans only the current isolated namespace and never uses FLUSHDB.
    pub async fn clear_namespace(&self) -> Result<()> {
        if !self.enabled() {
            // No key was ever written, and no connection is available.
            return Ok(());
        }
        ensure!(
            self.namespace().starts_with("gonof_test_"),
            "bulk cleanup is only allowed for isolated test namespaces"
        );
        let mut cursor = 0_u64;
        loop {
            let (next, keys): (u64, Vec<String>) = self
                .command(
                    redis::cmd("SCAN")
                        .arg(cursor)
                        .arg("MATCH")
                        .arg(format!("{}:*", self.namespace()))
                        .arg("COUNT")
                        .arg(100),
                )
                .await?;
            if !keys.is_empty() {
                self.command::<u64>(redis::cmd("UNLINK").arg(keys)).await?;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }
}

/// Whether this connection is still usable.
///
/// A server error reply (OOM, WRONGTYPE, READONLY…) means the request arrived and the reply was fully read, so the
/// connection itself is fine; discarding it would only make later requests reconnect and redo the version check for
/// nothing. Only read/write failures, protocol parse failures and authentication failures mean the connection is
/// broken. To the caller both cases are [`Unavailable`] (503); this only decides whether the connection is kept.
fn connection_broken(error: &redis::RedisError) -> bool {
    error.is_io_error() || error.is_unrecoverable_error()
}

/// Takes only the command name and the first argument: a SET value is a whole HTML page and must not go into the debug
/// panel. `EVAL` skips the full script text and lists its keys.
fn describe(command: &redis::Cmd) -> (String, String) {
    let mut args = command.args_iter().filter_map(|arg| match arg {
        redis::Arg::Simple(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        redis::Arg::Cursor => None,
    });
    let name = args.next().unwrap_or_default();
    let key = if name.eq_ignore_ascii_case("PING") {
        String::new()
    } else if name.eq_ignore_ascii_case("EVAL") {
        let _script = args.next();
        let count = args
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        args.take(count).collect::<Vec<_>>().join(" ")
    } else {
        args.next().unwrap_or_default()
    };
    (name, key)
}

fn validate_version(info: &str) -> Result<()> {
    let version = info
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .context("the Redis server returned no version")?;
    let major = version
        .trim()
        .split('.')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .context("invalid Redis version format")?;
    ensure!(major >= 7, "Redis 7.0 or later is required");
    Ok(())
}

/// Lets handlers take the cache handle.
///
/// **Once enabled, Redis is a hard dependency** — the connection and version are verified at startup, and if it is
/// unavailable at runtime the response is 503. What is removed here is the **request-level gate**, not that
/// dependency:
///
/// - Every place that uses Redis attaches the [`Unavailable`] marker when that real call fails, and the response layer
///   returns 503, with no probing beforehand: reading the cache works this way, and so does rate limiting
///   ([`crate::rate_limit`]; write requests such as login, password recovery, private messages and interactions pass
///   it before writing to the database) — when rate limiting is unavailable the request is not let through, so the
///   database write never happens
/// - Cache writes happen after the SQL commit; probing ahead of write requests buys no protection and would instead
///   report writes that already committed successfully as failures
/// - There is a time gap between the probe and the command, so a gate cannot stop the real races anyway
pub fn protect(router: Router, cache: Cache) -> Router {
    router.layer(Extension(cache))
}

pub fn unavailable() -> Response {
    unavailable_message(format!(
        "{}\n",
        crate::i18n::current().common_error_unavailable()
    ))
}

/// A Redis failure passes the original error straight through: without this sentence you would have to dig through the
/// server logs.
pub fn unavailable_detail(detail: &str) -> Response {
    unavailable_message(format!("{detail}\n"))
}

fn unavailable_message(message: String) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::RETRY_AFTER, "3"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        message,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_fillers_and_waiters_releases_the_registry_entry() {
        use std::{
            future::Future,
            task::{Context, Waker},
        };
        let mut cx = Context::from_waker(Waker::noop());
        let registry = FillRegistry::default();
        let owner = FillRegistration::new(&registry, "key");
        let turn = owner.gate.lock().await;
        let wait = async {
            let waiter = FillRegistration::new(&registry, "key");
            let _turn = waiter.gate.lock().await;
        };
        let mut wait = Box::pin(wait);
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        drop(wait);
        assert_eq!(registry.lock().unwrap()["key"].users, 1);
        drop(turn);
        drop(owner);
        assert!(registry.lock().unwrap().is_empty());

        let fill = async {
            let owner = FillRegistration::new(&registry, "other");
            let _turn = owner.gate.lock().await;
            std::future::pending::<()>().await;
        };
        let mut fill = Box::pin(fill);
        assert!(fill.as_mut().poll(&mut cx).is_pending());
        drop(fill);
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn requires_redis_seven_or_newer() {
        for version in ["6.2.20", "", "unknown"] {
            assert!(validate_version(&format!("redis_version:{version}\r\n")).is_err());
        }
        for version in ["7.0.0", "7.4.1", "8.8.0"] {
            validate_version(&format!("# Server\r\nredis_version:{version}\r\n")).unwrap();
        }
    }

    /// The debug panel lists only the keys of `EVAL`, not the full script text.
    #[test]
    fn eval_descriptions_list_keys_not_the_script() {
        let mut command = redis::cmd("EVAL");
        command
            .arg("return 0")
            .arg(2)
            .arg(&["ns:ratelimit:a:1", "ns:ratelimit:b:2"])
            .arg(&["allow", "0000beef", "5", "60"]);
        assert_eq!(
            describe(&command),
            ("EVAL".into(), "ns:ratelimit:a:1 ns:ratelimit:b:2".into())
        );
    }

    /// A `rediss://` connection builds its rustls client configuration from the process-default crypto provider, and
    /// with two backends in the dependency tree rustls panics when none is installed. The listener stands in for a
    /// TLS Redis: the client must get as far as sending a ClientHello, and the closed connection must come back as an
    /// ordinary error. This test needs neither Redis nor a database.
    #[tokio::test]
    async fn tls_connections_start_a_handshake_instead_of_panicking() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut first = [0u8; 1];
            socket.read_exact(&mut first).await.unwrap();
            // Dropping the socket ends the handshake, so the client fails at once instead of at its timeout.
            first[0]
        });
        let config = RedisConfig {
            url: format!("rediss://127.0.0.1:{port}/0"),
            ..RedisConfig::default()
        };
        let Err(error) = Cache::connect(&config).await else {
            panic!("the handshake cannot succeed");
        };
        assert!(error.downcast_ref::<Unavailable>().is_some(), "{error:#}");
        let first = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the client never connected")
            .unwrap();
        // 0x16 is the TLS handshake record type, the first byte of a ClientHello.
        assert_eq!(first, 0x16);
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    /// When disabled, Redis must not be touched at all: no connection, the gate lets requests through, reads miss,
    /// writes are dropped, cleanup is a no-op.
    /// This test needs neither Redis nor a database.
    #[tokio::test]
    async fn disabled_cache_never_touches_redis() {
        let config = RedisConfig {
            enabled: false,
            // The connection string is left empty and invalid, to prove that nothing is validated or parsed when
            // disabled.
            url: String::new(),
            ..RedisConfig::default()
        };
        let cache = Cache::connect(&config).await.unwrap();
        assert!(!cache.enabled());
        cache.ensure_available().await.unwrap();
        let key = CacheKey::new(&super::super::registry::FLOORS, 1, &("flat", 1)).unwrap();
        let lifetimes = Lifetimes {
            member: 30,
            page: 60,
            list: 60,
            statistics: 600,
            forum_statistics: 600,
        };
        cache.set_json(&key, &"ignored", lifetimes).await.unwrap();
        assert!(cache.get_json::<String>(&key).await.unwrap().is_none());
        cache.clear_namespace().await.unwrap();
    }

    #[test]
    fn cache_keys_separate_versions_and_response_variants() {
        use super::super::registry::{FEED_ITEMS, FLOORS, RENDERED_BODIES};
        let key = |entry, revision, variant| {
            CacheKey::new(entry, revision, &variant)
                .unwrap()
                .redis_key("ns")
        };
        assert_ne!(
            key(&FLOORS, 1, ("flat", 1, false)),
            key(&FLOORS, 2, ("flat", 1, false))
        );
        assert_ne!(
            key(&FLOORS, 1, ("flat", 1, false)),
            key(&FLOORS, 1, ("flat", 1, true))
        );
        assert_ne!(
            key(&FLOORS, 1, ("flat", 1, false)),
            key(&RENDERED_BODIES, 1, ("flat", 1, false)),
            "two caches with the floor lifetime do not share keys"
        );
        assert_ne!(
            key(&FLOORS, 1, ("flat", 1, false)),
            key(&FEED_ITEMS, 1, ("flat", 1, false))
        );
        assert_ne!(
            key(&FLOORS, 1, ("flat", 1, false)),
            key(&FLOORS, 1, ("nested", 1, false))
        );
        assert!(CacheKey::new(&FLOORS, -1, &()).is_err());
    }

    #[test]
    fn lifetimes_follow_settings_and_bucket_statistics() {
        let lifetimes = Lifetimes {
            member: 30,
            page: 60,
            list: 30,
            statistics: 600,
            forum_statistics: 300,
        };
        assert_eq!(lifetimes.of(Lifetime::Page), 60);
        assert_eq!(lifetimes.of(Lifetime::List), 30);
        assert_eq!(lifetimes.of(Lifetime::Member), 30);
        assert_eq!(lifetimes.of(Lifetime::Process), 0);
        assert_eq!(lifetimes.of(Lifetime::Statistics), 600);
        assert_eq!(lifetimes.of(Lifetime::ForumStatistics), 300);
        assert_eq!(lifetimes.of(Lifetime::Capacity), 0);
        assert_eq!(lifetimes.statistics_bucket(1199), 1);
        assert_eq!(lifetimes.statistics_bucket(1200), 2);
        assert_eq!(lifetimes.forum_statistics_bucket(1199), 3);
        assert_eq!(lifetimes.forum_statistics_bucket(1200), 4);
    }

    #[test]
    fn member_frame_lifetime_is_capped_below_the_process_ttl() {
        let mut settings = crate::runtime::settings::tests::seeded();
        for (ttl, member) in [(3600, MEMBER_MAX_SECONDS), (120, 120), (0, 0)] {
            settings.cache.ttl_seconds = ttl;
            assert_eq!(Lifetimes::from_settings(&settings).member, member, "{ttl}");
        }
    }
}
