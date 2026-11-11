//! Connection pools, transactions and executors: callers see only the project's own types, and the backend is decided
//! by the connection string (docs/architecture.md, "Data").
//!
//! `Tx` derefs to `TxConn` and `PoolConn` derefs to `PooledConn`, so `db.observe(&mut *tx)`,
//! `db.observe(&mut **tx)` and `.execute(&mut *connection)` are written as with SQLx's own types.
//!
//! SQLite has separate read and write pools: write transactions, `execute` and acquired connections use the write pool;
//! `fetch*` and read-only transactions use the read pool (connections set to read-only, `query_only`); both pools open
//! up to `database.max_connections`. SQLite itself arbitrates who gets the write lock: write transactions start with
//! `BEGIN IMMEDIATE`; if the lock is unavailable they wait 5 seconds and then report BUSY (handled as a lock conflict,
//! like MySQL's lock wait timeout 1205). Under WAL reads do not block writes and writes do not block reads; while
//! writes queue, reads still have their own connections.
use std::ops::{Deref, DerefMut};

use sqlx::{
    MySql, MySqlConnection, MySqlPool, Sqlite, SqliteConnection, SqlitePool, pool::PoolConnection,
};

/// Connection pool. Test and diagnostic code may use `&pool` directly as an executor (not counted in the debug panel or
/// the footer).
#[derive(Clone, Debug)]
pub struct Pool(PoolKind);

#[derive(Clone, Debug)]
enum PoolKind {
    MySql(MySqlPool),
    Sqlite(SqlitePools),
}

/// The two SQLite pools, pointing at the same database file.
#[derive(Clone, Debug)]
pub(super) struct SqlitePools {
    pub(super) write: SqlitePool,
    pub(super) read: SqlitePool,
}

/// Transactions on `Database` named by purpose; how each backend opens them is in [`Pool::begin_for`].
#[derive(Clone, Copy, Debug)]
pub(super) enum Begin {
    /// Ordinary write transaction.
    Write,
    /// Content writes: every statement sees rows committed by other transactions while it waited for locks.
    ContentWrite,
    /// Read-only, one snapshot (starting from the first read).
    ReadOnly,
    /// Read-only consistent snapshot; the connection is closed after use.
    Snapshot,
    /// Write transaction; the connection is closed after use and not returned to the pool.
    WriteClosing,
}

impl Pool {
    pub(super) fn from_mysql(pool: MySqlPool) -> Self {
        Self(PoolKind::MySql(pool))
    }

    pub(super) fn from_sqlite(write: SqlitePool, read: SqlitePool) -> Self {
        Self(PoolKind::Sqlite(SqlitePools { write, read }))
    }

    /// Only for MySQL-specific diagnostic and test code (EXPLAIN, fault injection, migrations). All other code goes
    /// through `db::query`.
    pub fn as_mysql(&self) -> Option<&MySqlPool> {
        match &self.0 {
            PoolKind::MySql(pool) => Some(pool),
            PoolKind::Sqlite(_) => None,
        }
    }

    /// Only for SQLite-specific diagnostic and test code: the write pool (one connection only).
    pub fn as_sqlite(&self) -> Option<&SqlitePool> {
        match &self.0 {
            PoolKind::MySql(_) => None,
            PoolKind::Sqlite(pools) => Some(&pools.write),
        }
    }

    /// Only for SQLite-specific diagnostic and test code: the read pool of read-only connections.
    pub fn as_sqlite_read(&self) -> Option<&SqlitePool> {
        match &self.0 {
            PoolKind::MySql(_) => None,
            PoolKind::Sqlite(pools) => Some(&pools.read),
        }
    }

    /// Ordinary transaction: MySQL uses the instance default REPEATABLE READ; SQLite uses `BEGIN IMMEDIATE`, taking the
    /// write lock at the start.
    pub async fn begin(&self) -> Result<Tx<'static>, sqlx::Error> {
        self.begin_for(Begin::Write).await
    }

    pub(super) async fn begin_for(&self, begin: Begin) -> Result<Tx<'static>, sqlx::Error> {
        match &self.0 {
            PoolKind::MySql(pool) => {
                // None is SQLx's default `BEGIN`.
                let statement = match begin {
                    Begin::Write => return Ok(Tx::mysql(pool.begin().await?)),
                    Begin::ContentWrite => {
                        Some("SET TRANSACTION ISOLATION LEVEL READ COMMITTED; START TRANSACTION")
                    }
                    Begin::ReadOnly => Some(
                        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; START TRANSACTION READ ONLY",
                    ),
                    Begin::Snapshot => Some(
                        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; START TRANSACTION WITH CONSISTENT SNAPSHOT, READ ONLY",
                    ),
                    Begin::WriteClosing => None,
                };
                let mut connection = pool.acquire().await?;
                if matches!(
                    begin,
                    Begin::ContentWrite | Begin::Snapshot | Begin::WriteClosing
                ) {
                    connection.close_on_drop();
                }
                Ok(Tx::mysql(
                    sqlx::Transaction::begin(connection, statement.map(Into::into)).await?,
                ))
            }
            PoolKind::Sqlite(pools) => {
                // Write transactions are always IMMEDIATE and use the write pool: only one writer at a time, taking the
                // write lock at the start, so all reads in the transaction happen inside the lock.
                // Read-only transactions are DEFERRED and use the read pool: under WAL the snapshot starts at the first
                // read and stays fixed after that.
                if matches!(begin, Begin::ReadOnly | Begin::Snapshot) {
                    let connection = pools.read.acquire().await?;
                    return Ok(Tx::sqlite(
                        sqlx::Transaction::begin(connection, Some("BEGIN DEFERRED".into())).await?,
                    ));
                }
                let mut connection = pools.write.acquire().await?;
                if matches!(begin, Begin::WriteClosing) {
                    connection.close_on_drop();
                }
                Ok(Tx::sqlite(
                    sqlx::Transaction::begin(connection, Some("BEGIN IMMEDIATE".into())).await?,
                ))
            }
        }
    }

    /// Acquires a connection: on SQLite it comes from the write pool (migrations and tests open transactions and change
    /// session settings on it).
    pub async fn acquire(&self) -> Result<PoolConn, sqlx::Error> {
        Ok(PoolConn(PooledConn(match &self.0 {
            PoolKind::MySql(pool) => PooledKind::MySql(pool.acquire().await?),
            PoolKind::Sqlite(pools) => PooledKind::Sqlite(pools.write.acquire().await?),
        })))
    }

    pub async fn close(&self) {
        match &self.0 {
            PoolKind::MySql(pool) => pool.close().await,
            PoolKind::Sqlite(pools) => {
                pools.read.close().await;
                pools.write.close().await;
            }
        }
    }

    /// Database name (tests name their Redis namespace after it): on MySQL the database name in the connection string,
    /// on SQLite the database file name without extension.
    pub fn database_name(&self) -> Option<String> {
        match &self.0 {
            PoolKind::MySql(pool) => pool.connect_options().get_database().map(str::to_owned),
            PoolKind::Sqlite(pools) => pools
                .write
                .connect_options()
                .get_filename()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned),
        }
    }

    /// For tests: opens another pool with the same connection parameters (cold connections, limited connection count,
    /// and so on).
    #[doc(hidden)]
    pub fn reopen_lazy(&self, min_connections: u32, max_connections: u32) -> Self {
        match &self.0 {
            PoolKind::MySql(pool) => Self::from_mysql(
                pool.options()
                    .clone()
                    .min_connections(min_connections)
                    .max_connections(max_connections)
                    .connect_lazy_with((*pool.connect_options()).clone()),
            ),
            PoolKind::Sqlite(pools) => {
                let reopen = |pool: &SqlitePool, max: u32| {
                    pool.options()
                        .clone()
                        .min_connections(min_connections.min(max))
                        .max_connections(max)
                        .connect_lazy_with((*pool.connect_options()).clone())
                };
                Self::from_sqlite(
                    reopen(&pools.write, 1),
                    reopen(&pools.read, max_connections),
                )
            }
        }
    }
}

/// Transaction. Derefs to the [`TxConn`] that executes statements inside the transaction.
pub struct Tx<'c>(TxConn<'c>);

/// The connection inside a transaction: this is what `db.observe(&mut *tx)` gets.
pub struct TxConn<'c>(TxKind<'c>);

enum TxKind<'c> {
    MySql(sqlx::Transaction<'c, MySql>),
    Sqlite(sqlx::Transaction<'c, Sqlite>),
}

impl<'c> Tx<'c> {
    fn mysql(transaction: sqlx::Transaction<'c, MySql>) -> Self {
        Self(TxConn(TxKind::MySql(transaction)))
    }

    fn sqlite(transaction: sqlx::Transaction<'c, Sqlite>) -> Self {
        Self(TxConn(TxKind::Sqlite(transaction)))
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self.0.0 {
            TxKind::MySql(transaction) => transaction.commit().await,
            TxKind::Sqlite(transaction) => transaction.commit().await,
        }
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        match self.0.0 {
            TxKind::MySql(transaction) => transaction.rollback().await,
            TxKind::Sqlite(transaction) => transaction.rollback().await,
        }
    }
}

impl<'c> Deref for Tx<'c> {
    type Target = TxConn<'c>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Tx<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// A connection taken from the pool. Derefs to the [`PooledConn`] that executes statements.
pub struct PoolConn(PooledConn);

pub struct PooledConn(PooledKind);

enum PooledKind {
    MySql(PoolConnection<MySql>),
    Sqlite(PoolConnection<Sqlite>),
}

impl PoolConn {
    /// Closed after use, not returned to the pool: session variables or unread results do not travel with the
    /// connection to another request.
    pub fn close_on_drop(&mut self) {
        match &mut self.0.0 {
            PooledKind::MySql(connection) => connection.close_on_drop(),
            PooledKind::Sqlite(connection) => connection.close_on_drop(),
        }
    }

    /// Opens a transaction on this connection; `statement` is the raw statement that opens it (written per backend).
    /// Application code uses the purpose-named `begin_*` on `Database`; this is left for tests that first change session
    /// variables and then open a transaction on the same connection.
    pub async fn begin_with(self, statement: &'static str) -> Result<Tx<'static>, sqlx::Error> {
        Ok(match self.0.0 {
            PooledKind::MySql(connection) => {
                Tx::mysql(sqlx::Transaction::begin(connection, Some(statement.into())).await?)
            }
            PooledKind::Sqlite(connection) => {
                Tx::sqlite(sqlx::Transaction::begin(connection, Some(statement.into())).await?)
            }
        })
    }

    /// Opens an ordinary transaction on this connection, same as [`Pool::begin`] (MySQL `BEGIN`, SQLite
    /// `BEGIN IMMEDIATE`).
    pub async fn begin(self) -> Result<Tx<'static>, sqlx::Error> {
        Ok(match self.0.0 {
            PooledKind::MySql(connection) => {
                Tx::mysql(sqlx::Transaction::begin(connection, None).await?)
            }
            PooledKind::Sqlite(connection) => Tx::sqlite(
                sqlx::Transaction::begin(connection, Some("BEGIN IMMEDIATE".into())).await?,
            ),
        })
    }

    /// Migrations run on this connection (the SQLx migrator needs the backend's own connection).
    pub(super) fn backend(&mut self) -> BackendConn<'_> {
        match &mut self.0.0 {
            PooledKind::MySql(connection) => BackendConn::MySql(connection),
            PooledKind::Sqlite(connection) => BackendConn::Sqlite(connection),
        }
    }
}

pub(super) enum BackendConn<'a> {
    MySql(&'a mut MySqlConnection),
    Sqlite(&'a mut SqliteConnection),
}

impl Deref for PoolConn {
    type Target = PooledConn;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PoolConn {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl PooledConn {
    /// Only for MySQL-specific diagnostic and test code.
    pub fn as_mysql(&mut self) -> Option<&mut MySqlConnection> {
        match &mut self.0 {
            PooledKind::MySql(connection) => Some(connection),
            PooledKind::Sqlite(_) => None,
        }
    }
}

/// Anything that can execute `db::query`: the pool, the connection inside a transaction, a connection taken from the
/// pool, and the [`ObservedExecutor`] wrapping them with debug recording.
pub trait Executor<'e>: Send {
    #[doc(hidden)]
    fn target(self) -> Target<'e>;
}

/// The backend connection one execution lands on. The fields are visible only inside the `db` module.
#[doc(hidden)]
pub struct Target<'e> {
    pub(super) observed: bool,
    pub(super) kind: TargetKind<'e>,
}

pub(super) enum TargetKind<'e> {
    MySqlPool(&'e MySqlPool),
    MySqlConn(&'e mut MySqlConnection),
    /// Write statements go to the write pool and read statements to the read pool, decided by the execution method.
    SqlitePool(&'e SqlitePools),
    SqliteConn(&'e mut SqliteConnection),
}

impl<'e> Executor<'e> for &'e Pool {
    fn target(self) -> Target<'e> {
        let kind = match &self.0 {
            PoolKind::MySql(pool) => TargetKind::MySqlPool(pool),
            PoolKind::Sqlite(pools) => TargetKind::SqlitePool(pools),
        };
        Target {
            observed: false,
            kind,
        }
    }
}

impl<'e, 'c: 'e> Executor<'e> for &'e mut TxConn<'c> {
    fn target(self) -> Target<'e> {
        let kind = match &mut self.0 {
            TxKind::MySql(transaction) => TargetKind::MySqlConn(transaction),
            TxKind::Sqlite(transaction) => TargetKind::SqliteConn(transaction),
        };
        Target {
            observed: false,
            kind,
        }
    }
}

impl<'e> Executor<'e> for &'e mut PooledConn {
    fn target(self) -> Target<'e> {
        let kind = match &mut self.0 {
            PooledKind::MySql(connection) => TargetKind::MySqlConn(connection),
            PooledKind::Sqlite(connection) => TargetKind::SqliteConn(connection),
        };
        Target {
            observed: false,
            kind,
        }
    }
}

impl<'e, 'c: 'e> Executor<'e> for &'e mut Tx<'c> {
    fn target(self) -> Target<'e> {
        (&mut self.0).target()
    }
}

impl<'e> Executor<'e> for &'e mut PoolConn {
    fn target(self) -> Target<'e> {
        (&mut self.0).target()
    }
}

/// Executor counted in the debug panel and footer counts: returned by `db.executor()` and `db.observe(..)`.
#[derive(Debug)]
pub struct ObservedExecutor<E> {
    inner: E,
}

impl<E> ObservedExecutor<E> {
    pub(super) fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<'e, E: Executor<'e>> Executor<'e> for ObservedExecutor<E> {
    fn target(self) -> Target<'e> {
        Target {
            observed: true,
            ..self.inner.target()
        }
    }
}
