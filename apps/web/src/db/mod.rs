//! The database is the only structured source of truth. Redis contains rebuildable caches.
//!
//! Callers only use this module's types ([`Database`], [`Tx`], [`DbRow`], [`QueryResult`], [`Executor`]), never
//! SQLx's backend types directly. The backend is decided at runtime by the connection string: `mysql://` or
//! `sqlite://`. Business SQL is written as MySQL text and rewritten by [`dialect`] on
//! SQLite; what cannot be rewritten is written by hand at the call site with `.sqlite(…)`.
mod conn;
pub mod dialect;
pub mod migrations;
mod observed;
mod queries;
mod rows;
#[cfg(test)]
mod source_guards;
pub mod sql_display;
mod sqlite;
#[cfg(test)]
mod sqlite_statements;

use conn::Begin;
pub use conn::{Executor, ObservedExecutor, Pool, PoolConn, PooledConn, Target, Tx, TxConn};
pub use queries::{
    Bindable, Query, QueryResult, QueryScalar, RawSql, SqlParameter, query, query_scalar, raw_sql,
};
pub use rows::{ColumnName, DbColumn, DbRow, Decodable};

use crate::config::DatabaseConfig;
use anyhow::{Context, Result, ensure};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use std::{str::FromStr, time::Duration};

pub fn text(row: &DbRow, column: &str) -> Result<String> {
    // MySQL marks TEXT using a binary collation as BLOB/BINARY in result metadata.
    // Callers identify schema text explicitly; the String decoder still validates UTF-8.
    Ok(row.text(column)?)
}

/// Deadlock or lock wait timeout (MySQL 1213, 1205; SQLite BUSY, LOCKED): the whole transaction was rolled back
/// and can be retried from the start.
pub fn is_lock_conflict(error: &sqlx::Error) -> bool {
    let Some(error) = error.as_database_error() else {
        return false;
    };
    if let Some(error) = error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
        return matches!(error.number(), 1213 | 1205);
    }
    error
        .try_downcast_ref::<sqlx::sqlite::SqliteError>()
        .is_some_and(sqlite::is_lock_conflict)
}

/// Whether a unique constraint violation hit this unique key. MySQL's error names the key (`… for key
/// 'table.key_name'`), SQLite's error names the columns (`UNIQUE constraint failed: table.col, table.col`), so both
/// must be given: `key` is the key name and `columns` are this key's "table.col".
pub fn unique_violation_on(error: &sqlx::Error, key: &str, columns: &[&str]) -> bool {
    let Some(error) = error
        .as_database_error()
        .filter(|error| error.is_unique_violation())
    else {
        return false;
    };
    let message = error.message();
    if error
        .try_downcast_ref::<sqlx::sqlite::SqliteError>()
        .is_some()
    {
        let failed: Vec<&str> = message
            .rsplit_once(": ")
            .map(|(_, list)| list.split(", ").collect())
            .unwrap_or_default();
        return failed.len() == columns.len()
            && columns.iter().all(|column| failed.contains(column));
    }
    message.contains(&format!(".{key}'")) || message.ends_with(&format!("'{key}'"))
}

/// The connection string decides the backend at runtime: `mysql://` or `sqlite://`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    MySql,
    Sqlite,
}

impl Backend {
    pub fn of(url: &str) -> Option<Self> {
        if url.starts_with("mysql://") {
            Some(Self::MySql)
        } else if url.starts_with("sqlite:") {
            Some(Self::Sqlite)
        } else {
            None
        }
    }

    /// The backend name shown in the debug panel, the footer and the health check.
    pub fn label(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::Sqlite => "SQLite",
        }
    }

    /// The backend for an SQLx driver name (`sqlx::Database::NAME`), for debug records that only have a generic
    /// executor.
    pub(crate) fn of_driver(name: &str) -> Self {
        if name == <sqlx::Sqlite as sqlx::Database>::NAME {
            Self::Sqlite
        } else {
            Self::MySql
        }
    }
}

#[derive(Clone, Debug)]
pub struct Database {
    pub pool: Pool,
}

impl Database {
    /// Used by the Web server and most commands: refuses to start when the SQLite database file does not exist.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self> {
        Self::open(config, false).await
    }

    /// Only for database-creating commands such as `gono-forum-cli migrate`: creates the SQLite database file when it
    /// does not exist (MySQL still requires the database to exist).
    pub async fn connect_creating(config: &DatabaseConfig) -> Result<Self> {
        Self::open(config, true).await
    }

    pub fn backend(&self) -> Backend {
        if self.pool.as_mysql().is_some() {
            Backend::MySql
        } else {
            Backend::Sqlite
        }
    }

    async fn open(config: &DatabaseConfig, create: bool) -> Result<Self> {
        config.validate()?;
        if Backend::of(&config.url) == Some(Backend::Sqlite) {
            let (write, read) = sqlite::connect(config, create).await?;
            let database = Self {
                pool: Pool::from_sqlite(write, read),
            };
            let version: String = query_scalar("SELECT sqlite_version()")
                .fetch_one(database.executor())
                .await?;
            ensure!(
                sqlite::supported_version(&version),
                "{}",
                crate::i18n::current().setup_config_sqlite_version(version.as_str())
            );
            return Ok(database);
        }
        Self::connect_mysql(config).await
    }

    async fn connect_mysql(config: &DatabaseConfig) -> Result<Self> {
        let options = MySqlConnectOptions::from_str(&config.url)?
            .charset("utf8mb4")
            .collation("utf8mb4_bin");
        let pool = MySqlPoolOptions::new()
            .min_connections(1).max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_seconds))
            .after_connect(|connection, _| Box::pin(async move {
                // Connection setup is driver work. Its SQLx event is captured by the
                // outer acquiring query's driver span, without a second application record.
                sqlx::query("SET SESSION sql_mode = CONCAT(@@sql_mode, ',STRICT_TRANS_TABLES,NO_AUTO_VALUE_ON_ZERO')")
                    .execute(&mut *connection).await?;
                Ok(())
            }))
            .connect_with(options).await.context(crate::i18n::current().setup_config_mysql_connect())?;
        let database = Self {
            pool: Pool::from_mysql(pool),
        };
        let version: String = query_scalar("SELECT VERSION()")
            .fetch_one(database.executor())
            .await?;
        ensure!(
            supported_version(&version),
            "{}",
            crate::i18n::current().setup_config_mysql_version(version.as_str())
        );
        Ok(database)
    }

    pub fn executor(&self) -> ObservedExecutor<&Pool> {
        self.observe(&self.pool)
    }
    pub fn observe<E>(&self, executor: E) -> ObservedExecutor<E> {
        ObservedExecutor::new(executor)
    }
    pub async fn begin_write(&self) -> Result<Tx<'static>> {
        Ok(self.pool.begin_for(Begin::Write).await?)
    }

    /// Notification candidates and recomputed thread counters must be read after acquiring
    /// the content lock, so each statement sees rows committed while this one waited.
    /// MySQL uses READ COMMITTED; SQLite write transactions are serialized as a whole, so after taking the write
    /// lock at the start they see the latest commits.
    pub(crate) async fn begin_content_write(&self) -> Result<Tx<'static>> {
        Ok(self.pool.begin_for(Begin::ContentWrite).await?)
    }

    /// Read-only transaction: sees the same data within one snapshot (the snapshot starts at the first read).
    pub async fn begin_read_only(&self) -> Result<Tx<'static>> {
        Ok(self.pool.begin_for(Begin::ReadOnly).await?)
    }

    /// Read-only consistent snapshot: MySQL fixes the snapshot when the transaction starts and closes the connection
    /// after use; SQLite's snapshot starts at the first read.
    pub async fn begin_snapshot(&self) -> Result<Tx<'static>> {
        Ok(self.pool.begin_for(Begin::Snapshot).await?)
    }

    /// An ordinary write transaction, but the connection is closed after use instead of going back to the pool:
    /// session variables changed in the transaction cannot reach other requests through the connection.
    pub async fn begin_write_closing(&self) -> Result<Tx<'static>> {
        Ok(self.pool.begin_for(Begin::WriteClosing).await?)
    }

    pub async fn migrate(&self) -> Result<()> {
        migrations::run(self, migrations::LATEST).await
    }
    /// Tables in the connected database, for the setup wizard: an install starts from an empty database, or resumes
    /// one that only an interrupted wizard has migrated. Returns the table count and whether the migration table is
    /// among them.
    pub async fn table_census(&self) -> Result<(i64, bool)> {
        let (total, migrations) = match self.backend() {
            Backend::MySql => (
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = '_sqlx_migrations'",
            ),
            Backend::Sqlite => (
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
            ),
        };
        let total: i64 = query_scalar(total).fetch_one(self.executor()).await?;
        let migrations: i64 = query_scalar(migrations).fetch_one(self.executor()).await?;
        Ok((total, migrations > 0))
    }

    /// For tests: only run migrations up to number `version`, to seed data on the old schema and then verify the
    /// upgrade result of the new migration.
    #[doc(hidden)]
    pub async fn migrate_until(&self, version: i64) -> Result<()> {
        migrations::run(self, version).await
    }
}

fn supported_version(version: &str) -> bool {
    if version.to_ascii_lowercase().contains("mariadb") {
        return false;
    }
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let Some(patch) = parts
        .next()
        .and_then(|part| part.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|part| part.parse::<u32>().ok())
    else {
        return false;
    };
    (major, minor, patch) >= (5, 7, 44)
}

#[cfg(test)]
mod tests {
    use super::supported_version;
    #[test]
    fn requires_the_mysql_compatibility_floor() {
        for version in ["5.7.44", "5.7.44-log", "8.0.36", "8.4.0", "9.0.0"] {
            assert!(supported_version(version), "{version}");
        }
        for version in ["5.7.43", "5.6.51", "10.11.6-MariaDB", "invalid", "8.0"] {
            assert!(!supported_version(version), "{version}");
        }
    }
}
