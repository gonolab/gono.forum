//! Connections for the SQLite backend: single-machine deployment, with the database file on a local disk.
//!
//! Every connection: WAL, `synchronous=FULL` (every commit is flushed to disk, matching InnoDB's default durability),
//! foreign key checks on, 5-second lock wait, case-sensitive LIKE (like MySQL's utf8mb4_bin), and the built-in `lower`
//! replaced by a version that lowercases by Unicode rules: SQLite's own only knows ASCII, while title search and
//! username filters must be case-insensitive for letters such as Ä and Σ.
use std::{ffi::c_int, str::FromStr, time::Duration};

use anyhow::{Context, Result, ensure};
use libsqlite3_sys as ffi;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous,
};

use crate::config::DatabaseConfig;

/// The bundled SQLite is 3.46.0; we use STRICT tables, generated columns, `unixepoch()`, `->>`, RETURNING and
/// `UPDATE … FROM`.
const MINIMUM: (u32, u32, u32) = (3, 46, 0);

pub(super) fn options(url: &str) -> Result<SqliteConnectOptions> {
    Ok(SqliteConnectOptions::from_str(url)
        .context(crate::i18n::current().setup_config_sqlite_url())?
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .pragma("case_sensitive_like", "ON"))
}

/// Returns (write pool, read pool), both sized by `database.max_connections`. Read pool connections are read-only
/// (`query_only`). SQLite arbitrates who gets the write lock: a write transaction starts with `BEGIN IMMEDIATE`, and
/// if it cannot get the lock it waits up to 5 seconds (`busy_timeout`); the wait happens in each connection's own
/// background thread and does not block the request holding the lock.
///
/// Only `gono-forum-cli migrate` passes true for `create`: the web server refuses to start
/// when the file does not exist, so a wrong path does not quietly create an empty forum.
pub(super) async fn connect(
    config: &DatabaseConfig,
    create: bool,
) -> Result<(SqlitePool, SqlitePool)> {
    let options = options(&config.url)?;
    let file = options.get_filename().to_owned();
    ensure!(
        create || file.exists(),
        "{}",
        crate::i18n::current().setup_config_sqlite_missing(file.display().to_string())
    );
    let pool = |max: u32| {
        SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(max)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_seconds))
            .after_connect(|connection, _| Box::pin(register_lower(connection)))
    };
    let write = pool(config.max_connections)
        .connect_with(options.clone().create_if_missing(create))
        .await
        .with_context(|| {
            crate::i18n::current().setup_config_sqlite_connect(file.display().to_string())
        })?;
    let read = pool(config.max_connections)
        .connect_with(options.pragma("query_only", "ON"))
        .await
        .with_context(|| {
            crate::i18n::current().setup_config_sqlite_read_connect(file.display().to_string())
        })?;
    Ok((write, read))
}

pub(super) fn supported_version(version: &str) -> bool {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) => {
            (major, minor, patch) >= MINIMUM
        }
        _ => false,
    }
}

async fn register_lower(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    let mut handle = connection.lock_handle().await?;
    let database = handle.as_raw_handle().as_ptr();
    // SAFETY: `database` is a valid handle while this connection's lock is held; the function name is a
    // NUL-terminated static string; the callback only reads its arguments and, via SQLITE_TRANSIENT, lets SQLite copy
    // the result itself, keeping no pointers. Registering the same name with the same argument count overrides the
    // built-in lower.
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            database,
            c"lower".as_ptr(),
            1,
            ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC,
            std::ptr::null_mut(),
            Some(unicode_lower),
            None,
            None,
            None,
        )
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(sqlx::Error::Configuration(
            format!("cannot register the SQLite function lower: error code {code}").into(),
        ))
    }
}

/// `lower(X)`: NULL is returned as NULL; anything else is taken as text (numbers are converted to text first, as the
/// built-in lower does) and lowercased character by character by Unicode rules.
unsafe extern "C" fn unicode_lower(
    context: *mut ffi::sqlite3_context,
    count: c_int,
    values: *mut *mut ffi::sqlite3_value,
) {
    // SAFETY: SQLite calls this with the argument count given at registration (1), and `values` points to `count`
    // valid values; the text pointer is valid for this call, and its length comes from sqlite3_value_bytes (which
    // must be called after taking the text).
    unsafe {
        if count != 1 {
            ffi::sqlite3_result_error(context, c"lower takes exactly one argument".as_ptr(), -1);
            return;
        }
        let value = *values;
        if ffi::sqlite3_value_type(value) == ffi::SQLITE_NULL {
            ffi::sqlite3_result_null(context);
            return;
        }
        let text = ffi::sqlite3_value_text(value);
        if text.is_null() {
            ffi::sqlite3_result_error_nomem(context);
            return;
        }
        let length = usize::try_from(ffi::sqlite3_value_bytes(value)).unwrap_or(0);
        // Lowercase character by character, not with str::to_lowercase: that one turns a word-final Σ into ς by
        // context, which MySQL's LOWER does not do.
        let lowered: String = String::from_utf8_lossy(std::slice::from_raw_parts(text, length))
            .chars()
            .flat_map(char::to_lowercase)
            .collect();
        let Ok(length) = c_int::try_from(lowered.len()) else {
            ffi::sqlite3_result_error_toobig(context);
            return;
        };
        ffi::sqlite3_result_text(
            context,
            lowered.as_ptr().cast(),
            length,
            ffi::SQLITE_TRANSIENT(),
        );
    }
}

/// Lock wait timed out or the table is locked (`SQLITE_BUSY`, `SQLITE_LOCKED` and their extended codes): the
/// transaction has been rolled back and can be retried from the start.
pub(super) fn is_lock_conflict(error: &sqlx::sqlite::SqliteError) -> bool {
    use sqlx::error::DatabaseError;
    error
        .code()
        .and_then(|code| code.parse::<c_int>().ok())
        .is_some_and(|code| matches!(code & 0xff, ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED))
}

#[cfg(test)]
mod tests {
    use super::supported_version;

    #[test]
    fn requires_the_bundled_sqlite_floor() {
        for version in ["3.46.0", "3.46.1", "3.51.0", "4.0.0"] {
            assert!(supported_version(version), "{version}");
        }
        for version in ["3.45.3", "3.8.11", "3.46", "invalid"] {
            assert!(!supported_version(version), "{version}");
        }
    }
}
