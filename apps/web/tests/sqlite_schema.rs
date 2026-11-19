//! The SQLite backend: after each backend runs all migrations, tables, columns, primary keys, indexes, foreign keys
//! and seed rows match item by item; on the SQLite side also checks connection parameters and database constraints
//! (STRICT, length CHECK, foreign keys, Unicode lowercase, case-sensitive LIKE, no reuse of auto-increment values),
//! that Web startup refuses when the database file does not exist, the read/write pool split, the error on
//! write-connection self-locking, and that the rewriter sits on the execution path.
//!
//! Where the two sides are written differently they correspond by the rewrite rules of
//! db/migrations/sqlite/0001_baseline.sql: MySQL's varchar(n)/char(n) maps to SQLite's `CHECK (length(col) <= n)`,
//! text to `CHECK (length(CAST(col AS BLOB)) <= 65535)`, and index names to `<table>__<original name>`.

use std::collections::{BTreeMap, BTreeSet};

use gono_forum_web::{
    config::DatabaseConfig,
    db::{self, Database, DbRow},
    test_support::TestFixture,
};

#[derive(Debug, PartialEq, Eq)]
struct Column {
    name: String,
    integer: bool,
    nullable: bool,
    default: Option<String>,
    /// Character limit of varchar/char.
    chars: Option<i64>,
    /// Byte limit of text.
    bytes: Option<i64>,
    generated: bool,
    autoincrement: bool,
}

/// Foreign key: (local columns, referenced table, referenced columns, ON UPDATE, ON DELETE).
type ForeignKey = (Vec<String>, String, Vec<String>, String, String);

#[derive(Debug, Default, PartialEq, Eq)]
struct Table {
    columns: Vec<Column>,
    primary: Vec<String>,
    /// Index name (original MySQL name) → (unique or not, columns).
    indexes: BTreeMap<String, (bool, Vec<String>)>,
    foreign: BTreeSet<ForeignKey>,
}

type Schema = BTreeMap<String, Table>;

async fn rows(db: &Database, sql: &str) -> Vec<DbRow> {
    db::query(sql).fetch_all(&db.pool).await.unwrap()
}

fn text(row: &DbRow, column: &str) -> String {
    db::text(row, column).unwrap()
}

/// The two sides write foreign key actions differently: when unspecified MySQL reports RESTRICT (the same as NO ACTION
/// in InnoDB), SQLite reports NO ACTION.
fn rule(rule: &str) -> String {
    match rule {
        "RESTRICT" => "NO ACTION".into(),
        other => other.into(),
    }
}

async fn mysql_schema(db: &Database) -> Schema {
    let mut schema = Schema::new();
    for row in rows(
        db,
        "SELECT TABLE_NAME AS t, COLUMN_NAME AS c, DATA_TYPE AS ty, IS_NULLABLE AS nullable, COLUMN_DEFAULT AS dflt,
                CAST(CHARACTER_MAXIMUM_LENGTH AS SIGNED) AS chars, EXTRA AS extra
           FROM information_schema.COLUMNS
          WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME <> '_sqlx_migrations'
          ORDER BY TABLE_NAME, ORDINAL_POSITION",
    )
    .await
    {
        let kind = text(&row, "ty");
        let extra = text(&row, "extra");
        let chars: Option<i64> = row.try_get("chars").unwrap();
        schema.entry(text(&row, "t")).or_default().columns.push(Column {
            name: text(&row, "c"),
            integer: kind == "bigint",
            nullable: text(&row, "nullable") == "YES",
            default: row.try_get::<Option<String>, _>("dflt").unwrap(),
            // Generated columns get no length CHECK on the SQLite side: their values only come from a few fixed
            // strings in the expression.
            chars: if matches!(kind.as_str(), "varchar" | "char") && !extra.contains("GENERATED") {
                Some(chars.expect("varchar and char have a length"))
            } else {
                None
            },
            bytes: match kind.as_str() {
                "text" => Some(65535),
                "mediumtext" => Some(16_777_215),
                _ => None,
            },
            generated: extra.contains("GENERATED"),
            autoincrement: extra.contains("auto_increment"),
        });
    }
    for row in rows(
        db,
        "SELECT TABLE_NAME AS t, INDEX_NAME AS i, CAST(NON_UNIQUE AS SIGNED) AS non_unique, COLUMN_NAME AS c
           FROM information_schema.STATISTICS
          WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME <> '_sqlx_migrations'
          ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
    )
    .await
    {
        let table = schema.get_mut(&text(&row, "t")).unwrap();
        let (index, column) = (text(&row, "i"), text(&row, "c"));
        if index == "PRIMARY" {
            table.primary.push(column);
        } else {
            let unique = row.try_get::<i64, _>("non_unique").unwrap() == 0;
            table.indexes.entry(index).or_insert((unique, Vec::new())).1.push(column);
        }
    }
    let mut foreign: BTreeMap<(String, String), ForeignKey> = BTreeMap::new();
    for row in rows(
        db,
        "SELECT k.TABLE_NAME AS t, k.CONSTRAINT_NAME AS n, k.COLUMN_NAME AS c, k.REFERENCED_TABLE_NAME AS rt,
                k.REFERENCED_COLUMN_NAME AS rc, r.UPDATE_RULE AS upd, r.DELETE_RULE AS del
           FROM information_schema.KEY_COLUMN_USAGE k
           JOIN information_schema.REFERENTIAL_CONSTRAINTS r
             ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME
          WHERE k.TABLE_SCHEMA = DATABASE()
          ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
    )
    .await
    {
        let entry = foreign
            .entry((text(&row, "t"), text(&row, "n")))
            .or_insert_with(|| {
                (
                    Vec::new(),
                    text(&row, "rt"),
                    Vec::new(),
                    rule(&text(&row, "upd")),
                    rule(&text(&row, "del")),
                )
            });
        entry.0.push(text(&row, "c"));
        entry.2.push(text(&row, "rc"));
    }
    for ((table, _), key) in foreign {
        schema.get_mut(&table).unwrap().foreign.insert(key);
    }
    schema
}

/// SQLite's default value is the original text from the CREATE TABLE statement: `0`, `''`, `'pending'`, `NULL`.
fn sqlite_default(value: Option<String>) -> Option<String> {
    let value = value?;
    if value == "NULL" {
        return None;
    }
    Some(
        match value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            Some(quoted) => quoted.replace("''", "'"),
            None => value,
        },
    )
}

fn check_limit(sql: &str, pattern: &str) -> Option<i64> {
    let start = sql.find(pattern)? + pattern.len();
    let digits: String = sql[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some(digits.parse().unwrap())
}

async fn sqlite_schema(db: &Database) -> Schema {
    let mut schema = Schema::new();
    for table in rows(
        db,
        "SELECT name, sql FROM sqlite_master
          WHERE type = 'table' AND name NOT IN ('_sqlx_migrations', 'sqlite_sequence') ORDER BY name",
    )
    .await
    {
        let (name, sql) = (text(&table, "name"), text(&table, "sql"));
        assert!(sql.ends_with(" STRICT"), "{name} is not a STRICT table");
        let mut entry = Table::default();
        let mut primary = Vec::new();
        for column in db::query(
            "SELECT name, type, \"notnull\" AS nn, dflt_value, pk, hidden FROM pragma_table_xinfo(?) ORDER BY cid",
        )
        .written_for(db::Backend::Sqlite)
        .bind(&name)
        .fetch_all(&db.pool)
        .await
        .unwrap()
        {
            let column_name = text(&column, "name");
            let kind = text(&column, "type");
            assert!(matches!(kind.as_str(), "INTEGER" | "TEXT"), "{name}.{column_name}: {kind}");
            let pk: i64 = column.try_get("pk").unwrap();
            if pk > 0 {
                primary.push((pk, column_name.clone()));
            }
            let quoted = format!("`{column_name}`");
            let autoincrement = sql.contains(&format!("{quoted} INTEGER PRIMARY KEY AUTOINCREMENT"));
            entry.columns.push(Column {
                integer: kind == "INTEGER",
                // INTEGER PRIMARY KEY is an alias of rowid; the pragma reports it nullable, but it is never NULL.
                nullable: column.try_get::<i64, _>("nn").unwrap() == 0 && !autoincrement,
                default: sqlite_default(column.try_get("dflt_value").unwrap()),
                chars: check_limit(&sql, &format!("CHECK (length({quoted}) <= ")),
                bytes: check_limit(&sql, &format!("CHECK (length(CAST({quoted} AS BLOB)) <= ")),
                generated: column.try_get::<i64, _>("hidden").unwrap() == 3,
                autoincrement,
                name: column_name,
            });
        }
        primary.sort();
        entry.primary = primary.into_iter().map(|(_, column)| column).collect();
        for index in db::query(
            "SELECT name, \"unique\" AS u, origin FROM pragma_index_list(?) ORDER BY name",
        )
        .written_for(db::Backend::Sqlite)
        .bind(&name)
        .fetch_all(&db.pool)
        .await
        .unwrap()
        {
            let origin = text(&index, "origin");
            if origin == "pk" {
                continue;
            }
            assert_eq!(origin, "c", "every index of {name} must come from CREATE INDEX");
            let index_name = text(&index, "name");
            let original = index_name
                .strip_prefix(&format!("{name}__"))
                .unwrap_or_else(|| panic!("index names must be <table>__<original name>: {index_name}"))
                .to_owned();
            let columns = db::query("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
                .bind(&index_name)
                .fetch_all(&db.pool)
                .await
                .unwrap()
                .iter()
                .map(|row| text(row, "name"))
                .collect();
            entry
                .indexes
                .insert(original, (index.try_get::<i64, _>("u").unwrap() == 1, columns));
        }
        let mut foreign: BTreeMap<i64, ForeignKey> = BTreeMap::new();
        for key in db::query(
            "SELECT id, \"table\" AS rt, \"from\" AS c, \"to\" AS rc, on_update, on_delete
               FROM pragma_foreign_key_list(?) ORDER BY id, seq",
        )
        .written_for(db::Backend::Sqlite)
        .bind(&name)
        .fetch_all(&db.pool)
        .await
        .unwrap()
        {
            let entry = foreign.entry(key.try_get("id").unwrap()).or_insert_with(|| {
                (
                    Vec::new(),
                    text(&key, "rt"),
                    Vec::new(),
                    text(&key, "on_update"),
                    text(&key, "on_delete"),
                )
            });
            entry.0.push(text(&key, "c"));
            entry.2.push(text(&key, "rc"));
        }
        entry.foreign = foreign.into_values().collect();
        schema.insert(name, entry);
    }
    schema
}

/// Non-empty tables listed row by row (integer columns as decimal, the rest as text), sorted by the first column.
async fn table_rows(db: &Database) -> BTreeMap<String, Vec<Vec<Option<String>>>> {
    let tables: Vec<String> = match db.backend() {
        db::Backend::MySql => rows(
            db,
            "SELECT TABLE_NAME AS name FROM information_schema.TABLES
              WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' AND TABLE_NAME <> '_sqlx_migrations'",
        )
        .await,
        db::Backend::Sqlite => rows(
            db,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT IN ('_sqlx_migrations', 'sqlite_sequence')",
        )
        .await,
    }
    .iter()
    .map(|row| text(row, "name"))
    .collect();
    let mut result = BTreeMap::new();
    for table in tables {
        let data: Vec<Vec<Option<String>>> =
            rows(db, &format!("SELECT * FROM `{table}` ORDER BY 1"))
                .await
                .iter()
                .map(|row| {
                    row.columns()
                        .iter()
                        .map(|column| {
                            if column.is_integer() {
                                row.try_get::<Option<i64>, _>(column.name())
                                    .unwrap()
                                    .map(|v| v.to_string())
                            } else {
                                row.try_get::<Option<String>, _>(column.name()).unwrap()
                            }
                        })
                        .collect()
                })
                .collect();
        if !data.is_empty() {
            result.insert(table, data);
        }
    }
    result
}

struct SqliteFile(std::path::PathBuf);

impl SqliteFile {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "gonof_sqlite_{label}_{}_{nanos}.db",
            std::process::id()
        )))
    }

    fn config(&self) -> DatabaseConfig {
        DatabaseConfig {
            url: format!("sqlite://{}", self.0.display()),
            max_connections: 4,
            acquire_timeout_seconds: 10,
        }
    }
}

impl Drop for SqliteFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

async fn migrated_sqlite(file: &SqliteFile) -> Database {
    let db = Database::connect_creating(&file.config()).await.unwrap();
    db.migrate().await.unwrap();
    db
}

#[tokio::test]
async fn both_backends_build_the_same_schema_and_seed_rows() {
    // Only checks the comparison of both sides (needs a MySQL database, so it runs in the MySQL round).
    gono_forum_web::mysql_only!();
    // Not `migrated()`: after migrating it also changes two settings the way an existing site would (forum URL, mail
    // sending), whereas this compares the baseline itself.
    let fixture = TestFixture::new().await.unwrap();
    fixture.db.migrate().await.unwrap();
    let file = SqliteFile::new("schema");
    let sqlite = migrated_sqlite(&file).await;

    let mysql_schema = mysql_schema(&fixture.db).await;
    let sqlite_schema = sqlite_schema(&sqlite).await;
    assert_eq!(
        mysql_schema.keys().collect::<Vec<_>>(),
        sqlite_schema.keys().collect::<Vec<_>>(),
        "tables on both sides"
    );
    // 82 tables in the baseline, one more each from 0002, 0003 and 0009.
    assert_eq!(mysql_schema.len(), 85);
    for (name, mysql) in &mysql_schema {
        assert_eq!(mysql, &sqlite_schema[name], "{name}");
    }
    let indexes: usize = sqlite_schema
        .values()
        .map(|table| table.indexes.len())
        .sum();
    // 171 in the baseline, one more each from 0003 and 0004, four more from 0009.
    assert_eq!(indexes, 177);

    let mysql_rows = table_rows(&fixture.db).await;
    let sqlite_rows = table_rows(&sqlite).await;
    assert_eq!(
        mysql_rows.keys().collect::<Vec<_>>(),
        [
            "gonof_common_content_revision",
            "gonof_common_credit_rule",
            "gonof_common_settings"
        ],
        "only these three tables have seed rows"
    );
    for (table, mysql) in &mysql_rows {
        let sqlite = &sqlite_rows[table];
        let differ: Vec<_> = mysql
            .iter()
            .zip(sqlite)
            .filter(|(left, right)| left != right)
            .take(5)
            .collect();
        assert!(
            differ.is_empty(),
            "seed rows of {table} differ (MySQL, SQLite): {differ:?}"
        );
        assert_eq!(mysql.len(), sqlite.len(), "seed row count of {table}");
    }
    // 143 seed rows in the baseline (settings 139, credit rules 3, content version 1), the layout document's three
    // settings rows from 0003 and the two notification retention settings from 0008.
    assert_eq!(mysql_rows.values().map(Vec::len).sum::<usize>(), 148);

    let objects: i64 =
        db::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type IN ('trigger', 'view')")
            .fetch_one(&sqlite.pool)
            .await
            .unwrap();
    assert_eq!(
        objects, 0,
        "the SQLite side has no triggers or views either"
    );
    let versions: Vec<i64> =
        db::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&sqlite.pool)
            .await
            .unwrap();
    assert_eq!(versions, (1..=db::migrations::LATEST).collect::<Vec<_>>());

    sqlite.pool.close().await;
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn sqlite_connections_enforce_the_schema_like_mysql_strict_mode() {
    let file = SqliteFile::new("rules");
    let db = migrated_sqlite(&file).await;
    let exec = |sql: &'static str| db::query(sql).execute(&db.pool);
    let scalar_text =
        |sql: &'static str| db::query_scalar::<Option<String>>(sql).fetch_one(&db.pool);

    // Connection parameters: WAL, sync on every commit, foreign key checks, the same for the write and read pools; the
    // read pool is additionally read-only.
    for (pool, query_only) in [
        (db.pool.as_sqlite().unwrap(), 0),
        (db.pool.as_sqlite_read().unwrap(), 1),
    ] {
        let number = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(number("PRAGMA synchronous").await, 2, "synchronous=FULL");
        assert_eq!(number("PRAGMA foreign_keys").await, 1);
        assert_eq!(number("PRAGMA query_only").await, query_only);
    }

    // varchar(190): counted in characters, so 190 three-byte characters are accepted and 191 refused. "\u{8349}" is
    // the Chinese character for "grass", three bytes in UTF-8.
    let ok = "\u{8349}".repeat(190);
    let long = "\u{8349}".repeat(191);
    db::query("INSERT INTO gonof_common_settings(skey, svalue) VALUES (?, '')")
        .bind(&ok)
        .execute(&db.pool)
        .await
        .unwrap();
    let error = db::query("INSERT INTO gonof_common_settings(skey, svalue) VALUES (?, '')")
        .bind(&long)
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );

    // text: counted in bytes, 21845 three-byte characters are exactly 65535 bytes, and one more is refused.
    db::query("UPDATE gonof_common_settings SET svalue = ? WHERE skey = ?")
        .bind("\u{8349}".repeat(21845))
        .bind(&ok)
        .execute(&db.pool)
        .await
        .unwrap();
    let error = db::query("UPDATE gonof_common_settings SET svalue = ? WHERE skey = ?")
        .bind("\u{8349}".repeat(21846))
        .bind(&ok)
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );

    // STRICT: integer columns do not accept text.
    let error = exec("INSERT INTO gonof_common_content_revision(id, revision) VALUES (2, 'abc')")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot store TEXT value in INTEGER column"),
        "{error}"
    );

    // Foreign keys: references to a nonexistent member or honor group are refused.
    let error = exec(
        "INSERT INTO gonof_common_member_honor(uid, honor_id, granted_by, granted_at) VALUES (999999, 1, 999999, 1)",
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "{error}"
    );

    // lower lowercases character by character per Unicode, leaving NULL as is; a word-final Σ also becomes σ (like
    // MySQL's LOWER, not turned into ς by context).
    // LIKE is case-sensitive (like utf8mb4_bin).
    assert_eq!(
        scalar_text("SELECT lower('ÄΣ ΟΔΟΣ Straße ABC')")
            .await
            .unwrap()
            .as_deref(),
        Some("äσ οδοσ straße abc")
    );
    assert_eq!(scalar_text("SELECT lower(NULL)").await.unwrap(), None);
    let like: i64 = db::query_scalar("SELECT 'Abc' LIKE 'abc'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(like, 0);
    let folded: i64 = db::query_scalar("SELECT lower('ÄBC') LIKE lower('äbc')")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(folded, 1);

    // Auto-increment does not reuse the deleted largest id (like MySQL 8), and inserting 0 still stores 0 (like
    // NO_AUTO_VALUE_ON_ZERO).
    let first =
        exec("INSERT INTO gonof_common_honor(name, description) VALUES ('Temporary A', '')")
            .await
            .unwrap()
            .last_insert_id();
    exec("DELETE FROM gonof_common_honor WHERE name = 'Temporary A'")
        .await
        .unwrap();
    let second =
        exec("INSERT INTO gonof_common_honor(name, description) VALUES ('Temporary B', '')")
            .await
            .unwrap()
            .last_insert_id();
    assert_eq!((first, second), (1, 2));
    exec("INSERT INTO gonof_common_honor(id, name, description) VALUES (0, 'Zero', '')")
        .await
        .unwrap();
    let zero: i64 = db::query_scalar("SELECT id FROM gonof_common_honor WHERE name = 'Zero'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(zero, 0);

    // A write transaction takes the write lock at the start (BEGIN IMMEDIATE); when another connection (another
    // process on the same machine) cannot get it, it reports BUSY, and the existing deadlock retry treats that as a
    // lock conflict.
    let holder = db.begin_write().await.unwrap();
    let options = (*db.pool.as_sqlite().unwrap().connect_options()).clone();
    let mut other: sqlx::SqliteConnection =
        sqlx::Connection::connect_with(&options.busy_timeout(std::time::Duration::ZERO))
            .await
            .unwrap();
    let error = sqlx::Executor::execute(&mut other, "BEGIN IMMEDIATE")
        .await
        .expect_err(
            "another write transaction is still open, so the write lock must not be granted",
        );
    assert!(db::is_lock_conflict(&error), "{error}");
    sqlx::Connection::close(other).await.unwrap();
    assert!(!db::is_lock_conflict(&sqlx::Error::RowNotFound));
    holder.rollback().await.unwrap();

    db.pool.close().await;
}

#[tokio::test]
async fn sqlite_pools_split_reads_from_writes_and_sqlite_arbitrates_the_write_lock() {
    let file = SqliteFile::new("pools");
    let db = migrated_sqlite(&file).await;

    // The rewriter sits on the execution path: locking reads, GREATEST and ODKU written as MySQL text still run on
    // SQLite.
    db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?) ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)")
        .bind("smoke.key")
        .bind("one")
        .execute(&db.pool)
        .await
        .unwrap();
    let upsert = db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?) ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)")
        .bind("smoke.key")
        .bind("two")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        upsert.rows_affected(),
        1,
        "an SQLite upsert that changes an existing row reports 1 (MySQL reports 2)"
    );
    let mut tx = db.begin_write().await.unwrap();
    let value: String =
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=? FOR UPDATE")
            .bind("smoke.key")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(value, "two");
    let larger: i64 = db::query_scalar("SELECT GREATEST(?, 7)")
        .bind(3_i64)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(larger, 7);
    // Hand-written SQLite text takes precedence over rewriting.
    let now: i64 = db::query_scalar("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
        .sqlite("SELECT unixepoch()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(now > 1_700_000_000);
    // Constructs the rewriter does not recognize fail immediately and are never sent to SQLite.
    let error = db::query_scalar::<i64>("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
        .fetch_one(&mut *tx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("UNIX_TIMESTAMP"), "{error}");

    // SQLite arbitrates the write lock: this task holding a write transaction while writing
    // to the database from the pool is a true self-lock; it reports BUSY after the 5-second lock wait, and the existing
    // retry treats it as a lock conflict (like MySQL's lock wait timeout 1205), so it never hangs.
    let started = std::time::Instant::now();
    let error = db::query("DELETE FROM gonof_common_settings WHERE skey='smoke.key'")
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert!(db::is_lock_conflict(&error), "{error}");
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(4) && waited < std::time::Duration::from_secs(15),
        "{waited:?}"
    );
    // Reads use the read pool and are unaffected; read pool connections are read-only.
    let count: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_settings WHERE skey=?")
        .bind("smoke.key")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let error =
        db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES('x','') RETURNING skey")
            .fetch_optional(&db.pool)
            .await
            .unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
    tx.commit().await.unwrap();

    // After commit the write-holder registration is cleared, so the same task can keep writing. Read-only transactions
    // use the read pool and cannot write inside.
    db::query("DELETE FROM gonof_common_settings WHERE skey='smoke.key'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut read = db.begin_read_only().await.unwrap();
    let error = db::query("DELETE FROM gonof_common_settings WHERE skey='x'")
        .execute(&mut *read)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
    read.rollback().await.unwrap();
    // Two concurrent write transactions in the same task just queue: whichever gets the write lock first finishes and
    // commits, then the other gets the lock.
    let write = |key: &'static str| {
        let db = db.clone();
        async move {
            let mut tx = db.begin_write().await?;
            db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,'')")
                .bind(key)
                .execute(&mut *tx)
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            tx.commit().await?;
            anyhow::Ok(())
        }
    };
    let (first, second) = tokio::join!(write("join.first"), write("join.second"));
    first.unwrap();
    second.unwrap();
    // While another task holds a write transaction, this task queues and waits without an error.
    let holder = db.begin_write().await.unwrap();
    let waiter = tokio::spawn({
        let db = db.clone();
        async move {
            db::query("DELETE FROM gonof_common_settings WHERE skey='x'")
                .execute(&db.pool)
                .await
                .map(|result| result.rows_affected())
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !waiter.is_finished(),
        "another task holds the write lock, so this task must wait for it"
    );
    holder.commit().await.unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), 0);

    db.pool.close().await;
}

#[tokio::test]
async fn the_web_entry_refuses_a_missing_sqlite_file_and_migrate_creates_it() {
    // The refusal is shown by the setup wizard, so it is a translation (docs/architecture.md, "Languages"); outside a
    // request it is English.
    let file = SqliteFile::new("missing");
    let error = Database::connect(&file.config()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("For a first installation, run gono-forum-cli migrate to create it"),
        "{error:#}"
    );
    assert!(
        !file.0.exists(),
        "the web entry must not create the database file"
    );

    let db = migrated_sqlite(&file).await;
    assert!(file.0.exists());
    // Once built, the Web entry can connect, and rerunning migrations does nothing.
    db.pool.close().await;
    let again = Database::connect(&file.config()).await.unwrap();
    again.migrate().await.unwrap();
    assert_eq!(again.backend(), db::Backend::Sqlite);
    again.pool.close().await;

    for url in [
        "sqlite::memory:",
        "sqlite://",
        "sqlite://file.db?mode=memory",
        "postgres://x/y",
    ] {
        let config = DatabaseConfig {
            url: url.into(),
            max_connections: 1,
            acquire_timeout_seconds: 1,
        };
        assert!(config.validate().is_err(), "{url}");
    }
}
