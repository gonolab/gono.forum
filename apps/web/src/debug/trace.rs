//! Tokio task-local query profiles. A profile exists only for an opted-in HTTP request.
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::db::{
    Backend,
    sql_display::{SqlValue, expand_sql_for},
};
use tracing::Instrument;

tokio::task_local! {
    static REQUEST_PROFILE: Option<Arc<RequestProfile>>;
}

#[derive(Clone, Debug)]
pub struct QueryRecord {
    pub source: QuerySource,
    /// The backend that executes it: parameters are expanded as this backend's literals, and the execution plan is
    /// also fetched from it.
    pub backend: Backend,
    pub sequence: usize,
    pub sql: String,
    pub parameters: Vec<SqlValue>,
    pub expanded_sql: String,
    pub expansion_error: Option<String>,
    pub elapsed: Duration,
    pub rows_returned: u64,
    pub rows_affected: Option<u64>,
    pub error: Option<String>,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuerySource {
    Application,
    /// A batch from the SQLx driver log: timing is measured, but the log has no success/failure result.
    Driver,
}

/// One Redis round trip. Only the command name and key are recorded: the value of a SET is a whole page of HTML and
/// does not belong in the panel.
#[derive(Clone, Debug)]
pub struct RedisRecord {
    pub sequence: usize,
    pub command: String,
    pub key: String,
    pub elapsed: Duration,
    pub outcome: RedisOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RedisOutcome {
    Hit,
    Miss,
    Ok,
    Failed(String),
}

/// One read of the in-process global cache ([`crate::cache`]). Blocks that hit send no SQL; the debug panel uses this to
/// mark which data came from the in-process cache.
#[derive(Clone, Debug)]
pub struct BlockRecord {
    pub sequence: usize,
    pub block: String,
    pub outcome: BlockOutcome,
    /// On a hit: how long ago the in-memory copy was loaded.
    pub age: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockOutcome {
    /// Present in memory and not expired; the database was not queried.
    Hit,
    /// Missing or expired; this request went back to the database and loaded it.
    Filled,
    /// The lifetime is 0 (`settings.cache.ttl_seconds`); read from the database and not loaded.
    Uncached,
}

#[derive(Debug, Default)]
pub struct RequestProfile {
    records: Mutex<Vec<QueryRecord>>,
    redis: Mutex<Vec<RedisRecord>>,
    blocks: Mutex<Vec<BlockRecord>>,
}

impl RequestProfile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<QueryRecord> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn redis_snapshot(&self) -> Vec<RedisRecord> {
        self.redis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn block_snapshot(&self) -> Vec<BlockRecord> {
        self.blocks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Called by [`crate::cache`] after every block read.
    pub fn record_block(&self, block: &str, outcome: BlockOutcome, age: Option<Duration>) {
        let mut blocks = self
            .blocks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = blocks.len() + 1;
        blocks.push(BlockRecord {
            sequence,
            block: block.to_owned(),
            outcome,
            age,
        });
    }

    /// Rewrites the last record as hit/miss.
    ///
    /// `command` only sees the generic return value and cannot tell whether a GET hit; `get_json` fills it in after
    /// getting the `Option<Vec<u8>>`. Cache calls within a request are all awaited sequentially, so the last record is
    /// the one just made.
    pub fn refine_last_redis(&self, outcome: RedisOutcome) {
        if let Some(last) = self
            .redis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_mut()
        {
            last.outcome = outcome;
        }
    }

    /// Called by [`crate::cache::Cache`] after every command returns.
    pub fn record_redis(&self, command: &str, key: &str, elapsed: Duration, outcome: RedisOutcome) {
        let mut redis = self
            .redis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = redis.len() + 1;
        redis.push(RedisRecord {
            sequence,
            command: command.to_owned(),
            key: key.to_owned(),
            elapsed,
            outcome,
        });
    }

    #[cfg(test)]
    pub(crate) fn start(self: &Arc<Self>, sql: &str) -> QueryGuard {
        self.start_on(Backend::MySql, sql)
    }

    pub(crate) fn start_on(self: &Arc<Self>, backend: Backend, sql: &str) -> QueryGuard {
        let started = Instant::now();
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = records.len();
        let record = QueryRecord {
            source: QuerySource::Application,
            backend,
            sequence: index + 1,
            sql: sql.to_owned(),
            parameters: Vec::new(),
            expanded_sql: sql.to_owned(),
            expansion_error: None,
            elapsed: Duration::ZERO,
            rows_returned: 0,
            rows_affected: None,
            error: None,
            cancelled: false,
        };
        // Reserve by start order: connection initialization and concurrent calls may finish first.
        records.push(record.clone());
        QueryGuard {
            profile: Arc::clone(self),
            index,
            started,
            record: Some(record),
        }
    }

    pub(crate) fn record_driver(
        &self,
        backend: Backend,
        sql: String,
        elapsed: Duration,
        rows_returned: u64,
        rows_affected: Option<u64>,
    ) {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = records.len() + 1;
        records.push(QueryRecord {
            source: QuerySource::Driver,
            backend,
            sequence,
            expanded_sql: sql.clone(),
            sql,
            parameters: Vec::new(),
            expansion_error: None,
            elapsed,
            rows_returned,
            rows_affected,
            // The SQLx logger provides no result; the UI must show it per source as "no result" (`debug-driver-no-result`), not as success.
            error: None,
            cancelled: false,
        });
    }
}

pub async fn scope<F: Future>(profile: Arc<RequestProfile>, future: F) -> F::Output {
    REQUEST_PROFILE
        .scope(Some(profile), async move {
            let span = tracing::debug_span!(target: "gonof::debug", "gonof_debug_request");
            future.instrument(span).await
        })
        .await
}

pub fn current() -> Option<Arc<RequestProfile>> {
    REQUEST_PROFILE.try_with(Clone::clone).ok().flatten()
}

/// Diagnostic queries must not become application SQL records or recursively explain themselves.
pub async fn without<F: Future>(future: F) -> F::Output {
    REQUEST_PROFILE.scope(None, future).await
}

pub(crate) struct QueryGuard {
    profile: Arc<RequestProfile>,
    index: usize,
    started: Instant,
    record: Option<QueryRecord>,
}

impl QueryGuard {
    pub(crate) fn parameters(&mut self, parameters: Result<Vec<SqlValue>, String>) {
        let record = self.record.as_mut().expect("unfinished query");
        match parameters {
            Ok(parameters) => {
                match expand_sql_for(record.backend, &record.sql, &parameters) {
                    Ok(sql) => record.expanded_sql = sql,
                    Err(error) => record.expansion_error = Some(error),
                }
                record.parameters = parameters;
            }
            Err(error) => record.expansion_error = Some(error),
        }
    }

    pub(crate) fn returned(&mut self) {
        let record = self.record.as_mut().expect("unfinished query");
        record.rows_returned = record.rows_returned.saturating_add(1);
    }

    pub(crate) fn affected(&mut self, count: u64) {
        let record = self.record.as_mut().expect("unfinished query");
        record.rows_affected = Some(record.rows_affected.unwrap_or(0).saturating_add(count));
    }

    pub(crate) fn finish(mut self, error: Option<String>) {
        let record = self.record.as_mut().expect("unfinished query");
        record.error = error;
        self.publish(false);
    }

    fn publish(&mut self, cancelled: bool) {
        if let Some(mut record) = self.record.take() {
            record.elapsed = self.started.elapsed();
            record.cancelled = cancelled;
            self.profile
                .records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())[self.index] = record;
        }
    }
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        self.publish(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nested_and_spawned_tasks_do_not_leak_profiles() {
        let outer = Arc::new(RequestProfile::new());
        let inner = Arc::new(RequestProfile::new());
        assert!(current().is_none());
        scope(Arc::clone(&outer), async {
            assert!(Arc::ptr_eq(&current().unwrap(), &outer));
            assert!(tokio::spawn(async { current().is_none() }).await.unwrap());
            without(async {
                assert!(current().is_none());
            })
            .await;
            assert!(Arc::ptr_eq(&current().unwrap(), &outer));
            scope(Arc::clone(&inner), async {
                assert!(Arc::ptr_eq(&current().unwrap(), &inner));
            })
            .await;
            assert!(Arc::ptr_eq(&current().unwrap(), &outer));
        })
        .await;
        assert!(current().is_none());
    }

    #[test]
    fn records_preserve_start_order_and_drop_marks_cancellation() {
        let profile = Arc::new(RequestProfile::new());
        let first = profile.start("SELECT 1");
        let second = profile.start("SELECT 2");
        second.finish(None);
        drop(first);
        let records = profile.snapshot();
        assert_eq!(
            records.iter().map(|row| row.sequence).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(records[0].cancelled);
        assert!(!records[1].cancelled);
    }

    #[test]
    fn failed_queries_preserve_parameters_rows_and_error_without_cancellation() {
        let profile = Arc::new(RequestProfile::new());
        let mut query = profile.start("SELECT ?, ?");
        query.parameters(Ok(vec![
            SqlValue::Text("Coins'\\\r\n".into()),
            SqlValue::Unsigned(u64::MAX),
        ]));
        query.returned();
        query.affected(2);
        query.affected(3);
        query.finish(Some("the database returned an error".into()));
        let record = &profile.snapshot()[0];
        assert_eq!(
            record.error.as_deref(),
            Some("the database returned an error")
        );
        assert!(!record.cancelled);
        assert_eq!(record.rows_returned, 1);
        assert_eq!(record.rows_affected, Some(5));
        assert_eq!(record.parameters.len(), 2);
        assert!(record.expansion_error.is_none());
        assert!(record.expanded_sql.contains("CONVERT(X'"));
        assert!(record.expanded_sql.ends_with("18446744073709551615"));
    }

    #[test]
    fn expansion_failure_keeps_original_sql_and_available_values() {
        let profile = Arc::new(RequestProfile::new());
        let mut mismatched = profile.start("SELECT ?, ?");
        mismatched.parameters(Ok(vec![SqlValue::Integer(1)]));
        mismatched.finish(None);
        let mut unavailable = profile.start("SELECT ?");
        unavailable.parameters(Err("driver parameters are not visible".into()));
        unavailable.finish(None);
        let records = profile.snapshot();
        assert_eq!(records[0].expanded_sql, "SELECT ?, ?");
        assert_eq!(records[0].parameters, [SqlValue::Integer(1)]);
        assert!(records[0].expansion_error.is_some());
        assert_eq!(records[1].expanded_sql, "SELECT ?");
        assert!(records[1].parameters.is_empty());
        assert_eq!(
            records[1].expansion_error.as_deref(),
            Some("driver parameters are not visible")
        );
    }

    #[tokio::test]
    async fn concurrent_request_scopes_keep_records_separate() {
        let first = Arc::new(RequestProfile::new());
        let second = Arc::new(RequestProfile::new());
        let record = |profile: Arc<RequestProfile>, sql: &'static str| async move {
            scope(profile, async move {
                let guard = current().unwrap().start(sql);
                tokio::task::yield_now().await;
                guard.finish(None);
            })
            .await;
        };
        tokio::join!(
            record(first.clone(), "SELECT 1"),
            record(second.clone(), "SELECT 2")
        );
        assert_eq!(first.snapshot().len(), 1);
        assert_eq!(second.snapshot().len(), 1);
        assert_eq!(first.snapshot()[0].sql, "SELECT 1");
        assert_eq!(second.snapshot()[0].sql, "SELECT 2");
        assert!(current().is_none());
    }
}
