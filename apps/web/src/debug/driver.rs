//! Supplements SQLx's internal connection-initialization logs without passing driver batches off as application
//! executor calls.
//!
//! SQLx 0.8.6 logs provide the full SQL, batch duration and row count, but no execution result or error.
//! This boundary is kept: SET batches are not split, per-statement durations are not estimated, and success is not
//! assumed.
use std::{fmt, sync::Arc, time::Duration};

use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id},
    subscriber::Interest,
};
use tracing_subscriber::{
    Layer,
    layer::{Context, Filter},
    registry::LookupSpan,
};

use super::trace::{self, RequestProfile};
use crate::db::Backend;

const SPAN_TARGET: &str = "gonof::debug";

pub struct DriverQueryLayer;

/// Filtered by its own layer; the console RUST_LOG must not be able to suppress SQLx events the page needs.
pub struct DriverFilter;

#[derive(Clone)]
struct QueryContext {
    profile: Arc<RequestProfile>,
    backend: Backend,
    sql: String,
}

impl<S> Filter<S> for DriverFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &Metadata<'_>, context: &Context<'_, S>) -> bool {
        if metadata.is_span() && metadata.target() == SPAN_TARGET {
            return trace::current().is_some();
        }
        metadata.is_event()
            && metadata.target() == "sqlx::query"
            && context.lookup_current().is_some_and(|span| {
                span.scope()
                    .any(|span| span.extensions().get::<QueryContext>().is_some())
            })
    }

    fn callsite_enabled(&self, metadata: &'static Metadata<'static>) -> Interest {
        if metadata.target() == SPAN_TARGET || metadata.target() == "sqlx::query" {
            // Whether a request exists depends on the current context, so this cannot be cached as a global always.
            Interest::sometimes()
        } else {
            Interest::never()
        }
    }
}

impl<S> Layer<S> for DriverQueryLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if attributes.metadata().target() != SPAN_TARGET
            || attributes.metadata().name() != "gonof_debug_query"
        {
            return;
        }
        let Some(profile) = trace::current() else {
            return;
        };
        let mut fields = QueryFields::default();
        attributes.record(&mut fields);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(QueryContext {
                profile,
                backend: if fields.database == Backend::Sqlite.label() {
                    Backend::Sqlite
                } else {
                    Backend::MySql
                },
                sql: fields.sql,
            });
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let Some(mut scope) = context.event_scope(event) else {
            return;
        };
        // The nearest executor span covers the outer pool query; after_connect's own queries must not be duplicated.
        let Some(span) = scope.find(|span| span.extensions().get::<QueryContext>().is_some())
        else {
            return;
        };
        let extensions = span.extensions();
        let Some(query) = extensions.get::<QueryContext>() else {
            return;
        };
        // An outer span can remain entered while diagnostics temporarily disable recording.
        // MySQL query events run in the caller task; require its current request explicitly.
        if !trace::current().is_some_and(|profile| Arc::ptr_eq(&profile, &query.profile)) {
            return;
        }
        let mut fields = DriverFields::default();
        event.record(&mut fields);
        let Some(elapsed) = fields
            .elapsed
            .and_then(|value| Duration::try_from_secs_f64(value).ok())
        else {
            return;
        };
        let sql = fields.full_sql();
        if sql == query.sql || !initialization_sql(&sql) {
            return;
        }
        query.profile.record_driver(
            query.backend,
            sql,
            elapsed,
            fields.rows_returned,
            fields.rows_affected,
        );
    }
}

/// The current SQLx MySQL connection initialization executes a parameterless SET batch.
fn initialization_sql(sql: &str) -> bool {
    sql.trim_start().starts_with("SET ")
}

pub(crate) fn query_span(backend: Backend, sql: &str) -> tracing::Span {
    tracing::debug_span!(target: "gonof::debug", "gonof_debug_query", sql, database = backend.label())
}

#[derive(Default)]
struct QueryFields {
    sql: String,
    database: String,
}

impl Visit for QueryFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "sql" => self.sql = value.to_owned(),
            "database" => self.database = value.to_owned(),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

#[derive(Default)]
struct DriverFields {
    statement: String,
    summary: String,
    elapsed: Option<f64>,
    rows_returned: u64,
    rows_affected: Option<u64>,
}

impl DriverFields {
    fn full_sql(&self) -> String {
        if self.statement.is_empty() {
            // SQLx omits db.statement only when the summary is exactly the same as the original SQL.
            self.summary.clone()
        } else {
            // Remove only the wrapping added by the SQLx logger, keeping the original SQL's whitespace and full content.
            self.statement
                .strip_prefix("\n\n")
                .and_then(|sql| sql.strip_suffix('\n'))
                .unwrap_or(&self.statement)
                .to_owned()
        }
    }
}

impl Visit for DriverFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "db.statement" => self.statement = value.to_owned(),
            "summary" => self.summary = value.to_owned(),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "rows_returned" => self.rows_returned = value,
            "rows_affected" => self.rows_affected = Some(value),
            _ => {}
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

#[cfg(test)]
pub(crate) fn install_for_tests() {
    use std::sync::Once;
    use tracing_subscriber::prelude::*;

    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_filter(tracing_subscriber::EnvFilter::new("off")),
            )
            .with(DriverQueryLayer.with_filter(DriverFilter))
            .init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_driver(sql: &str) {
        tracing::info!(
            target: "sqlx::query",
            summary = "SET …",
            db.statement = sql,
            elapsed_secs = 0.001_f64,
            rows_returned = 0_u64,
            rows_affected = 0_u64,
            "driver test query"
        );
    }

    #[tokio::test]
    async fn driver_capture_respects_diagnostic_scope_and_request_ownership() {
        install_for_tests();
        let profile = Arc::new(RequestProfile::new());
        let other = Arc::new(RequestProfile::new());
        trace::scope(profile.clone(), async {
            let span = query_span(Backend::MySql, "SELECT 1");
            let _entered = span.enter();
            emit_driver("SELECT 1");
            emit_driver("SET NAMES utf8mb4");
            trace::without(async { emit_driver("SET @diagnostic = 1") }).await;
            trace::scope(other.clone(), async { emit_driver("SET @other = 1") }).await;
        })
        .await;
        emit_driver("SET @outside = 1");
        let records = profile.snapshot();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sql, "SET NAMES utf8mb4");
        assert_eq!(records[0].source, trace::QuerySource::Driver);
        assert!(records[0].error.is_none());
        assert!(other.snapshot().is_empty());
    }

    #[test]
    fn only_mysql_initialization_batches_are_supplemented() {
        assert!(initialization_sql(
            " \nSET NAMES utf8mb4; SET time_zone = '+00:00'"
        ));
        assert!(!initialization_sql("SELECT 1"));
        assert!(!initialization_sql("RESET CONNECTION"));
    }

    #[test]
    fn recovers_full_sql_without_trimming_its_original_whitespace() {
        let fields = DriverFields {
            statement: "\n\n  SET NAMES utf8mb4; \n\n".into(),
            summary: "SET NAMES utf8mb4; …".into(),
            ..Default::default()
        };
        assert_eq!(fields.full_sql(), "  SET NAMES utf8mb4; \n");
        assert_eq!(
            DriverFields {
                summary: "SET time_zone".into(),
                ..Default::default()
            }
            .full_sql(),
            "SET time_zone"
        );
    }
}
