//! Rate limiting, branching by deployment shape:
//!
//! - `redis.enabled = false` (single machine, single process): in-process counters [`Local`]. With only one process,
//!   it is the global rate limit — a correct implementation, not a degradation.
//! - `redis.enabled = true` (multi-process, multi-machine multi-process): a Redis sliding log, key
//!   `{namespace}:ratelimit:{scope}:{key}`. One call is one Lua script and one round trip however many keys it has,
//!   over the cache's multiplexed connection ([`Cache::eval`]), counted in the footer's Redis count and the debug
//!   panel. **Redis is a hard dependency**: a failed call returns an error carrying the [`crate::cache::Unavailable`]
//!   marker, which the response layer turns into 503; it does not fall back to in-process counters and does not let
//!   the request through.
//!
//! Rules of the Redis shape:
//!
//! - Sliding log: every allowed request records one entry in a sorted set, scored by the microseconds of Redis `TIME`
//!   (machine clocks need not agree); once the number of entries in the window reaches the limit the request is
//!   rejected, and rejections are not recorded in the window.
//! - Checking several keys at once **short-circuits in order**, the same as in-process: keys are judged one by one in
//!   the given order, and the first full key rejects; entries already recorded before it are not withdrawn, and later
//!   keys are not judged.
//! - A key's expiry equals the scope's window and is renewed after every record; checking without recording
//!   ([`Limits::blocked`]) does not renew it.
//! - There is no per-scope key limit; keys are reclaimed by expiry.
//!
//! The two shapes use different window algorithms: in-process is a fixed window starting from the first count, Redis
//! is a sliding window.
use std::{collections::HashMap, fmt::Display, sync::Mutex};

use anyhow::Result;

use crate::cache::Cache;

/// One rate limit bucket: scope, key, limit within the window and window length in seconds. A limit of 0 always
/// rejects (the same in both shapes).
#[derive(Clone, Debug)]
pub struct Rule {
    scope: &'static str,
    key: String,
    limit: u32,
    window: i64,
}

impl Rule {
    pub fn new(scope: &'static str, key: impl Display, limit: u32, window: i64) -> Self {
        Self {
            scope,
            key: key.to_string(),
            limit,
            window,
        }
    }
}

/// The rate limit handle held by the application, choosing the implementation by `redis.enabled`.
pub struct Limits {
    local: Local,
    cache: Cache,
}

impl Limits {
    pub fn new(cache: Cache) -> Self {
        Self {
            local: Local::default(),
            cache,
        }
    }

    /// Judges and counts: returns `true` when all are allowed. Several keys short-circuit in order, see the module
    /// docs.
    pub async fn allow(&self, rules: &[Rule]) -> Result<bool> {
        if !self.cache.enabled() {
            return Ok(rules.iter().all(|rule| self.local.allow(rule)));
        }
        Ok(self
            .redis(
                Mode::Allow,
                rules,
                &format!("{:08x}", rand::random::<u32>()),
            )
            .await?
            == 0)
    }

    /// Only looks without counting: `true` when any key has already reached its limit in the current window.
    pub async fn blocked(&self, rules: &[Rule]) -> Result<bool> {
        if !self.cache.enabled() {
            return Ok(rules.iter().any(|rule| self.local.blocked(rule)));
        }
        Ok(self.redis(Mode::Blocked, rules, "").await? != 0)
    }

    /// Tries to take one slot in all failure buckets at once. Once a failure is confirmed, the slot is kept; on success
    /// or any other error the slot must be returned.
    /// When the request is aborted, Redis times out or the process exits, the slot is conservatively kept and expires
    /// with the window at the latest.
    pub async fn reserve_failure(&self, rules: Vec<Rule>) -> Result<Option<FailureAttempt<'_>>> {
        // Only one key is reserved per scope (for example one each for account and IP). Duplicate scopes are rejected,
        // so the capacity check and the writes correspond one to one.
        anyhow::ensure!(
            rules
                .iter()
                .enumerate()
                .all(|(i, rule)| rules[..i].iter().all(|other| rule.scope != other.scope)),
            "a failure limit reserved the same purpose twice"
        );
        let mark = if self.cache.enabled() {
            let token = format!("r-{:032x}", rand::random::<u128>());
            if self.redis(Mode::Reserve, &rules, &token).await? != 0 {
                return Ok(None);
            }
            AttemptMark::Redis(token)
        } else {
            let Some(deadlines) = self.local.reserve(&rules) else {
                return Ok(None);
            };
            AttemptMark::Local(deadlines)
        };
        Ok(Some(FailureAttempt {
            limits: self,
            rules,
            mark,
        }))
    }

    /// 0 means everything is allowed (or nothing is full); otherwise the number (from 1) of the first full key.
    async fn redis(&self, mode: Mode, rules: &[Rule], member: &str) -> Result<i64> {
        if rules.is_empty() {
            return Ok(0);
        }
        let namespace = self.cache.namespace();
        let keys: Vec<String> = rules
            .iter()
            .map(|rule| format!("{namespace}:ratelimit:{}:{}", rule.scope, rule.key))
            .collect();
        let mut args = vec![mode.as_str().to_owned(), member.to_owned()];
        for rule in rules {
            args.push(rule.limit.to_string());
            args.push(rule.window.max(1).to_string());
        }
        self.cache.eval(SLIDING_LOG, &keys, &args).await
    }
}

enum AttemptMark {
    Local(Vec<i64>),
    Redis(String),
}

/// Each request holds one; it can be consumed only once, so it never returns someone else's failure or a count in the
/// next window.
#[must_use]
pub struct FailureAttempt<'a> {
    limits: &'a Limits,
    rules: Vec<Rule>,
    mark: AttemptMark,
}

impl FailureAttempt<'_> {
    /// Authentication did not fail: return the slot reserved this time. Callers that may write to the database should
    /// return the slot before committing the business change.
    pub async fn release(self) -> Result<()> {
        match self.mark {
            AttemptMark::Local(deadlines) => self.limits.local.release(&self.rules, &deadlines),
            AttemptMark::Redis(token) => {
                self.limits
                    .redis(Mode::Release, &self.rules, &token)
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Allow,
    Reserve,
    Release,
    Blocked,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Reserve => "reserve",
            Self::Release => "release",
            Self::Blocked => "blocked",
        }
    }
}

/// Sliding log. KEYS are the buckets' keys; ARGV[1] is the mode (allow / blocked / reserve / release), ARGV[2] is a
/// random suffix for this call, followed by two items per key: limit and window seconds. Returns 0 or the number of
/// the first full key.
///
/// Numbers are always written as strings with `%.0f` before handing them to Redis: Lua 5.1 concatenates numbers with
/// `%.14g`, which loses precision on 16-digit microsecond timestamps.
/// Microsecond timestamps are below 2^53, so a `double` represents them exactly.
const SLIDING_LOG: &str = r"
local mode = ARGV[1]
if mode == 'release' then
  for _, key in ipairs(KEYS) do
    redis.call('ZREM', key, ARGV[2])
  end
  return 0
end
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000000 + tonumber(clock[2])
local stamp = string.format('%.0f', now)
local function window_floor(i)
  return string.format('%.0f', now - tonumber(ARGV[2 * i + 2]) * 1000000)
end
-- A failure is counted only when every bucket has room: when a later bucket is full, earlier ones are not charged either.
if mode == 'blocked' or mode == 'reserve' then
  for i, key in ipairs(KEYS) do
    if redis.call('ZCOUNT', key, '(' .. window_floor(i), '+inf') >= tonumber(ARGV[2 * i + 1]) then
      return i
    end
  end
  if mode == 'blocked' then return 0 end
end
local member = ARGV[2]
if mode == 'allow' then member = stamp .. '-' .. member end
for i, key in ipairs(KEYS) do
  redis.call('ZREMRANGEBYSCORE', key, '-inf', window_floor(i))
  if redis.call('ZCARD', key) >= tonumber(ARGV[2 * i + 1]) then return i end
  redis.call('ZADD', key, stamp, member)
  redis.call('PEXPIRE', key, string.format('%.0f', tonumber(ARGV[2 * i + 2]) * 1000))
end
return 0
";

/// Maximum number of keys recorded at the same time per scope.
const LIMIT_SCOPE_CAPACITY: usize = 10_000;

/// Counts within one scope: key → (window deadline, times used).
type LimitEntries = HashMap<String, (i64, u32)>;

/// In-process rate limiting, bucketed by scope (`scope`), each bucket bounded on its own. The window starts from the
/// first count.
///
/// When full, only new keys of this scope are rejected and still-valid records are not cleared; otherwise flooding it
/// could wipe out other people's counts.
/// Bucketing avoids collateral damage: with one table for the whole site, flooding password recovery with changing
/// e-mail addresses would make new keys for login, registration and the API get 429 too.
pub(crate) struct Local {
    capacity: usize,
    scopes: Mutex<HashMap<&'static str, LimitEntries>>,
}

impl Default for Local {
    fn default() -> Self {
        Self::with_capacity(LIMIT_SCOPE_CAPACITY)
    }
}

impl Local {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            scopes: Mutex::default(),
        }
    }

    pub(crate) fn allow(&self, rule: &Rule) -> bool {
        let now = crate::identity::now();
        let mut scopes = self.scopes.lock().expect("rate limit lock not poisoned");
        let entries = scopes.entry(rule.scope).or_default();
        entries.retain(|_, (until, _)| *until > now);
        if self.full(entries, rule) {
            return false;
        }
        let entry = entries
            .entry(rule.key.clone())
            .or_insert((now + rule.window, 0));
        entry.1 += 1;
        true
    }

    fn reserve(&self, rules: &[Rule]) -> Option<Vec<i64>> {
        let now = crate::identity::now();
        let mut scopes = self.scopes.lock().expect("rate limit lock not poisoned");
        for rule in rules {
            let entries = scopes.entry(rule.scope).or_default();
            entries.retain(|_, (until, _)| *until > now);
            if self.full(entries, rule) {
                return None;
            }
        }
        let mut deadlines = Vec::with_capacity(rules.len());
        for rule in rules {
            let entry = scopes
                .get_mut(rule.scope)
                .expect("scope already checked")
                .entry(rule.key.clone())
                .or_insert((now + rule.window, 0));
            entry.1 += 1;
            deadlines.push(entry.0);
        }
        Some(deadlines)
    }

    fn release(&self, rules: &[Rule], deadlines: &[i64]) {
        let mut scopes = self.scopes.lock().expect("rate limit lock not poisoned");
        for (rule, deadline) in rules.iter().zip(deadlines) {
            let entries = scopes.get_mut(rule.scope).expect("scope was reserved");
            if let Some((until, count)) = entries.get_mut(&rule.key)
                && until == deadline
            {
                *count -= 1;
                if *count == 0 {
                    entries.remove(&rule.key);
                }
            }
        }
    }

    fn full(&self, entries: &LimitEntries, rule: &Rule) -> bool {
        match entries.get(&rule.key) {
            Some((_, count)) => *count >= rule.limit,
            None => rule.limit == 0 || entries.len() >= self.capacity,
        }
    }

    pub(crate) fn blocked(&self, rule: &Rule) -> bool {
        // Consistent with Redis's `ZCOUNT >= 0`: a bucket with a limit of 0 counts as full even with nothing recorded.
        if rule.limit == 0 {
            return true;
        }
        let now = crate::identity::now();
        let mut scopes = self.scopes.lock().expect("rate limit lock not poisoned");
        let Some(entries) = scopes.get_mut(rule.scope) else {
            return false;
        };
        entries.retain(|_, (until, _)| *until > now);
        self.full(entries, rule)
    }
}

#[cfg(test)]
mod tests {
    use super::{Local, Rule};

    #[test]
    fn a_full_limit_scope_rejects_only_its_own_new_keys() {
        let limits = Local::with_capacity(3);
        for email in ["a", "b", "c"] {
            assert!(limits.allow(&Rule::new("forgot-email-long", email, 5, 3600)));
        }
        // A full bucket rejects new keys but does not clear existing counts, and existing keys keep counting as usual.
        assert!(!limits.allow(&Rule::new("forgot-email-long", "d", 5, 3600)));
        assert!(limits.blocked(&Rule::new("forgot-email-long", "d", 5, 3600)));
        assert!(!limits.blocked(&Rule::new("forgot-email-long", "a", 5, 3600)));
        assert!(limits.allow(&Rule::new("forgot-email-long", "a", 5, 3600)));
        // Other scopes are not affected: when the whole site shared one table, they would all get 429 here.
        assert!(limits.allow(&Rule::new("login-ip", "192.0.2.10", 30, 300)));
        assert!(limits.allow(&Rule::new("register", "192.0.2.10", 20, 3600)));
        assert!(!limits.blocked(&Rule::new("login-ip", "192.0.2.10", 30, 300)));
    }

    #[test]
    fn zero_limit_always_rejects() {
        let limits = Local::default();
        assert!(!limits.allow(&Rule::new("register", "192.0.2.10", 0, 3600)));
        assert!(limits.blocked(&Rule::new("register", "192.0.2.10", 0, 3600)));
    }
}
