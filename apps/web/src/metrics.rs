//! Per-request timing and query counts, for the footer line `19.1 ms · MySQL 11 · Redis 23` (`SQLite` on SQLite
//! sites; the Redis count only when Redis is enabled).
//!
//! This is separate from the SQL panel in [`crate::debug`]: that one records full statements and parameters and only
//! collects when the debug panel is turned on in the admin area (`settings.debug.panel`); this one is just a timestamp
//! and two counters, always on (whether the footer shows them depends on `settings.display.footer_metrics`), and each
//! query costs one task-local lookup plus one relaxed increment.
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

tokio::task_local! {
    static REQUEST_METRICS: Option<Arc<RequestMetrics>>;
}

#[derive(Debug)]
pub struct RequestMetrics {
    started: Instant,
    sql: AtomicUsize,
    redis: AtomicUsize,
}

impl Default for RequestMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestMetrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            sql: AtomicUsize::new(0),
            redis: AtomicUsize::new(0),
        }
    }

    /// Counted from when the request entered the middleware. The footer is evaluated at the end of the template, so it
    /// already includes rendering the whole body.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn sql(&self) -> usize {
        self.sql.load(Ordering::Relaxed)
    }

    pub fn redis(&self) -> usize {
        self.redis.load(Ordering::Relaxed)
    }
}

/// Counts one SQL statement (MySQL or SQLite). Outside a request (background tasks, tests) there is no context, so it
/// is silently skipped.
pub fn count_sql() {
    let _ = REQUEST_METRICS.try_with(|metrics| {
        if let Some(metrics) = metrics {
            metrics.sql.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Counts one Redis command — the footer should reflect the real number of round trips.
pub fn count_redis() {
    let _ = REQUEST_METRICS.try_with(|metrics| {
        if let Some(metrics) = metrics {
            metrics.redis.fetch_add(1, Ordering::Relaxed);
        }
    });
}

pub fn current() -> Option<Arc<RequestMetrics>> {
    REQUEST_METRICS.try_with(Clone::clone).ok().flatten()
}

pub async fn scope<F: Future>(metrics: Arc<RequestMetrics>, future: F) -> F::Output {
    REQUEST_METRICS.scope(Some(metrics), future).await
}

/// Always-on middleware: creates a set of counters for each request; static assets are not counted.
///
/// In the layer order it must wrap the identity and cache middleware — session queries are also a cost of this
/// request, and the footer should reflect that faithfully.
pub async fn middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.uri().path().starts_with("/static/") {
        return next.run(request).await;
    }
    // The time basis is, like the counters, cross-cutting per-request state, so they share this layer: the site
    // settings are written in afterwards by `SiteFrame::load`, and reads before that see the defaults.
    scope(
        Arc::new(RequestMetrics::new()),
        crate::clock::scope(next.run(request)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_only_inside_a_request_scope() {
        // Outside the scope it must not panic, nor be counted against another request.
        count_sql();
        count_redis();

        let metrics = Arc::new(RequestMetrics::new());
        scope(metrics.clone(), async {
            count_sql();
            count_sql();
            count_redis();
            assert_eq!(current().unwrap().sql(), 2);
        })
        .await;
        assert_eq!(metrics.sql(), 2);
        assert_eq!(metrics.redis(), 1);
        assert!(metrics.elapsed() < Duration::from_secs(5));
    }
}
