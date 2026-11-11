//! Queries and binding: callers never see backend types. Parameters keep their original Rust types until execution,
//! when the backend encodes them; the debug panel records a separate `SqlValue` at bind time.
//!
//! SQL is written as MySQL text. On SQLite it goes through [`dialect::sqlite`](super::dialect::sqlite) before
//! execution; for constructs the rewriter does not recognize, the call site supplies SQLite text with
//! [`Query::sqlite`], written right after the MySQL text.
use super::{
    DbRow, Decodable,
    conn::{Executor, Target, TargetKind},
    dialect,
    observed::{Observed, record_bound_parameters},
    sql_display::SqlValue,
};
use crate::debug::trace;
use futures_util::{
    future::BoxFuture,
    stream::{BoxStream, StreamExt, TryStreamExt},
};
use sqlx::{
    Arguments, Database, Encode, Error, Execute, Executor as _, MySql, Sqlite, Type,
    error::BoxDynError,
    mysql::{MySqlArguments, MySqlQueryResult},
    sqlite::{SqliteArguments, SqliteQueryResult},
};
use std::borrow::Cow;

pub trait SqlParameter {
    fn sql_value(&self) -> SqlValue;
}
macro_rules! signed { ($($ty:ty),+) => { $(impl SqlParameter for $ty {
    fn sql_value(&self) -> SqlValue { SqlValue::Integer(i64::from(*self)) }
})+ }; }
macro_rules! unsigned { ($($ty:ty),+) => { $(impl SqlParameter for $ty {
    fn sql_value(&self) -> SqlValue { SqlValue::Unsigned(u64::from(*self)) }
})+ }; }
signed!(i8, i16, i32, i64);
unsigned!(u8, u16, u32, u64);
impl SqlParameter for f32 {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Float(f64::from(*self))
    }
}
impl SqlParameter for f64 {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Float(*self)
    }
}
impl SqlParameter for bool {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Bool(*self)
    }
}
impl SqlParameter for str {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(self.to_owned())
    }
}
impl SqlParameter for String {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(self.clone())
    }
}
impl SqlParameter for [u8] {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Bytes(self.to_vec())
    }
}
impl SqlParameter for Vec<u8> {
    fn sql_value(&self) -> SqlValue {
        SqlValue::Bytes(self.clone())
    }
}
impl<T: SqlParameter + ?Sized> SqlParameter for &T {
    fn sql_value(&self) -> SqlValue {
        (*self).sql_value()
    }
}
impl<T: SqlParameter> SqlParameter for Option<T> {
    fn sql_value(&self) -> SqlValue {
        self.as_ref()
            .map_or(SqlValue::Null, SqlParameter::sql_value)
    }
}

/// Rust types that can be bound as parameters. The backends' encoding bounds are written only here.
pub trait Bindable<'q>:
    'q + Send + Encode<'q, MySql> + Type<MySql> + Encode<'q, Sqlite> + Type<Sqlite> + SqlParameter
{
}
impl<'q, T> Bindable<'q> for T where
    T: 'q
        + Send
        + Encode<'q, MySql>
        + Type<MySql>
        + Encode<'q, Sqlite>
        + Type<Sqlite>
        + SqlParameter
{
}

/// One bound parameter: keeps its original Rust type and is written into the backend's argument buffer only at
/// execution.
trait Argument<'q>: Send {
    fn add_mysql(self: Box<Self>, arguments: &mut MySqlArguments) -> Result<(), BoxDynError>;
    fn add_sqlite(self: Box<Self>, arguments: &mut SqliteArguments<'q>) -> Result<(), BoxDynError>;
}

impl<'q, T> Argument<'q> for T
where
    T: 'q + Send + Encode<'q, MySql> + Type<MySql> + Encode<'q, Sqlite> + Type<Sqlite>,
{
    fn add_mysql(self: Box<Self>, arguments: &mut MySqlArguments) -> Result<(), BoxDynError> {
        arguments.add(*self)
    }
    fn add_sqlite(self: Box<Self>, arguments: &mut SqliteArguments<'q>) -> Result<(), BoxDynError> {
        arguments.add(*self)
    }
}

pub struct Query<'q> {
    sql: &'q str,
    /// Hand-written SQLite text; without it, SQLite executes the rewritten `sql`.
    sqlite: Option<&'q str>,
    arguments: Vec<Box<dyn Argument<'q> + 'q>>,
    persistent: bool,
    // None means no complete capture is available, not that the query has zero bindings.
    parameters: Option<Vec<SqlValue>>,
}
pub fn query(sql: &str) -> Query<'_> {
    Query {
        sql,
        sqlite: None,
        arguments: Vec::new(),
        persistent: true,
        parameters: trace::current().map(|_| Vec::new()),
    }
}

/// Execution result: rows affected and the auto-increment value. Both backends count matched rows as affected (the
/// MySQL connection has CLIENT_FOUND_ROWS on).
#[derive(Clone, Copy, Debug, Default)]
pub struct QueryResult {
    rows_affected: u64,
    last_insert_id: u64,
}

impl QueryResult {
    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }
    pub fn last_insert_id(&self) -> u64 {
        self.last_insert_id
    }
}

impl From<MySqlQueryResult> for QueryResult {
    fn from(result: MySqlQueryResult) -> Self {
        Self {
            rows_affected: result.rows_affected(),
            last_insert_id: result.last_insert_id(),
        }
    }
}

impl From<SqliteQueryResult> for QueryResult {
    fn from(result: SqliteQueryResult) -> Self {
        Self {
            rows_affected: result.rows_affected(),
            // rowid starts at 1; tables only use positive auto-increment values.
            last_insert_id: u64::try_from(result.last_insert_rowid()).unwrap_or(0),
        }
    }
}

/// Dispatches by execution target: anything wrapped by `db.executor()` / `db.observe(..)` gets [`Observed`] (counted
/// in the debug panel and the footer count); everything else goes straight to SQLx. In each branch `$q` is that
/// backend's execution form and `$x` is that backend's executor; the branches have different types, hence a macro
/// expansion instead of a closure, and the result of `$body` is converted to the project's own type with `From`.
///
/// The SQLite branch first takes the SQLite text to execute (returning an error when it cannot be rewritten), and uses
/// the write pool when `$write` is true, otherwise the read pool. The rewritten text is a local variable, so `$body`
/// must await its result inside this block.
macro_rules! dispatch {
    ($target:expr, $query:expr, $write:expr, |$x:ident, $q:ident| $body:expr) => {{
        let Target { observed, kind } = $target;
        match kind {
            TargetKind::MySqlPool(pool) => {
                let $q = $query.into_mysql();
                if observed {
                    let $x = Observed::new(pool);
                    $body
                } else {
                    let $x = pool;
                    $body
                }
            }
            TargetKind::MySqlConn(connection) => {
                let $q = $query.into_mysql();
                if observed {
                    let $x = Observed::new(connection);
                    $body
                } else {
                    let $x = connection;
                    $body
                }
            }
            TargetKind::SqlitePool(pools) => {
                let sql = $query.sqlite_text()?;
                let $q = $query.into_sqlite(&sql);
                let pool = if $write { &pools.write } else { &pools.read };
                if observed {
                    let $x = Observed::new(pool);
                    $body
                } else {
                    let $x = pool;
                    $body
                }
            }
            TargetKind::SqliteConn(connection) => {
                let sql = $query.sqlite_text()?;
                let $q = $query.into_sqlite(&sql);
                let connection = &mut *connection;
                if observed {
                    let $x = Observed::new(connection);
                    $body
                } else {
                    let $x = connection;
                    $body
                }
            }
        }
    }};
}

impl<'q> Query<'q> {
    pub fn bind<T>(mut self, value: T) -> Self
    where
        T: Bindable<'q>,
    {
        if trace::current().is_none() {
            // A query may leave its original scope before binding. Discard partial captures
            // so a later observed execution cannot mistake them for the complete argument list.
            self.parameters = None;
        } else if let Some(parameters) = &mut self.parameters {
            parameters.push(value.sql_value());
        }
        self.arguments.push(Box::new(value));
        self
    }
    pub fn persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Hand-written SQLite text with the same number and order of parameters as the MySQL text (write `?NNN` to change
    /// the order). Only for constructs the rewriter cannot handle.
    pub fn sqlite(mut self, sql: &'q str) -> Self {
        self.sqlite = Some(sql);
        self
    }

    /// The text has already been assembled for `backend` (fragments of dynamic SQL written per backend): executed
    /// verbatim on SQLite, bypassing the rewriter.
    pub fn written_for(mut self, backend: super::Backend) -> Self {
        if backend == super::Backend::Sqlite {
            self.sqlite = Some(self.sql);
        }
        self
    }

    /// The text actually executed on SQLite.
    fn sqlite_text(&self) -> Result<Cow<'q, str>, Error> {
        match self.sqlite {
            Some(sql) => Ok(Cow::Borrowed(sql)),
            None => Ok(dialect::sqlite(self.sql)?),
        }
    }

    /// MySQL execution form: encodes parameters by their original types; an encoding failure surfaces at execution
    /// (same as SQLx's `bind`).
    pub(super) fn into_mysql(self) -> BackendQuery<'q, MySql> {
        let mut arguments = MySqlArguments::default();
        let failed = self
            .arguments
            .into_iter()
            .find_map(|argument| argument.add_mysql(&mut arguments).err());
        BackendQuery::new(
            self.sql,
            arguments,
            failed,
            self.persistent,
            self.parameters,
        )
    }

    /// SQLite execution form, like [`Query::into_mysql`]; `sql` is the text taken by [`Query::sqlite_text`].
    pub(super) fn into_sqlite<'s>(self, sql: &'s str) -> BackendQuery<'s, Sqlite>
    where
        'q: 's,
    {
        let mut arguments = SqliteArguments::default();
        let failed = self
            .arguments
            .into_iter()
            .find_map(|argument| argument.add_sqlite(&mut arguments).err());
        BackendQuery::new(sql, arguments, failed, self.persistent, self.parameters)
    }

    pub fn execute<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<QueryResult, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let target = executor.target();
        Box::pin(async move {
            dispatch!(target, self, true, |executor, query| executor
                .execute(query)
                .await
                .map(QueryResult::from))
        })
    }
    /// On SQLite the rewritten text is a local variable that the stream cannot outlive, so all rows are fetched first
    /// and then yielded one by one.
    pub fn fetch<'e, 'c: 'e, E>(self, executor: E) -> BoxStream<'e, Result<DbRow, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let Target { observed, kind } = executor.target();
        let target = match kind {
            TargetKind::MySqlPool(pool) => {
                let query = self.into_mysql();
                return if observed {
                    Box::pin(
                        Observed::new(pool)
                            .fetch(query)
                            .map(|row| row.map(DbRow::from)),
                    )
                } else {
                    Box::pin(pool.fetch(query).map(|row| row.map(DbRow::from)))
                };
            }
            TargetKind::MySqlConn(connection) => {
                let query = self.into_mysql();
                return if observed {
                    Box::pin(
                        Observed::new(connection)
                            .fetch(query)
                            .map(|row| row.map(DbRow::from)),
                    )
                } else {
                    Box::pin(connection.fetch(query).map(|row| row.map(DbRow::from)))
                };
            }
            kind => Target { observed, kind },
        };
        let rows = Box::pin(async move {
            dispatch!(target, self, false, |executor, query| executor
                .fetch_all(query)
                .await
                .map(|rows| rows.into_iter().map(DbRow::from).collect::<Vec<_>>()))
        });
        Box::pin(
            futures_util::stream::once(rows)
                .map_ok(|rows| futures_util::stream::iter(rows.into_iter().map(Ok)))
                .try_flatten(),
        )
    }
    pub fn fetch_all<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<Vec<DbRow>, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let target = executor.target();
        Box::pin(async move {
            dispatch!(target, self, false, |executor, query| executor
                .fetch_all(query)
                .await
                .map(|rows| rows.into_iter().map(DbRow::from).collect()))
        })
    }
    pub fn fetch_one<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<DbRow, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let target = executor.target();
        Box::pin(async move {
            dispatch!(target, self, false, |executor, query| executor
                .fetch_one(query)
                .await
                .map(DbRow::from))
        })
    }
    pub fn fetch_optional<'e, 'c: 'e, E>(
        self,
        executor: E,
    ) -> BoxFuture<'e, Result<Option<DbRow>, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let target = executor.target();
        Box::pin(async move {
            dispatch!(target, self, false, |executor, query| executor
                .fetch_optional(query)
                .await
                .map(|row| row.map(DbRow::from)))
        })
    }
}

/// The execution form of a [`Query`] on one backend. `take_arguments` hands the parameters recorded at bind time to
/// the debug record.
pub(super) struct BackendQuery<'q, DB: Database> {
    sql: &'q str,
    arguments: Option<Result<<DB as Database>::Arguments<'q>, BoxDynError>>,
    persistent: bool,
    parameters: Option<Vec<SqlValue>>,
}

impl<'q, DB: Database> BackendQuery<'q, DB> {
    fn new(
        sql: &'q str,
        arguments: <DB as Database>::Arguments<'q>,
        failed: Option<BoxDynError>,
        persistent: bool,
        parameters: Option<Vec<SqlValue>>,
    ) -> Self {
        Self {
            sql,
            arguments: Some(match failed {
                None => Ok(arguments),
                Some(error) => Err(error),
            }),
            persistent,
            parameters,
        }
    }
}

impl<'q, DB: Database> Execute<'q, DB> for BackendQuery<'q, DB> {
    fn sql(&self) -> &'q str {
        self.sql
    }
    fn statement(&self) -> Option<&<DB as Database>::Statement<'q>> {
        None
    }
    fn take_arguments(&mut self) -> Result<Option<<DB as Database>::Arguments<'q>>, BoxDynError> {
        if let Some(parameters) = &self.parameters {
            record_bound_parameters(parameters);
        }
        self.arguments.take().transpose()
    }
    fn persistent(&self) -> bool {
        self.persistent
    }
}

pub struct QueryScalar<'q, O> {
    inner: Query<'q>,
    output: std::marker::PhantomData<O>,
}
pub fn query_scalar<O>(sql: &str) -> QueryScalar<'_, O> {
    QueryScalar {
        inner: query(sql),
        output: std::marker::PhantomData,
    }
}
impl<'q, O> QueryScalar<'q, O>
where
    O: Send + Unpin + for<'r> Decodable<'r>,
{
    pub fn bind<T>(mut self, value: T) -> Self
    where
        T: Bindable<'q>,
    {
        self.inner = self.inner.bind(value);
        self
    }
    pub fn persistent(mut self, persistent: bool) -> Self {
        self.inner = self.inner.persistent(persistent);
        self
    }
    /// Same as [`Query::sqlite`].
    pub fn sqlite(mut self, sql: &'q str) -> Self {
        self.inner = self.inner.sqlite(sql);
        self
    }
    /// Same as [`Query::written_for`].
    pub fn written_for(mut self, backend: super::Backend) -> Self {
        self.inner = self.inner.written_for(backend);
        self
    }
    pub fn fetch_one<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<O, Error>>
    where
        'q: 'e,
        O: 'e,
        E: Executor<'c>,
    {
        let row = self.inner.fetch_one(executor);
        Box::pin(async move { row.await?.try_get(0) })
    }
    pub fn fetch_optional<'e, 'c: 'e, E>(
        self,
        executor: E,
    ) -> BoxFuture<'e, Result<Option<O>, Error>>
    where
        'q: 'e,
        O: 'e,
        E: Executor<'c>,
    {
        let row = self.inner.fetch_optional(executor);
        Box::pin(async move { row.await?.map(|row| row.try_get(0)).transpose() })
    }
    pub fn fetch_all<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<Vec<O>, Error>>
    where
        'q: 'e,
        O: 'e,
        E: Executor<'c>,
    {
        let rows = self.inner.fetch_all(executor);
        Box::pin(async move { rows.await?.into_iter().map(|row| row.try_get(0)).collect() })
    }
}

/// Raw SQL without parameters that may contain several statements (`sqlx::raw_sql`). Only for tests and fault
/// injection. On SQLite, single statements starting with SELECT, INSERT, UPDATE, DELETE or WITH also go through the
/// rewriter (ordinary data-changing statements in test helpers are written as MySQL text, as with `db::query`); DDL,
/// PRAGMA and multi-statement text run verbatim.
pub struct RawSql<'q>(&'q str);
pub fn raw_sql(sql: &str) -> RawSql<'_> {
    RawSql(sql)
}
impl<'q> RawSql<'q> {
    fn into_mysql(self) -> sqlx::RawSql<'q> {
        sqlx::raw_sql(self.0)
    }
    fn sqlite_text(&self) -> Result<Cow<'q, str>, Error> {
        let head: String = self
            .0
            .trim_start()
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_ascii_uppercase();
        let statement = matches!(
            head.as_str(),
            "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH"
        );
        if statement && !self.0.trim_end().trim_end_matches(';').contains(';') {
            Ok(dialect::sqlite(self.0.trim_end().trim_end_matches(';'))?)
        } else {
            Ok(Cow::Borrowed(self.0))
        }
    }
    fn into_sqlite<'s>(self, sql: &'s str) -> sqlx::RawSql<'s> {
        sqlx::raw_sql(sql)
    }
    pub fn execute<'e, 'c: 'e, E>(self, executor: E) -> BoxFuture<'e, Result<QueryResult, Error>>
    where
        'q: 'e,
        E: Executor<'c>,
    {
        let target = executor.target();
        Box::pin(async move {
            dispatch!(target, self, true, |executor, sql| executor
                .execute(sql)
                .await
                .map(QueryResult::from))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{encode::IsNull, mysql::MySqlTypeInfo};
    use std::sync::Arc;

    // Encoding remains valid; the test fails if disabled observation evaluates a snapshot.
    struct UnobservedParameter(i64);

    impl SqlParameter for UnobservedParameter {
        fn sql_value(&self) -> SqlValue {
            panic!("disabled observation must not capture a parameter")
        }
    }

    impl Type<MySql> for UnobservedParameter {
        fn type_info() -> MySqlTypeInfo {
            <i64 as Type<MySql>>::type_info()
        }
    }

    impl Encode<'_, MySql> for UnobservedParameter {
        fn encode_by_ref(&self, buf: &mut Vec<u8>) -> Result<IsNull, BoxDynError> {
            <i64 as Encode<'_, MySql>>::encode_by_ref(&self.0, buf)
        }
    }

    impl Type<Sqlite> for UnobservedParameter {
        fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
            <i64 as Type<Sqlite>>::type_info()
        }
    }

    impl<'q> Encode<'q, Sqlite> for UnobservedParameter {
        fn encode_by_ref(
            &self,
            buf: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
        ) -> Result<IsNull, BoxDynError> {
            <i64 as Encode<'q, Sqlite>>::encode_by_ref(&self.0, buf)
        }
    }

    #[test]
    fn unobserved_bindings_skip_snapshots_and_keep_sqlx_arguments() {
        assert!(trace::current().is_none());
        let query = query("SELECT ?").bind(UnobservedParameter(7));
        assert!(query.parameters.is_none());
        let mut query = query.into_mysql();
        assert_eq!(query.take_arguments().unwrap().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn binding_outside_the_original_scope_discards_partial_capture() {
        let profile = Arc::new(trace::RequestProfile::new());
        let query = trace::scope(profile.clone(), async {
            let query = query("SELECT ?, ?, ?").bind(1_i64);
            assert_eq!(query.parameters, Some(vec![SqlValue::Integer(1)]));
            query
        })
        .await;
        let query = query.bind(UnobservedParameter(2));
        assert!(query.parameters.is_none());
        trace::scope(profile, async {
            let query = query.bind(UnobservedParameter(3));
            assert!(query.parameters.is_none());
            let mut query = query.into_mysql();
            assert_eq!(query.take_arguments().unwrap().unwrap().len(), 3);
        })
        .await;
    }
}
