//! SQLx executor adapter (MySQL and SQLite): execution still uses the original bound arguments and
//! cached statement. The public `db::ObservedExecutor` marks a target as observed; `db::query` wraps the
//! backend connection in this adapter only for observed targets.
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{Stream, future::BoxFuture, stream::BoxStream};
use sqlx::{
    Arguments, Database, Describe, Either, Error, Execute, Executor, error::BoxDynError,
    mysql::MySqlQueryResult, sqlite::SqliteQueryResult,
};
use tracing::Instrument;

use crate::debug::{
    driver,
    trace::{self, QueryGuard},
};

use super::{Backend, sql_display::SqlValue};
use std::cell::RefCell;

#[derive(Debug)]
pub(super) struct Observed<E> {
    inner: E,
}

impl<E> Observed<E> {
    pub(super) fn new(inner: E) -> Self {
        Self { inner }
    }
}

/// Takes arguments once, records their values, then forwards that exact argument object to SQLx.
/// Keeping the original query preserves its cached statement and persistence setting.
struct CapturedExecute<'q, DB: Database, Q> {
    query: Q,
    arguments: Option<Result<Option<<DB as Database>::Arguments<'q>>, BoxDynError>>,
}

impl<'q, DB: Database, Q: Execute<'q, DB>> CapturedExecute<'q, DB, Q> {
    fn new(mut query: Q, guard: &mut QueryGuard) -> Self {
        let (arguments, captured) = capture_parameters(|| query.take_arguments());
        match &arguments {
            Ok(Some(arguments)) => {
                guard.parameters(match captured {
                    Some(parameters) => Ok(parameters),
                    // SQLx proves a zero-length argument buffer even without a snapshot.
                    None if arguments.len() == 0 => Ok(Vec::new()),
                    None => Err(crate::i18n::current().debug_expand_not_observed().into()),
                });
            }
            // The simple protocol has no placeholders to expand.
            Ok(None) => {}
            Err(error) => guard.parameters(Err(
                crate::i18n::current().debug_expand_encoding(error.to_string())
            )),
        }
        Self {
            query,
            arguments: Some(arguments),
        }
    }
}

#[derive(Default)]
struct CaptureState {
    active: bool,
    parameters: Option<Vec<SqlValue>>,
}
thread_local! { static PARAMETERS: RefCell<CaptureState> = RefCell::new(CaptureState::default()); }

// This hand-off exists only during the synchronous take_arguments call; it never spans
// an await or stream poll. Nesting and unwinding restore the prior slot.
fn capture_parameters<T>(take: impl FnOnce() -> T) -> (T, Option<Vec<SqlValue>>) {
    struct Restore(Option<CaptureState>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PARAMETERS.with(|slot| *slot.borrow_mut() = self.0.take().expect("capture state"));
        }
    }
    let previous = PARAMETERS.with(|slot| {
        slot.replace(CaptureState {
            active: true,
            parameters: None,
        })
    });
    let restore = Restore(Some(previous));
    let result = take();
    let parameters = PARAMETERS.with(|slot| slot.borrow_mut().parameters.take());
    drop(restore);
    (result, parameters)
}

pub(super) fn record_bound_parameters(parameters: &[SqlValue]) {
    PARAMETERS.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.active {
            state.parameters = Some(parameters.to_vec());
        }
    });
}

impl<'q, DB: Database, Q: Execute<'q, DB>> Execute<'q, DB> for CapturedExecute<'q, DB, Q> {
    fn sql(&self) -> &'q str {
        self.query.sql()
    }

    fn statement(&self) -> Option<&<DB as Database>::Statement<'q>> {
        self.query.statement()
    }

    fn take_arguments(&mut self) -> Result<Option<<DB as Database>::Arguments<'q>>, BoxDynError> {
        self.arguments.take().unwrap_or(Ok(None))
    }

    fn persistent(&self) -> bool {
        self.query.persistent()
    }
}

/// Rows affected: each backend has its own result type for an execution, and they share no common trait.
pub(super) trait RowsAffected {
    fn rows_affected(&self) -> u64;
}
impl RowsAffected for MySqlQueryResult {
    fn rows_affected(&self) -> u64 {
        MySqlQueryResult::rows_affected(self)
    }
}
impl RowsAffected for SqliteQueryResult {
    fn rows_affected(&self) -> u64 {
        SqliteQueryResult::rows_affected(self)
    }
}

type QueryStep<DB> = Result<Either<<DB as Database>::QueryResult, <DB as Database>::Row>, Error>;

struct ObservedStream<'e, DB: Database> {
    inner: Option<BoxStream<'e, QueryStep<DB>>>,
    guard: Option<QueryGuard>,
    span: tracing::Span,
}

impl<DB: Database> Stream for ObservedStream<'_, DB>
where
    DB::QueryResult: RowsAffected,
{
    type Item = QueryStep<DB>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let _entered = this.span.enter();
        let poll = this
            .inner
            .as_mut()
            .expect("live stream")
            .as_mut()
            .poll_next(cx);
        match &poll {
            Poll::Ready(Some(Ok(step))) => {
                if let Some(guard) = &mut this.guard {
                    match step {
                        Either::Left(result) => guard.affected(result.rows_affected()),
                        Either::Right(_) => guard.returned(),
                    }
                }
            }
            Poll::Ready(Some(Err(error))) => {
                if let Some(guard) = this.guard.take() {
                    guard.finish(Some(error.to_string()));
                }
            }
            Poll::Ready(None) => {
                if let Some(guard) = this.guard.take() {
                    guard.finish(None);
                }
            }
            Poll::Pending => {}
        }
        poll
    }
}

impl<DB: Database> Drop for ObservedStream<'_, DB> {
    fn drop(&mut self) {
        // MySQL's QueryLogger emits its event when the stream is dropped; a cancellation must keep the request
        // attribution too.
        let _entered = self.span.enter();
        drop(self.inner.take());
    }
}

impl<'c, E: Executor<'c>> Executor<'c> for Observed<E>
where
    <E::Database as Database>::QueryResult: RowsAffected,
{
    type Database = E::Database;

    fn fetch_many<'e, 'q: 'e, Q>(self, query: Q) -> BoxStream<'e, QueryStep<E::Database>>
    where
        'c: 'e,
        Q: 'q + Execute<'q, E::Database>,
    {
        crate::metrics::count_sql();
        let Some(profile) = trace::current() else {
            return self.inner.fetch_many(query);
        };
        let backend = Backend::of_driver(<E::Database as Database>::NAME);
        let mut guard = profile.start_on(backend, query.sql());
        let span = driver::query_span(backend, query.sql());
        let query = CapturedExecute::new(query, &mut guard);
        Box::pin(ObservedStream::<E::Database> {
            inner: Some(self.inner.fetch_many(query)),
            guard: Some(guard),
            span,
        })
    }

    fn fetch_optional<'e, 'q: 'e, Q>(
        self,
        query: Q,
    ) -> BoxFuture<'e, Result<Option<<E::Database as Database>::Row>, Error>>
    where
        'c: 'e,
        Q: 'q + Execute<'q, E::Database>,
    {
        crate::metrics::count_sql();
        let Some(profile) = trace::current() else {
            return self.inner.fetch_optional(query);
        };
        let backend = Backend::of_driver(<E::Database as Database>::NAME);
        let mut guard = profile.start_on(backend, query.sql());
        let span = driver::query_span(backend, query.sql());
        let query = CapturedExecute::new(query, &mut guard);
        let future = self.inner.fetch_optional(query);
        Box::pin(
            async move {
                let result = future.await;
                if matches!(&result, Ok(Some(_))) {
                    guard.returned();
                }
                guard.finish(result.as_ref().err().map(ToString::to_string));
                result
            }
            .instrument(span),
        )
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<E::Database as Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<E::Database as Database>::Statement<'q>, Error>>
    where
        'c: 'e,
    {
        self.inner.prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<Describe<E::Database>, Error>>
    where
        'c: 'e,
    {
        self.inner.describe(sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::query, debug::trace::RequestProfile};
    use futures_util::StreamExt;
    use std::sync::Arc;

    #[tokio::test]
    async fn captures_typed_values_and_forwards_arguments_once() {
        let profile = Arc::new(RequestProfile::new());
        trace::scope(profile.clone(), async {
            let mut guard = profile.start("SELECT ?, ?, ?, ?");
            let query = query("SELECT ?, ?, ?, ?")
                .bind(7_i64)
                .bind("a'b")
                .bind(None::<i64>)
                .bind(u32::MAX)
                .into_mysql();
            let mut captured = CapturedExecute::new(query, &mut guard);
            assert_eq!(captured.take_arguments().unwrap().unwrap().len(), 4);
            assert!(captured.take_arguments().unwrap().is_none());
            guard.finish(None);
        })
        .await;
        let record = &profile.snapshot()[0];
        assert_eq!(
            record.parameters,
            [
                SqlValue::Integer(7),
                SqlValue::Text("a'b".into()),
                SqlValue::Null,
                SqlValue::Unsigned(u64::from(u32::MAX))
            ]
        );
        assert!(record.expansion_error.is_none());
    }

    #[tokio::test]
    async fn late_observation_preserves_execution_but_does_not_invent_parameters() {
        let unobserved = query("SELECT ?, ?")
            .bind(None::<i64>)
            .bind(u32::MAX)
            .into_mysql();
        let created_outside = query("SELECT ?");
        let profile = Arc::new(RequestProfile::new());
        trace::scope(profile.clone(), async {
            let mut guard = profile.start("SELECT ?, ?");
            let mut captured = CapturedExecute::new(unobserved, &mut guard);
            assert_eq!(captured.take_arguments().unwrap().unwrap().len(), 2);
            assert!(captured.take_arguments().unwrap().is_none());
            guard.finish(None);

            let mut guard = profile.start("SELECT ?");
            let mut captured =
                CapturedExecute::new(created_outside.bind(7_i64).into_mysql(), &mut guard);
            assert_eq!(captured.take_arguments().unwrap().unwrap().len(), 1);
            guard.finish(None);
        })
        .await;
        for record in profile.snapshot() {
            assert!(record.parameters.is_empty());
            assert_eq!(record.expanded_sql, record.sql);
            assert_eq!(
                record.expansion_error.as_deref(),
                Some(
                    "The bound parameters were not captured by request observation and cannot be shown reliably"
                )
            );
            assert!(record.error.is_none());
        }
    }

    #[tokio::test]
    async fn unobserved_query_with_no_arguments_can_still_be_displayed() {
        let unobserved = query("SELECT 1").into_mysql();
        let profile = Arc::new(RequestProfile::new());
        trace::scope(profile.clone(), async {
            let mut guard = profile.start("SELECT 1");
            let mut captured = CapturedExecute::new(unobserved, &mut guard);
            assert_eq!(captured.take_arguments().unwrap().unwrap().len(), 0);
            guard.finish(None);
        })
        .await;
        let record = &profile.snapshot()[0];
        assert!(record.parameters.is_empty());
        assert!(record.expansion_error.is_none());
        assert_eq!(record.expanded_sql, "SELECT 1");
    }

    #[test]
    fn foreign_bound_queries_do_not_get_fabricated_values() {
        let profile = Arc::new(RequestProfile::new());
        let mut guard = profile.start("SELECT ?");
        let _captured = CapturedExecute::new(
            sqlx::query::<sqlx::MySql>("SELECT ?").bind(7_i64),
            &mut guard,
        );
        guard.finish(None);
        assert!(profile.snapshot()[0].expansion_error.is_some());
    }

    #[test]
    fn capture_slots_restore_nested_and_panicked_state() {
        let (_, outer) = capture_parameters(|| {
            record_bound_parameters(&[SqlValue::Integer(1)]);
            let (_, inner) =
                capture_parameters(|| record_bound_parameters(&[SqlValue::Integer(2)]));
            assert_eq!(inner, Some(vec![SqlValue::Integer(2)]));
            let _ = std::panic::catch_unwind(|| capture_parameters(|| panic!("capture panic")));
        });
        assert_eq!(outer, Some(vec![SqlValue::Integer(1)]));
        record_bound_parameters(&[SqlValue::Integer(3)]);
        let (_, empty) = capture_parameters(|| ());
        assert_eq!(empty, None);
    }

    #[tokio::test]
    async fn stream_errors_and_cancellation_are_recorded() {
        let profile = Arc::new(RequestProfile::new());
        let mut stream = ObservedStream::<sqlx::MySql> {
            inner: Some(Box::pin(futures_util::stream::iter([Err(
                Error::RowNotFound,
            )]))),
            guard: Some(profile.start("SELECT 1")),
            span: tracing::Span::none(),
        };
        assert!(stream.next().await.unwrap().is_err());
        drop(stream);
        let cancelled = ObservedStream::<sqlx::MySql> {
            inner: Some(Box::pin(futures_util::stream::pending())),
            guard: Some(profile.start("SELECT 2")),
            span: tracing::Span::none(),
        };
        drop(cancelled);
        let records = profile.snapshot();
        assert!(records[0].error.is_some());
        assert!(!records[0].cancelled);
        assert!(records[1].cancelled);
    }
}
