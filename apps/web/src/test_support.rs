//! Integration test fixtures for independent services. Missing configuration must fail; no in-memory database
//! substitute is provided.
//!
//! Connects to MySQL by default (`GONOF_TEST_MYSQL_URL` or the test configuration); with `GONOF_TEST_BACKEND=sqlite`
//! each fixture creates its own SQLite database file `gonof_test_<pid>_<nanoseconds>_<sequence>.db` in the system
//! temporary directory and deletes it afterwards.
use crate::db::Tx;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use sqlx::{MySqlPool, mysql::MySqlPoolOptions};

use crate::{
    cache::Cache,
    config::{Config, RedisConfig},
    db::Database,
    project_root,
};

/// The versions of the terms of use and the privacy notice a registration form carries in its hidden fields, as the
/// pairs to submit with it (empty when the form has no such field).
pub fn policy_fields(html: &str) -> [(&'static str, String); 2] {
    let value = |name: &str| {
        let marker = format!("name=\"{name}\" value=\"");
        html.split_once(&marker)
            .and_then(|(_, rest)| rest.split('"').next())
            .unwrap_or_default()
            .to_owned()
    };
    [
        ("terms_version", value("terms_version")),
        ("privacy_version", value("privacy_version")),
    ]
}

/// Each fixture uses its own MySQL database, Redis namespace and local mail/media directories.
/// Takes the host and port part of the Origin.
///
/// The same-origin check compares the request's own `Host` with the origin's host and port, so the two must match when
/// building requests — a real HTTP/1.1 client always sends `Host`, but requests assembled for `oneshot` do not.
pub fn host_of(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, rest)| rest)
}

pub struct TestFixture {
    pub config: Config,
    pub db: Database,
    pub cache: Cache,
    /// This fixture's global process cache handle (not subscribed). Router tests use `router::build_with_global` to
    /// install it into the application, and pass the same copy when calling save functions directly, so the
    /// application's next request sees the new value after saving.
    pub global: crate::cache::GlobalCache,
    pub root: PathBuf,
    isolation: Isolation,
    name: String,
    cleaned: AtomicBool,
}

/// Isolated database: on MySQL a separate database created by the admin connection, on SQLite a separate database file.
enum Isolation {
    MySql { admin: MySqlPool, admin_url: String },
    Sqlite { file: PathBuf },
}

/// Tests connect to SQLite when `GONOF_TEST_BACKEND=sqlite`.
pub fn sqlite_tests() -> bool {
    std::env::var("GONOF_TEST_BACKEND").as_deref() == Ok("sqlite")
}

/// Cases that only verify MySQL mechanisms (execution plans, information_schema, MySQL error codes in trigger-style
/// fault injection, session variables, and so on) are skipped on SQLite: they print one line of explanation and return
/// directly.
#[macro_export]
macro_rules! mysql_only {
    () => {
        if $crate::test_support::sqlite_tests() {
            eprintln!("checks a MySQL mechanism only; skipped on SQLite");
            return;
        }
    };
}

fn remove_sqlite_files(file: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", file.display()));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl TestFixture {
    pub async fn new() -> Result<Self> {
        let mut config = Config::load_test(&project_root())?;
        let name = unique_name();
        config.redis.namespace = name.clone();
        let cache = Cache::connect(&config.redis).await?;
        if sqlite_tests() {
            let file = std::env::temp_dir().join(format!("{name}.db"));
            config.database.url = format!("sqlite://{}", file.display());
            let db = Database::connect_creating(&config.database)
                .await
                .context("cannot create the SQLite test database")?;
            let root = std::env::temp_dir().join(&name);
            if let Err(error) = std::fs::create_dir_all(&root) {
                db.pool.close().await;
                let _ = remove_sqlite_files(&file);
                return Err(error.into());
            }
            config.identity.local_directory = ".local".into();
            config.media.local_directory = ".local/media".into();
            let global = crate::cache::GlobalCache::new(cache.clone());
            return Ok(Self {
                config,
                db,
                cache,
                global,
                root,
                isolation: Isolation::Sqlite { file },
                name,
                cleaned: AtomicBool::new(false),
            });
        }
        let admin_url = config.database.url.clone();
        let admin = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(
                config.database.acquire_timeout_seconds,
            ))
            .connect(&config.database.url)
            .await
            .context("cannot connect to the MySQL test server")?;
        sqlx::query(&format!(
            "CREATE DATABASE `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_bin"
        ))
        .execute(&admin)
        .await
        .context("cannot create the isolated test database; check the CREATE DATABASE privilege")?;
        let mut url = url::Url::parse(&config.database.url)?;
        url.set_path(&name);
        config.database.url = url.to_string();
        let db = match Database::connect(&config.database).await {
            Ok(db) => db,
            Err(error) => {
                let _ = sqlx::query(&format!("DROP DATABASE `{name}`"))
                    .execute(&admin)
                    .await;
                admin.close().await;
                return Err(error.context("cannot connect to the isolated test database"));
            }
        };
        let root = std::env::temp_dir().join(&name);
        if let Err(error) = std::fs::create_dir_all(&root) {
            db.pool.close().await;
            let _ = sqlx::query(&format!("DROP DATABASE `{name}`"))
                .execute(&admin)
                .await;
            admin.close().await;
            return Err(error.into());
        }
        config.identity.local_directory = ".local".into();
        config.media.local_directory = ".local/media".into();
        let global = crate::cache::GlobalCache::new(cache.clone());
        Ok(Self {
            config,
            db,
            cache,
            global,
            root,
            isolation: Isolation::MySql { admin, admin_url },
            name,
            cleaned: AtomicBool::new(false),
        })
    }

    /// Runs the migrations and writes the forum URL as [`TEST_SITE_URL`]: mail and other work outside a request need
    /// a forum URL, and the baseline seeds it empty (the setup wizard asks for it). Cases testing "forum URL left
    /// empty" set it back to an empty string themselves.
    ///
    /// The email sending switch (`settings.email.enabled`) likewise: the baseline seeds it as off (a new site has not
    /// configured email yet), and the fixture turns it on. Cases testing "stop sending when off" turn it off
    /// themselves.
    ///
    /// Finally the site is set up as an English site with its own brand words ([`restore_sample_site`]), so tests
    /// assert English text (docs/translating.md, "For developers: adding interface text").
    pub async fn migrated() -> Result<Self> {
        let fixture = Self::new().await?;
        if let Err(error) = fixture.db.migrate().await {
            let _ = fixture.cleanup().await;
            return Err(error.context("cannot migrate the isolated test database"));
        }
        set_site_url(&fixture.db, TEST_SITE_URL).await?;
        set_setting(&fixture.db, "settings.email.enabled", "true").await?;
        restore_sample_site(&fixture.db).await?;
        Ok(fixture)
    }

    /// Explicit cleanup can report cleanup failures as test failures; when not called, Drop backstops synchronously,
    /// see the notes on `Drop`.
    pub async fn cleanup(self) -> Result<()> {
        self.cleanup_ref().await
    }

    /// Outer HTTP/media fixtures can clean up explicitly after assertions without moving their still-live
    /// Router/Database fields.
    pub async fn cleanup_ref(&self) -> Result<()> {
        if self.cleaned.load(Ordering::Acquire) {
            return Ok(());
        }
        let redis_result = self.cache.clear_namespace().await;
        self.db.pool.close().await;
        let database_result = match &self.isolation {
            Isolation::MySql { admin, .. } => {
                let dropped = sqlx::query(&format!("DROP DATABASE `{}`", self.name))
                    .execute(admin)
                    .await
                    .map(drop)
                    .map_err(anyhow::Error::from);
                admin.close().await;
                dropped
            }
            Isolation::Sqlite { file } => remove_sqlite_files(file).map_err(anyhow::Error::from),
        };
        let files_result = std::fs::remove_dir_all(&self.root);
        database_result.context("cannot drop the isolated test database")?;
        files_result.context("cannot remove the test's local files")?;
        redis_result.context("cannot clear the test's Redis namespace")?;
        self.cleaned.store(true, Ordering::Release);
        Ok(())
    }
}

/// The Drop backstop cleanup waits at most this long; on timeout it gives up with a notice, so one bad fixture cannot
/// stall the whole test run.
const DROP_CLEANUP_LIMIT: Duration = Duration::from_secs(60);

impl Drop for TestFixture {
    fn drop(&mut self) {
        if self.cleaned.load(Ordering::Acquire) {
            return;
        }
        // Cases without an explicit cleanup and panics all go through here. It must wait for cleanup to finish before
        // returning: a detached thread would be killed outright when the test process exits, leaking the database.
        //
        // Cleanup runs in a separate thread and runtime, because the current thread is most likely driving a Tokio
        // runtime and cannot block_on on top of it. While waiting, the original runtime is stalled, so the pending
        // ROLLBACKs in the connection pool cannot be sent, and uncommitted transactions keep holding metadata locks;
        // so first KILL the connections pointing at this database, then DROP.
        let database = match &self.isolation {
            Isolation::MySql { admin_url, .. } => Ok(admin_url.clone()),
            Isolation::Sqlite { file } => Err(file.clone()),
        };
        let name = self.name.clone();
        let redis = self.config.redis.clone();
        let root = self.root.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(drop_isolated(&database, &name, &redis, &root));
        });
        match receiver.recv_timeout(DROP_CLEANUP_LIMIT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!(
                "test fixture cleanup did not finish: {}: {error:#}; check that database and Redis namespace",
                self.name
            ),
            Err(_) => eprintln!(
                "test fixture cleanup timed out or was interrupted: {}; check that database and Redis namespace",
                self.name
            ),
        }
    }
}

/// `database`: on MySQL the admin connection string, on SQLite the database file. The SQLite database file is deleted
/// directly: the connection pool stalls with the original runtime, and the file can be deleted even while its handles
/// are still open in the process (on Unix it is the directory entry that is deleted).
fn drop_isolated(
    database: &std::result::Result<String, PathBuf>,
    name: &str,
    redis: &RedisConfig,
    root: &Path,
) -> Result<()> {
    let url = match database {
        Ok(url) => url,
        Err(file) => {
            remove_sqlite_files(file)?;
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    Cache::connect(redis).await?.clear_namespace().await?;
                    if root.exists() {
                        std::fs::remove_dir_all(root)?;
                    }
                    Ok(())
                });
        }
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let pool = MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(10))
                .connect(url)
                .await?;
            sqlx::query("SET SESSION lock_wait_timeout=10")
                .execute(&pool)
                .await?;
            let connections: Vec<u64> = sqlx::query_scalar(
                "SELECT ID FROM information_schema.PROCESSLIST WHERE DB=? AND ID<>CONNECTION_ID()",
            )
            .bind(name)
            .fetch_all(&pool)
            .await?;
            for id in connections {
                // Between the query and KILL the connection may have closed on its own (1094), which does not affect
                // the DROP that follows.
                let _ = sqlx::query(&format!("KILL {id}")).execute(&pool).await;
            }
            sqlx::query(&format!("DROP DATABASE IF EXISTS `{name}`"))
                .execute(&pool)
                .await?;
            pool.close().await;
            Cache::connect(redis).await?.clear_namespace().await?;
            if root.exists() {
                std::fs::remove_dir_all(root)?;
            }
            Ok(())
        })
}

/// Description of each step of a SQLite execution plan (`EXPLAIN QUERY PLAN`), e.g. `SEARCH t USING INDEX …`,
/// `USE TEMP B-TREE FOR ORDER BY`.
/// `sql` is the original text with parameters expanded from the debug record (already through the rewriter on SQLite).
pub async fn sqlite_plan(db: &Database, sql: &str) -> Result<Vec<String>> {
    let rows = crate::db::query(&format!("EXPLAIN QUERY PLAN {sql}"))
        .written_for(crate::db::Backend::Sqlite)
        .fetch_all(&db.pool)
        .await?;
    rows.iter()
        .map(|row| crate::db::text(row, "detail"))
        .collect()
}

/// The thread table (alias `t`) is read in index order: the SQLite plan builds no temporary B-tree sort for it (the
/// equivalent of MySQL's filesort), and has no full table scan that bypasses indexes.
pub fn assert_sqlite_index_order(plan: &[String], sql: &str) {
    assert!(
        !plan.iter().any(|step| step.contains("USE TEMP B-TREE")),
        "the thread table must be read in index order: {sql}\n{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step == "SCAN t"),
        "the thread table must not be scanned in full: {sql}\n{plan:?}"
    );
}

/// A backlog for the Outbox claim plan tests: for each of `types`, events already sent, plus due pending and sent
/// events of a type no worker claims; then fresh statistics, so both planners weigh the claim index against the
/// primary key and `gonof_outbox_delivery` as they would on a busy site.
pub async fn outbox_backlog(db: &Database, types: &[&str], now: i64) -> Result<()> {
    const ROWS: usize = 300;
    let mut rows = Vec::new();
    for (n, kind) in types.iter().enumerate() {
        for m in 0..ROWS {
            rows.push(format!(
                "('backlog:{n}:{m}','{kind}','test',{m},'{{}}',{at},{at},'sent')",
                at = now - 86_400
            ));
        }
    }
    for m in 0..2 * ROWS {
        let status = if m % 2 == 0 { "pending" } else { "sent" };
        rows.push(format!(
            "('backlog:other:{m}','test.backlog','test',{m},'{{}}',{at},{at},'{status}')",
            at = now - 3_600
        ));
    }
    for chunk in rows.chunks(500) {
        crate::db::query(&format!(
            "INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES {}",
            chunk.join(",")
        ))
        .execute(db.executor())
        .await?;
    }
    crate::db::raw_sql(match db.backend() {
        crate::db::Backend::Sqlite => "ANALYZE",
        crate::db::Backend::MySql => "ANALYZE TABLE gonof_common_outbox",
    })
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// The statements among `records` that look for an event to claim (they read the outbox with a lease condition;
/// the recheck of the chosen row by primary key aside) read `gonof_outbox_claim` in index order, a range on type,
/// status and due time, on both backends (docs/queries.md, "Outbox claims"). On MySQL no read of the outbox uses
/// another index or a filesort; on SQLite no search of the outbox uses another index or feeds a temporary B-tree.
/// Returns how many statements were checked.
pub async fn assert_outbox_claim_plans(
    db: &Database,
    records: &[crate::debug::trace::QueryRecord],
) -> Result<usize> {
    let claims: Vec<_> = records
        .iter()
        .filter(|r| {
            let sql: String = r.sql.split_whitespace().collect();
            r.source == crate::debug::trace::QuerySource::Application
                && sql.contains("FROMgonof_common_outbox")
                && sql.contains("lease_until<=?")
                && !sql.contains("WHEREeventid=?")
        })
        .collect();
    assert!(!claims.is_empty(), "no claim statement was recorded");
    for record in &claims {
        let sql = &record.expanded_sql;
        match db.backend() {
            crate::db::Backend::MySql => {
                let plan = crate::db::query(&format!("EXPLAIN {sql}"))
                    .fetch_all(db.executor())
                    .await?;
                let mut reads = 0;
                for row in &plan {
                    // SQLx only gets the column names of EXPLAIN at execution time, so they are looked up by name.
                    let text = |column: &str| -> Result<String> {
                        let ordinal = row
                            .columns()
                            .iter()
                            .position(|c| c.name() == column)
                            .context("EXPLAIN column")?;
                        Ok(row
                            .try_get::<Option<Vec<u8>>, _>(ordinal)?
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                            .unwrap_or_default())
                    };
                    if !matches!(text("table")?.as_str(), "o" | "gonof_common_outbox") {
                        continue;
                    }
                    reads += 1;
                    let (kind, key, key_len, extra) = (
                        text("type")?,
                        text("key")?,
                        text("key_len")?,
                        text("Extra")?,
                    );
                    assert_eq!(key, "gonof_outbox_claim", "{}", record.sql);
                    assert_eq!(kind, "range", "{}", record.sql);
                    // `event_type` 322 bytes, `status` 66 and `available_at` 8: the range covers the due time.
                    assert_eq!(key_len, "396", "{}", record.sql);
                    assert!(!extra.contains("filesort"), "{}\n{extra}", record.sql);
                }
                assert!(reads > 0, "{}", record.sql);
            }
            crate::db::Backend::Sqlite => {
                let steps: Vec<(i64, String)> =
                    crate::db::query(&format!("EXPLAIN QUERY PLAN {sql}"))
                        .written_for(crate::db::Backend::Sqlite)
                        .fetch_all(&db.pool)
                        .await?
                        .iter()
                        .map(|row| Ok((row.try_get("parent")?, crate::db::text(row, "detail")?)))
                        .collect::<Result<_>>()?;
                let outbox = |detail: &str| {
                    detail
                        .strip_prefix("SEARCH ")
                        .or_else(|| detail.strip_prefix("SCAN "))
                        .and_then(|rest| {
                            rest.strip_prefix("gonof_common_outbox")
                                .or_else(|| rest.strip_prefix('o'))
                        })
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
                };
                let reads: Vec<_> = steps.iter().filter(|(_, d)| outbox(d)).collect();
                assert!(!reads.is_empty(), "{}\n{steps:?}", record.sql);
                for (parent, detail) in reads {
                    assert!(
                        detail.contains(
                            "INDEX gonof_common_outbox__gonof_outbox_claim (event_type=? AND status=? AND available_at<?)"
                        ),
                        "{}\n{steps:?}",
                        record.sql
                    );
                    assert!(
                        !steps
                            .iter()
                            .any(|(p, d)| p == parent && d.starts_with("USE TEMP B-TREE")),
                        "the outbox rows are not sorted: {}\n{steps:?}",
                        record.sql
                    );
                }
            }
        }
    }
    Ok(claims.len())
}

/// Fault injection for tests: makes statements fail on `table` at `timing` (`BEFORE INSERT`, `BEFORE UPDATE`,
/// `BEFORE DELETE`) when `condition` holds, with `message` in the error. `condition` may refer to `NEW.column` and
/// `OLD.column`, written as an expression both backends accept; an empty string means always fail.
/// MySQL uses `SIGNAL SQLSTATE '45000'`, SQLite uses `RAISE(ABORT, …)`. Created only in test databases, removed with
/// [`drop_trigger`] afterwards; the project itself does not use triggers (docs/architecture.md, "Data"). Trigger DDL
/// cannot go through the prepared statement protocol, so it is executed as raw text.
pub async fn fail_when(
    db: &Database,
    name: &str,
    timing: &str,
    table: &str,
    condition: &str,
    message: &str,
) -> Result<()> {
    let sql = match (db.backend(), condition.is_empty()) {
        (crate::db::Backend::MySql, true) => format!(
            "CREATE TRIGGER {name} {timing} ON {table} FOR EACH ROW SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT='{message}'"
        ),
        (crate::db::Backend::MySql, false) => format!(
            "CREATE TRIGGER {name} {timing} ON {table} FOR EACH ROW BEGIN IF {condition} THEN SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT='{message}'; END IF; END"
        ),
        (crate::db::Backend::Sqlite, true) => format!(
            "CREATE TRIGGER {name} {timing} ON {table} FOR EACH ROW BEGIN SELECT RAISE(ABORT, '{message}'); END"
        ),
        (crate::db::Backend::Sqlite, false) => format!(
            "CREATE TRIGGER {name} {timing} ON {table} FOR EACH ROW WHEN {condition} BEGIN SELECT RAISE(ABORT, '{message}'); END"
        ),
    };
    ddl(db, &sql).await
}

/// Drops the trigger created by [`fail_when`].
pub async fn drop_trigger(db: &Database, name: &str) -> Result<()> {
    ddl(db, &format!("DROP TRIGGER {name}")).await
}

/// Same as [`drop_trigger`], without erroring when the trigger does not exist.
pub async fn drop_trigger_if_exists(db: &Database, name: &str) -> Result<()> {
    ddl(db, &format!("DROP TRIGGER IF EXISTS {name}")).await
}

/// Changes the database schema on one connection. Each SQLite connection caches the schema on its own: after another
/// connection has created or dropped triggers, this connection's cache may still be stale, and parsing DDL would then
/// falsely report "already exists" or "does not exist". So first read sqlite_master once on this connection, letting it
/// notice that the schema version changed and reload, then execute.
async fn ddl(db: &Database, sql: &str) -> Result<()> {
    let mut connection = db.pool.acquire().await?;
    if db.backend() == crate::db::Backend::Sqlite {
        crate::db::query("SELECT COUNT(*) FROM sqlite_master")
            .fetch_one(&mut *connection)
            .await?;
    }
    crate::db::raw_sql(sql).execute(&mut *connection).await?;
    Ok(())
}

/// Database names and Redis namespaces carry the process ID: `gonof_test_<pid>_<nanoseconds>_<sequence>`. When several
/// sessions share one instance, only this run's leftovers are counted and cleaned up, by process ID.
fn unique_name() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "gonof_test_{}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// When a test needs to compare a whole set of replies, read page by page explicitly;
/// production paths do not use this helper.
pub async fn thread_with_all_pages(
    db: &Database,
    id: u64,
) -> Result<Option<crate::content::Thread>> {
    let Some(mut thread) = crate::content::thread(db, id).await? else {
        return Ok(None);
    };
    let paging = crate::urls::Paging::from_settings(&crate::runtime::read(db).await?);
    let mut replies = Vec::new();
    for page in 1..=thread.reply_count.div_ceil(paging.replies).max(1) {
        crate::content::load_replies(db, &mut thread, page, true, None, 1, paging).await?;
        replies.append(&mut thread.posts);
    }
    thread.posts = replies;
    Ok(Some(thread))
}

/// Creates the site's first administrator `admin` the way the setup wizard does and returns its random password.
pub async fn create_admin(identity: &crate::identity::Identity) -> String {
    let password = format!("Admin-{}", crate::identity::random_token());
    identity
        .setup_admin("admin", None, &password)
        .await
        .expect("creating the test administrator");
    password
}

/// For tests: an unsubscribed process cache handle with Redis disabled, for cases that construct services
/// (`Identity`, `Mailer`) on their own. Cases that share settings with the router application must pass the
/// application's copy ([`TestFixture::global`]), otherwise after the application saves settings this side sees them
/// only after up to one lifetime.
pub fn detached_global() -> crate::cache::GlobalCache {
    crate::cache::GlobalCache::new(
        Cache::new(&RedisConfig {
            enabled: false,
            ..RedisConfig::default()
        })
        .expect("a cache handle with Redis off cannot fail"),
    )
}

/// The same site time zone as the migration seed (`settings.time.offset_minutes` 480, UTC+8), for tests that call
/// settlement and statistics directly; pages and the write gate through the router take it from the settings.
pub const SEEDED_OFFSET_MINUTES: i64 = 480;

/// The same sidebar parameters as the migration seed (`settings.display.hot_days` 7, `hot_count` 10,
/// `latest_count` 10, site time zone UTC+8), for tests that call content services directly; pages through the router
/// take them from the page frame settings.
pub const SEEDED_SIDEBAR: crate::content::SidebarParams = crate::content::SidebarParams {
    hot_days: 7,
    hot_count: 10,
    latest_count: 10,
    offset_minutes: SEEDED_OFFSET_MINUTES,
};

/// The same floor pagination as the migration seed (50 floors per page, 20 nested replies per page), for tests that
/// call content services directly.
pub const SEEDED_PAGING: crate::urls::Paging = crate::urls::Paging {
    replies: 50,
    sub_replies: 20,
};

/// The same highlighted reply rule as the migration seed (at least 1 like, at most 3 replies).
pub const SEEDED_HIGHLIGHTS: crate::interaction::HighlightRules =
    crate::interaction::HighlightRules {
        min_likes: 1,
        count: 3,
    };

/// The forum URL the test fixture writes into `settings.site.url`.
pub const TEST_SITE_URL: &str = "http://127.0.0.1:58833";

/// Writes the forum URL by changing the database directly (for tests). The write gate and admin edit page read the
/// database and see it immediately; pages shown through the process cache see it only after up to one lifetime, so
/// tests must call the corresponding application's `GlobalCache::clear_local` after changing it.
pub async fn set_site_url(database: &Database, url: &str) -> Result<()> {
    crate::db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey='settings.site.url'")
        .bind(url)
        .execute(database.executor())
        .await?;
    Ok(())
}

/// Changes one setting by changing the database directly (for tests). The write gate and admin edit page read the
/// database and see it immediately; pages shown through the process cache wait for the corresponding application's
/// `GlobalCache::clear_local`. Errors when the key does not exist; no new row is created.
pub async fn set_setting(database: &Database, key: &str, value: &str) -> Result<()> {
    let updated = crate::db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
        .bind(value)
        .bind(key)
        .execute(database.executor())
        .await?
        .rows_affected();
    anyhow::ensure!(
        updated == 1 || crate::runtime::read(database).await.is_ok(),
        "no such setting: {key}"
    );
    Ok(())
}

/// The test site (docs/translating.md, "For developers: adding interface text"): English with dashed dates, a site
/// name, description, read-only notice and level names, brand words that differ from the English defaults, and two
/// honor groups. Cases about the defaults or about another language change these back themselves.
pub async fn restore_sample_site(database: &Database) -> Result<()> {
    for (key, value) in [
        ("settings.site.language", "en"),
        ("settings.time.date_format", "ymd-dash"),
        ("settings.site.name", "Sample Community"),
        (
            "settings.site.description",
            "A sample community built with Gono Forum, for running tests.",
        ),
        (
            "settings.site.read_only_message",
            "The forum is in read-only mode for a while. Please come back soon.",
        ),
        (
            "settings.growth.level_names",
            "One-Star Member\nTwo-Star Member\nThree-Star Member\nFour-Star Member\nFive-Star Member\nSix-Star Member\nSeven-Star Member\nEight-Star Member",
        ),
        ("settings.site.brand_text", "Sample Forum"),
        ("settings.site.credit_name", "Coins"),
        ("settings.site.index_name", "Popularity Index"),
    ] {
        set_setting(database, key, value).await?;
    }
    crate::db::query("INSERT INTO gonof_common_honor(id,name,description,displayorder,status,version) VALUES(1,'Builder','For outstanding work building the community',1,1,1),(2,'Sponsor','Sponsors the running of the community',2,1,1)")
        .execute(database.executor())
        .await?;
    Ok(())
}

/// Test fixture: raises the posting quota to 1000. Changes the database directly; the write gate reads the database
/// every time, so it takes effect immediately.
pub async fn raise_posting_quotas(database: &Database) -> Result<()> {
    crate::db::query(
        "UPDATE gonof_common_settings SET svalue='1000' WHERE skey IN ('settings.posting.first_day_threads','settings.posting.first_day_replies','settings.posting.daily_threads','settings.posting.daily_replies')",
    )
    .execute(database.executor())
    .await?;
    Ok(())
}

/// Delivers a batch with the default mail configuration. The default configuration cannot decrypt the SMTP
/// authorization code and can only deliver locally, so it is for tests only; production paths use `Mailer::new` with
/// the real configuration and the application's global cache handle.
///
/// Mail links use the admin-area forum URL: before delivering, it is first written as `base_url` (on a real site an
/// administrator sets it), then delivered with a freshly created handle (no cached old value).
pub async fn deliver_mail(
    database: &Database,
    directory: &Path,
    base_url: &str,
    now: i64,
) -> Result<crate::mail::DeliveryReport> {
    set_site_url(database, base_url).await?;
    crate::mail::Mailer::new(
        database.clone(),
        crate::secrets::CommonConfig::default(),
        directory.to_owned(),
        detached_global(),
    )
    .deliver_pending(now)
    .await
}

/// Writes an unread notification through the official entry point (also incrementing the recipient's unread count by
/// one), to build data for integration tests; returns the notification ID.
///
/// Unread counts are maintained only by code: writing notifications with raw SQL would make the count and the details
/// disagree, and marking them read later would be rejected by the conditional update. Like the business path, the
/// recipient member row is locked first.
pub async fn insert_notification(
    db: &Database,
    notice: &crate::notifications::record::NewNotice<'_>,
) -> Result<i64> {
    let mut tx = db.begin_write().await?;
    let id = insert_notification_in(db, &mut tx, notice).await?;
    tx.commit().await?;
    Ok(id)
}

/// Same as [`insert_notification`], written in the caller's transaction, to make rollback easy to verify.
pub async fn insert_notification_in(
    db: &Database,
    tx: &mut Tx<'_>,
    notice: &crate::notifications::record::NewNotice<'_>,
) -> Result<i64> {
    crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(notice.uid)
        .fetch_one(db.observe(&mut **tx))
        .await?;
    crate::notifications::record::insert(db, tx, notice).await
}

/// After a test changes a forum with raw SQL (visibility, enabled state, name, slug, parent forum), rewrites the
/// top-level forum ID of the threads of that forum and its child forums through the official copy code and bumps the
/// site-wide version (`family_fid` is the only forum copy on the thread row). Forum visibility and enabled state are
/// judged by the process forum cache: cases going through HTTP must also clear the fixture's
/// `GlobalCache::clear_local`.
pub async fn sync_forum_copies(db: &Database, fid: i64) -> Result<()> {
    let mut tx = db.begin_content_write().await?;
    crate::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
        .bind(fid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    let children: Vec<i64> = crate::db::query_scalar(
        "SELECT fid FROM gonof_forum_forum WHERE parent_fid=? ORDER BY fid",
    )
    .bind(fid)
    .fetch_all(db.observe(&mut *tx))
    .await?;
    for forum in std::iter::once(fid).chain(children) {
        crate::content::copies::resync_forum(db, &mut tx, forum).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// After a test changes a thread's posts or thread row with raw SQL (hide, withdraw, change body, change status),
/// rewrites the sign of `displayorder` by the first post through the official copy code and bumps the thread version
/// and site-wide version, with the same effect as going through the moderation and edit entry points:
/// floor, list and feed caches are invalidated accordingly. Counts are not recomputed here; use [`refresh_listing`]
/// when counts are needed too.
pub async fn resync_thread(db: &Database, tid: i64) -> Result<()> {
    let mut tx = db.begin_content_write().await?;
    crate::db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
        .bind(tid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    crate::content::copies::sync_first_hidden(db, &mut tx, tid).await?;
    crate::content::copies::bump_site(db, &mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// After a test changes threads or posts in bulk with raw SQL (hide, visibility, publish status, read permission,
/// building data), recomputes the unlisted `displayorder`, the nested replies' `root_invisible`, forum thread counts
/// and member counts by the listing rules (docs/queries.md, "Whether a thread is listed") and bumps the site-wide
/// version (`content::copies::rebuild_listing`, also used by the development fixture import); then recomputes the
/// like, downvote, favorite and tip count columns from the details (`interaction::counters::rebuild`).
pub async fn refresh_listing(db: &Database) -> Result<()> {
    let mut tx = db.begin_content_write().await?;
    crate::content::copies::rebuild_listing(db, &mut tx).await?;
    crate::interaction::counters::rebuild(db, &mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// After a test writes a thread's replies with raw SQL, recomputes its live reply count and last-reply copies
/// (`last_reply_pid`, `last_reply_author`, `last_reply_uid`, `last_reply_summary`, `lastpost`) from scratch
/// (`content::recount_effective_replies`), and bumps the site-wide version.
pub async fn refresh_replies(db: &Database, tid: i64) -> Result<()> {
    let mut tx = db.begin_content_write().await?;
    crate::db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
        .bind(tid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    crate::content::recount_effective_replies(db, &mut tx, tid).await?;
    crate::content::copies::bump_site(db, &mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// After a test changes a member's username or avatar with raw SQL, rewrites the author copies and last replier through
/// the official copy code and bumps the site-wide version, with the same effect as renaming or changing the avatar.
pub async fn sync_member_copies(db: &Database, uid: i64) -> Result<()> {
    let mut tx = db.begin_write().await?;
    crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(uid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    crate::content::copies::member_changed(db, &mut tx, uid).await?;
    tx.commit().await?;
    Ok(())
}

/// Runs the content consistency check `apps/web/tests/consistency/content.sql` (without the last row, the trigger
/// count) and returns each item's name and amount. Can run inside an uncommitted transaction, to prove that a certain
/// inconsistency is caught by the check.
pub async fn content_drift<'c>(
    executor: impl crate::db::Executor<'c>,
) -> Result<Vec<(String, i64)>> {
    check_script(executor, "content.sql").await
}

/// Runs the identity consistency check `apps/web/tests/consistency/identity.sql` (without the last row, the trigger
/// count); usage as for [`content_drift`].
pub async fn identity_drift<'c>(
    executor: impl crate::db::Executor<'c>,
) -> Result<Vec<(String, i64)>> {
    check_script(executor, "identity.sql").await
}

/// After raw SQL corrupts identity and moderation tables: the database still rejects it (foreign key, unique key,
/// `NOT NULL`, column width), or within the same uncommitted transaction the "`item`" of the identity consistency
/// check `apps/web/tests/consistency/identity.sql` catches it. Used by constraint tests for rules that code rather
/// than the database enforces; the transaction is rolled back at the end, leaving no bad data.
pub async fn assert_identity_rejected_or_flagged(db: &Database, statement: &str, item: &str) {
    assert_rejected_or_flagged(db, "identity.sql", statement, item).await;
}

/// Every item of the identity consistency check is 0 (existing data in the identity and moderation tables satisfies
/// the value rules that code enforces).
pub async fn assert_identity_consistent(db: &Database) {
    assert_consistent(db, "identity.sql").await;
}

/// The site has `boards` boards, each with a color and a glyph stored as the board form stores them
/// (`organization::board_color`, `organization::board_glyph`), and no two share a color or a glyph: what the setup
/// wizard's sample, `seed-demo` and the committed example datasets bring (docs/skins.md, "Board colors and glyphs").
pub async fn assert_boards_have_distinct_colors_and_glyphs(db: &Database, boards: usize) {
    let rows = crate::db::query("SELECT slug, color, glyph FROM gonof_forum_forum ORDER BY fid")
        .fetch_all(db.executor())
        .await
        .unwrap();
    assert_eq!(rows.len(), boards, "boards");
    let mut colors = std::collections::BTreeSet::new();
    let mut glyphs = std::collections::BTreeSet::new();
    for row in &rows {
        let slug = crate::db::text(row, "slug").unwrap();
        let color = crate::db::text(row, "color").unwrap();
        let glyph = crate::db::text(row, "glyph").unwrap();
        assert!(
            !color.is_empty() && crate::organization::board_color(&color).as_ref() == Some(&color),
            "board {slug} has the color {color:?}"
        );
        assert!(
            !glyph.is_empty() && crate::organization::board_glyph(&glyph).as_ref() == Some(&glyph),
            "board {slug} has the glyph {glyph:?}"
        );
        assert!(colors.insert(color), "board {slug} repeats a color");
        assert!(glyphs.insert(glyph), "board {slug} repeats a glyph");
    }
}

/// Same as [`assert_identity_rejected_or_flagged`], using the messaging consistency check
/// `apps/web/tests/consistency/messaging.sql` (private messages, follows, blocks, favorites, likes, requests,
/// revisions, attachments).
pub async fn assert_messaging_rejected_or_flagged(db: &Database, statement: &str, item: &str) {
    assert_rejected_or_flagged(db, "messaging.sql", statement, item).await;
}

/// Every item of the messaging consistency check is 0.
pub async fn assert_messaging_consistent(db: &Database) {
    assert_consistent(db, "messaging.sql").await;
}

async fn assert_rejected_or_flagged(db: &Database, script: &str, statement: &str, item: &str) {
    let mut tx = db.pool.begin().await.unwrap();
    if crate::db::query(statement).execute(&mut *tx).await.is_ok() && !sqlite_tests() {
        let drift = check_script(&mut tx, script).await.unwrap();
        assert!(
            drift
                .iter()
                .any(|(name, amount)| name == item && *amount > 0),
            "{statement} should be caught by \"{item}\" in {script}: {drift:?}"
        );
    }
    tx.rollback().await.unwrap();
}

async fn assert_consistent(db: &Database, script: &str) {
    let mut conn = db.pool.acquire().await.unwrap();
    let drift = check_script(&mut conn, script).await.unwrap();
    let bad: Vec<_> = drift.iter().filter(|(_, amount)| *amount != 0).collect();
    assert!(
        bad.is_empty(),
        "{script} found rows that break the rules: {bad:?}"
    );
}

/// Runs one of the consistency checks in `apps/web/tests/consistency/`. They are written in MySQL syntax
/// (REGEXP, SUBSTRING_INDEX, information_schema…) and run only on MySQL: on SQLite they return empty, so "every item
/// is 0" checks pass as usual; cases dedicated to testing that the checks catch deviations are marked as MySQL-only.
/// The trigger total row at the end of each file is filtered out.
async fn check_script<'c>(
    executor: impl crate::db::Executor<'c>,
    file: &str,
) -> Result<Vec<(String, i64)>> {
    if sqlite_tests() {
        return Ok(Vec::new());
    }
    let sql =
        std::fs::read_to_string(project_root().join("apps/web/tests/consistency").join(file))?;
    let body = sql.trim_end().trim_end_matches(';');
    let rows = crate::db::query(&format!(
        "SELECT item, amount FROM ({body}\n) checks WHERE item NOT LIKE 'trigger count%'"
    ))
    .fetch_all(executor)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                crate::db::text(row, "item")?,
                row.try_get::<i64, _>("amount")?,
            ))
        })
        .collect()
}

/// After a test changes forums, threads or posts in bulk with raw SQL, recomputes the copies of every forum and every
/// thread through the official copy code and invalidates caches, with the same effect as saving each forum in the admin
/// area and going through the moderation entry points one by one. Only for tests that build data; cases verifying the
/// synchronization itself go through the business entry points.
pub async fn resync_copies(db: &Database) -> Result<()> {
    let forums: Vec<i64> =
        crate::db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid")
            .fetch_all(db.executor())
            .await?;
    for fid in forums {
        let mut tx = db.begin_content_write().await?;
        crate::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
            .bind(fid)
            .fetch_one(db.observe(&mut *tx))
            .await?;
        crate::content::copies::resync_forum(db, &mut tx, fid).await?;
        tx.commit().await?;
    }
    let threads: Vec<i64> =
        crate::db::query_scalar("SELECT tid FROM gonof_forum_thread ORDER BY tid")
            .fetch_all(db.executor())
            .await?;
    for tid in threads {
        resync_thread(db, tid).await?;
    }
    Ok(())
}
