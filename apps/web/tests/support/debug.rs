//! Debug behavior runs against each isolated MySQL fixture.
use std::{
    path::PathBuf,
    sync::{Arc, Once},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
    middleware,
    response::{Html, IntoResponse},
    routing::get,
};
use gono_forum_web::{
    content,
    db::{Database, sql_display::SqlValue},
    debug::{
        self,
        driver::{DriverFilter, DriverQueryLayer},
        trace::{self, QueryRecord, QuerySource, RequestProfile},
    },
    identity::Identity,
    project_root, router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

use crate::support;

pub async fn assert_debug_contract(db: &Database) {
    static DEBUG_TRACING: Once = Once::new();
    DEBUG_TRACING.call_once(|| {
        tracing_subscriber::registry()
            .with(DriverQueryLayer.with_filter(DriverFilter))
            .init();
    });
    // The original text with expanded parameters writes literals for the backend that executed it; index diagnostics
    // run EXPLAIN QUERY PLAN on SQLite.
    assert_bound_values_roundtrip(db).await;
    assert_failure_order_and_counts(db).await;
    assert_concurrent_profiles(db).await;
    assert_index_diagnostics(db).await;
    let account = debug_account(db).await;
    assert_html_responses(db, &account.cookie).await;
    assert_footer_metrics(db, &account.cookie).await;
    assert_sql_carriage_returns_in_html(db).await;
    assert_non_html_responses(db, &account.cookie).await;
    // Must come before assert_debug_permissions: that bumps credentials_version and voids this session ticket.
    assert_editor_reuses_forum_choices(db, &account.cookie).await;
    assert_sensitive_pages_skip_the_panel(db, &account.cookie).await;
    assert_debug_permissions(db, &account.cookie, account.uid).await;
    account.cleanup(db).await;
}

/// The forum dropdown on the posting page and the URL resolution of `/new/{slug}` both come from the process forum
/// cache in the page frame (docs/settings-and-caching.md, "The process cache"), so on a cache hit the whole page does
/// not read the forum table.
async fn assert_editor_reuses_forum_choices(db: &Database, cookie: &str) {
    const FORUM_TABLE: &str = "FROM gonof_forum_forum";
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    // The first signed-in request loads the member slot.
    let (status, _, warm) = authorized_fetch(&app, "/", false, cookie).await;
    assert_eq!(status, StatusCode::OK);
    // That SQL carries its own subquery for the parent forum, and the panel has another copy with the bound values
    // expanded, so it appears more than once.
    assert!(
        panel(&warm).contains(FORUM_TABLE),
        "the first request loads the forum cache"
    );
    let slug: String =
        gono_forum_web::db::query_scalar("SELECT slug FROM gonof_forum_forum ORDER BY fid")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    for path in ["/new", &format!("/new/{slug}")] {
        let (status, _, body) = authorized_fetch(&app, path, false, cookie).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{path} must serve the editor directly"
        );
        assert_eq!(
            panel(&body).matches(FORUM_TABLE).count(),
            0,
            "{path} must not read the forum table on a cache hit"
        );
        assert!(
            panel(&body)
                .contains("data-debug-block=\"forums-members\" data-debug-block-hit=\"true\"")
        );
    }
}

/// Sensitive pages (API tokens, data requests) do not get the panel injected even when an administrator has debugging
/// on: the SQL bound values contain token digests and account data. The list is the same one used to hide the debug
/// entry on pages (previously `/my/api` was missed).
async fn assert_sensitive_pages_skip_the_panel(db: &Database, cookie: &str) {
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    let (status, _, body) = authorized_fetch(&app, "/my/profile", false, cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("id=\"gonof-debug\""),
        "an ordinary settings page gets the panel"
    );
    for path in ["/my/api", "/my/data"] {
        for htmx in [false, true] {
            let (status, _, body) = authorized_fetch(&app, path, htmx, cookie).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_no_debug(&body);
        }
    }
}

struct DebugAccount {
    uid: i64,
    cookie: String,
    root: PathBuf,
}

impl DebugAccount {
    async fn cleanup(self, db: &Database) {
        for sql in [
            "DELETE FROM gonof_common_session WHERE uid = ?",
            "DELETE FROM gonof_common_member_presence WHERE uid = ?",
            "DELETE FROM gonof_common_log WHERE uid = ?",
            "DELETE FROM gonof_common_member WHERE uid = ?",
        ] {
            gono_forum_web::db::query(sql)
                .bind(self.uid)
                .execute(&db.pool)
                .await
                .unwrap();
        }
        gono_forum_web::db::query("UPDATE gonof_common_settings SET svalue = 'pending' WHERE skey = 'identity.bootstrap_admin'")
            .execute(&db.pool).await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

async fn debug_account(db: &Database) -> DebugAccount {
    let root = std::env::temp_dir().join(format!(
        "gonof_debug_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // The account's local directory; cleanup removes it again.
    std::fs::create_dir_all(&root).unwrap();
    let identity = Identity::new(
        db.clone(),
        support::config_for(db).identity,
        root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let password = gono_forum_web::test_support::create_admin(&identity).await;
    let grant = identity.login("admin", &password).await.unwrap();
    DebugAccount {
        uid: grant.member.uid,
        cookie: format!("gonof_session={}", grant.session_token),
        root,
    }
}

async fn authorized_fetch(
    app: &Router,
    path: &str,
    htmx: bool,
    cookie: &str,
) -> (StatusCode, HeaderMap, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::COOKIE, cookie)
                .header("HX-Request", if htmx { "true" } else { "false" })
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, headers, body)
}

fn assert_no_debug(body: &str) {
    for output in [
        "gonof-debug",
        "/static/css/debug.css",
        "/static/js/debug.js",
        "FROM gonof_forum_forum",
        "data-debug-index",
        "Indexes (EXPLAIN plan)",
    ] {
        assert!(
            !body.contains(output),
            "an unauthorized response must not contain debug output: {output}"
        );
    }
}

async fn assert_debug_permissions(db: &Database, cookie: &str, uid: i64) {
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    for sql in [
        "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = ?",
        "UPDATE gonof_common_member SET `freeze` = 0, adminid = 0, email = 'debug-member@example.test', emailstatus = 1 WHERE uid = ?",
        "UPDATE gonof_common_member SET adminid = 1, email = NULL, emailstatus = 0, credentials_version = credentials_version + 1 WHERE uid = ?",
    ] {
        gono_forum_web::db::query(sql)
            .bind(uid)
            .execute(&db.pool)
            .await
            .unwrap();
        for htmx in [false, true] {
            let (status, _, body) = authorized_fetch(&app, "/", htmx, cookie).await;
            assert_eq!(status, StatusCode::OK);
            assert_no_debug(&body);
        }
    }
}

fn completed(profile: &RequestProfile, expected: usize) -> Vec<QueryRecord> {
    let records = profile.snapshot();
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.sequence, index + 1);
        assert!(!record.cancelled);
        assert!(record.elapsed > std::time::Duration::ZERO);
        assert!(record.expansion_error.is_none(), "{record:?}");
    }
    let records: Vec<_> = records
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .collect();
    assert_eq!(
        records.len(),
        expected,
        "application sources and placeholder SQL: {:?}",
        records
            .iter()
            .map(|record| (record.source, &record.sql))
            .collect::<Vec<_>>()
    );
    records
}

async fn assert_bound_values_roundtrip(db: &Database) {
    let sql = "SELECT ? AS value";
    // "\u{91d1}\u{5e01}" is the Chinese for "gold coins": three-byte characters next to a four-byte emoji, which the
    // expanded literal must keep as they are.
    let mut text_values = vec![
        "\u{91d1}\u{5e01}🍓 text 'double quote\" ? $1 $10 -- /* */ </code><script>".to_owned(),
        "path\\quoted'\nnext line\r\t\x08\x0c\x1a\x7f".to_owned(),
        format!(
            "{}the end is not cut off",
            "\u{91d1}\u{5e01}🍓 long query".repeat(4000)
        ),
    ];
    // MySQL text columns must store NUL as is.
    {
        text_values.push("contains a \0 byte".into());
    }
    for value in text_values {
        let profile = Arc::new(RequestProfile::new());
        let row = trace::scope(Arc::clone(&profile), async {
            gono_forum_web::db::query(sql)
                .bind(&value)
                .fetch_one(db.executor())
                .await
                .unwrap()
        })
        .await;
        assert_eq!(gono_forum_web::db::text(&row, "value").unwrap(), value);
        let records = completed(&profile, 1);
        assert_eq!(records[0].sql, sql);
        assert_eq!(records[0].parameters, [SqlValue::Text(value.clone())]);
        assert_eq!(records[0].rows_returned, 1);
        let replay = gono_forum_web::db::query(&records[0].expanded_sql)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            gono_forum_web::db::text(&replay, "value").unwrap(),
            value,
            "{records:?}"
        );
    }
    for value in [i64::MIN, -42, 0, i64::MAX] {
        let sql = "SELECT ? AS value /* integer roundtrip */";
        let profile = Arc::new(RequestProfile::new());
        let row = trace::scope(Arc::clone(&profile), async {
            gono_forum_web::db::query(sql)
                .bind(value)
                .fetch_one(db.executor())
                .await
                .unwrap()
        })
        .await;
        assert_eq!(row.try_get::<i64, _>("value").unwrap(), value);
        let records = completed(&profile, 1);
        assert_eq!(records[0].parameters, [SqlValue::Integer(value)]);
        let replay = gono_forum_web::db::query(&records[0].expanded_sql)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(replay.try_get::<i64, _>("value").unwrap(), value);
    }
    for value in [-2.75_f64, 0.0, 1.5, 1.0e-30] {
        let sql = "SELECT ? AS value /* float roundtrip */";
        let profile = Arc::new(RequestProfile::new());
        trace::scope(Arc::clone(&profile), async {
            let row = gono_forum_web::db::query(sql)
                .bind(value)
                .fetch_one(db.executor())
                .await
                .unwrap();
            assert_eq!(row.try_get::<f64, _>("value").unwrap(), value);
        })
        .await;
        let records = completed(&profile, 1);
        assert_eq!(records[0].parameters, [SqlValue::Float(value)]);
        let replay = gono_forum_web::db::query(&records[0].expanded_sql)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(replay.try_get::<f64, _>("value").unwrap(), value);
    }
    for value in [Vec::<u8>::new(), vec![0, 1, b'\'', b'\\', 128, 255]] {
        // Binary parameters are read back by bytes, verifying the raw bytes are transmitted intact (HEX gives different
        // results for numbers on the two backends, and the rewriter does not accept it).
        let sql = "SELECT ? AS value /* bytes roundtrip */";
        let profile = Arc::new(RequestProfile::new());
        trace::scope(Arc::clone(&profile), async {
            let row = gono_forum_web::db::query(sql)
                .bind(value.clone())
                .fetch_one(db.executor())
                .await
                .unwrap();
            assert_eq!(row.try_get::<Vec<u8>, _>("value").unwrap(), value);
        })
        .await;
        let records = completed(&profile, 1);
        assert_eq!(records[0].parameters, [SqlValue::Bytes(value.clone())]);
        let replay = gono_forum_web::db::query(&records[0].expanded_sql)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(replay.try_get::<Vec<u8>, _>("value").unwrap(), value);
    }
    for value in [false, true] {
        let sql = "SELECT CASE WHEN ? THEN 1 ELSE 0 END AS value";
        let profile = Arc::new(RequestProfile::new());
        trace::scope(Arc::clone(&profile), async {
            let row = gono_forum_web::db::query(sql)
                .bind(value)
                .fetch_one(db.executor())
                .await
                .unwrap();
            assert_eq!(row.try_get::<i64, _>("value").unwrap(), i64::from(value));
        })
        .await;
        let records = completed(&profile, 1);
        assert_eq!(records[0].parameters, [SqlValue::Bool(value)]);
        let replay = gono_forum_web::db::query(&records[0].expanded_sql)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(replay.try_get::<i64, _>("value").unwrap(), i64::from(value));
    }
    let sql = "SELECT ? AS value /* nullable roundtrip */";
    let profile = Arc::new(RequestProfile::new());
    trace::scope(Arc::clone(&profile), async {
        let row = gono_forum_web::db::query(sql)
            .bind(Option::<i64>::None)
            .fetch_one(db.executor())
            .await
            .unwrap();
        assert!(row.is_null("value").unwrap());
    })
    .await;
    let records = completed(&profile, 1);
    assert_eq!(records[0].parameters, [SqlValue::Null]);
    let replay = gono_forum_web::db::query(&records[0].expanded_sql)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(replay.is_null("value").unwrap());

    // Adjacent subtraction must never turn a negative parameter into a -- comment.
    let profile = Arc::new(RequestProfile::new());
    let minus = "SELECT 1-? AS value";
    trace::scope(Arc::clone(&profile), async {
        gono_forum_web::db::query(minus)
            .bind(-1_i64)
            .fetch_one(db.executor())
            .await
            .unwrap();
    })
    .await;
    let records = completed(&profile, 1);
    let replay = gono_forum_web::db::query(&records[0].expanded_sql)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(replay.try_get::<i64, _>("value").unwrap(), 2);

    // The ten bound parameters are expanded in order, and literals inside quotes and comments stay as they are.
    let integer = { "SIGNED" };
    let ten = &format!(
        "SELECT CAST(?+?+?+?+?+?+?+?+?+? AS {integer}) AS value, '? $1 $10' AS quoted /* ? $1 */"
    );
    let profile = Arc::new(RequestProfile::new());
    trace::scope(Arc::clone(&profile), async {
        let mut query = gono_forum_web::db::query(ten);
        for value in 1_i64..=10 {
            query = query.bind(value);
        }
        let row = query.fetch_one(db.executor()).await.unwrap();
        assert_eq!(row.try_get::<i64, _>("value").unwrap(), 55);
    })
    .await;
    let records = completed(&profile, 1);
    assert_eq!(records[0].parameters.len(), 10);
    let replay = gono_forum_web::db::query(&records[0].expanded_sql)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(replay.try_get::<i64, _>("value").unwrap(), 55);
    assert_eq!(
        gono_forum_web::db::text(&replay, "quoted").unwrap(),
        "? $1 $10"
    );
}

async fn assert_failure_order_and_counts(db: &Database) {
    let profile = Arc::new(RequestProfile::new());
    trace::scope(Arc::clone(&profile), async {
        gono_forum_web::db::query("SELECT 1 UNION ALL SELECT 2")
            .fetch_all(db.executor())
            .await
            .unwrap();
        assert!(
            gono_forum_web::db::query("SELECT * FROM gonof_debug_nonexistent")
                .fetch_all(db.executor())
                .await
                .is_err()
        );
        gono_forum_web::db::query(
            "UPDATE gonof_forum_thread SET subject = subject WHERE tid = 101",
        )
        .execute(db.executor())
        .await
        .unwrap();
        gono_forum_web::db::query("SELECT tid FROM gonof_forum_thread WHERE tid = -1")
            .fetch_optional(db.executor())
            .await
            .unwrap();
    })
    .await;
    let records = completed(&profile, 4);
    assert_eq!(records[0].rows_returned, 2);
    assert!(records[0].error.is_none());
    assert!(records[1].error.is_some());
    assert_eq!(records[1].rows_returned, 0);
    assert!(records[2].rows_affected.is_some());
    assert_eq!(records[3].rows_returned, 0);
    assert!(records[3].error.is_none());
    assert!(trace::current().is_none());
}

async fn assert_concurrent_profiles(db: &Database) {
    let first = Arc::new(RequestProfile::new());
    let second = Arc::new(RequestProfile::new());
    let sql = "SELECT ? AS value";
    let run = |profile, value| {
        let sql = &sql;
        trace::scope(profile, async move {
            for _ in 0..3 {
                gono_forum_web::db::query(sql)
                    .bind(value)
                    .fetch_one(db.executor())
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    tokio::join!(
        run(Arc::clone(&first), "profile_first"),
        run(Arc::clone(&second), "profile_second")
    );
    for (profile, own, other) in [
        (first, "profile_first", "profile_second"),
        (second, "profile_second", "profile_first"),
    ] {
        let records = profile.snapshot();
        let queries: Vec<_> = records.iter().filter(|record| record.sql == sql).collect();
        assert_eq!(queries.len(), 3);
        for query in queries {
            assert_eq!(query.parameters, [SqlValue::Text(own.into())]);
            assert!(query.expanded_sql.contains(own));
            assert!(!query.expanded_sql.contains(other));
        }
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.sequence, index + 1);
            assert!(record.error.is_none());
        }
    }
}

async fn assert_index_diagnostics(db: &Database) {
    let original_views: i64 =
        gono_forum_web::db::query_scalar("SELECT views FROM gonof_forum_thread WHERE tid = 101")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let hostile = "'; UPDATE gonof_forum_thread SET views = views + 1000 WHERE tid = 101; -- </code><script>alert('index')</script> ? $1 /*";
    let indexed = "SELECT message FROM gonof_forum_post WHERE pid = ?";
    let scan = "SELECT message FROM gonof_forum_post WHERE message = ?";
    let profile = Arc::new(RequestProfile::new());
    let diagnostics = trace::scope(Arc::clone(&profile), async {
        let row = gono_forum_web::db::query(indexed)
            .bind(103002_i64)
            .fetch_optional(db.executor())
            .await
            .unwrap();
        assert!(row.is_some());
        gono_forum_web::db::query(scan)
            .bind("index-contract-absent")
            .fetch_optional(db.executor())
            .await
            .unwrap();
        gono_forum_web::db::query("SELECT 7 AS constant_value")
            .fetch_one(db.executor())
            .await
            .unwrap();
        gono_forum_web::db::query(scan)
            .bind(hostile)
            .fetch_optional(db.executor())
            .await
            .unwrap();
        gono_forum_web::db::query(
            "UPDATE gonof_forum_thread SET views = views + 1 WHERE tid = 101",
        )
        .execute(db.executor())
        .await
        .unwrap();
        assert!(
            gono_forum_web::db::query("SELECT * FROM gonof_index_diagnostic_missing_table")
                .fetch_all(db.executor())
                .await
                .is_err()
        );
        let records = completed(&profile, 6);
        let original_record_count = profile.snapshot().len();
        let diagnostics = debug::explain::collect(db, &records).await;
        assert_eq!(
            profile.snapshot().len(),
            original_record_count,
            "EXPLAIN diagnostics must not count as original SQL or be collected recursively"
        );
        assert_eq!(
            diagnostics.len(),
            records.len(),
            "diagnostics match the original SQL one to one, in order"
        );
        diagnostics
    })
    .await;
    assert_eq!(
        diagnostics[0].status, "indexed",
        "a primary key query gets an index diagnostic: {}",
        diagnostics[0].summary
    );
    assert!(!diagnostics[0].summary.is_empty());
    assert_eq!(
        diagnostics[1].status, "scan",
        "a body filter without an index is recognized as a table scan: {}",
        diagnostics[1].summary
    );
    assert_eq!(
        diagnostics[2].status, "none",
        "a constant query reads no table and must not be reported as a scan: {}",
        diagnostics[2].summary
    );
    assert_eq!(
        diagnostics[3].status, "scan",
        "quotes, semicolons and comments in parameters stay text: {}",
        diagnostics[3].summary
    );
    assert!(
        !diagnostics[3].summary.contains("<script>"),
        "the index summary describes the plan without the bound parameter text"
    );
    assert_eq!(
        diagnostics[4].status, "skipped",
        "a write statement only shows that it was skipped and is not replayed"
    );
    assert!(
        matches!(diagnostics[5].status, "skipped" | "unavailable"),
        "a failed query must not report a successful plan"
    );
    let final_views: i64 =
        gono_forum_web::db::query_scalar("SELECT views FROM gonof_forum_thread WHERE tid = 101")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        final_views,
        original_views + 1,
        "diagnostics must not run the write again or the hostile SQL in a parameter"
    );
    gono_forum_web::db::query("UPDATE gonof_forum_thread SET views = ? WHERE tid = 101")
        .bind(original_views)
        .execute(&db.pool)
        .await
        .unwrap();
}

fn panel(body: &str) -> &str {
    &body[body
        .find("<details id=\"gonof-debug\"")
        .expect("must contain a debug panel")..]
}

fn query_count(body: &str) -> usize {
    panel(body)
        .split("data-debug-queries=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// The three footer values are always on; Redis records are merged into the same panel, before SQL.
async fn assert_footer_metrics(db: &Database, cookie: &str) {
    const BRAND: &str = r#"<a class="colophon__brand" href="https://www.gono.forum" target="_blank" rel="noopener noreferrer">Gono Forum</a>"#;

    // Debugging off: the text and values are still there, but no panel.
    set_panel(db, false).await;
    let plain = router::build(support::config_for(db), &project_root(), db.clone());
    let (_, _, body) = support::fetch(&plain, "/", false).await;
    assert!(body.contains(BRAND), "the footer must link to Gono Forum");
    assert!(
        footer(&body).contains(" ms · "),
        "the footer must give the processing time: {body}"
    );
    assert!(!body.contains("gonof-debug"));
    // The values come from this request: the home page must read the database at least once.
    // The footer writes MySQL or SQLite by this site's backend.
    let database = format!("{} ", db.backend().label());
    assert!(
        footer_number(&body, &database) > 0,
        "the footer count for {database}must not be 0: {body}"
    );

    // Debugging on and Redis available: in the same panel the Redis section comes before the SQL section.
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    let (_, _, body) = authorized_fetch(&app, "/", false, cookie).await;
    assert_eq!(
        body.matches(r#"id="gonof-debug""#).count(),
        1,
        "there must be exactly one panel"
    );
    let merged = panel(&body);
    let redis = merged
        .find("<h2>Redis ")
        .expect("the panel has no Redis section");
    let sql = merged
        .find("<h2>SQL records ")
        .expect("the panel has no SQL section");
    assert!(
        redis < sql,
        "the Redis section comes before the SQL section"
    );
    assert!(
        footer_number(&body, "Redis ") > 0,
        "the home page reads the shared cache twice: {body}"
    );

    // Debugging on but Redis disabled: no Redis section in the panel and no Redis count in the footer.
    let mut config = support::config_for(db);
    config.redis.enabled = false;
    let app = router::build(config, &project_root(), db.clone());
    let (_, _, body) = authorized_fetch(&app, "/", false, cookie).await;
    let without_redis = panel(&body);
    assert!(without_redis.contains("<h2>SQL records "));
    assert!(!without_redis.contains("<h2>Redis "));
    assert!(
        footer_number(&body, &database) > 0,
        "the footer still counts {database}without Redis: {body}"
    );
    let footer = footer(&body);
    assert!(
        !footer.contains("Redis"),
        "the footer must leave out the Redis count without Redis: {footer}"
    );
}

/// The text of the footer's legal line.
fn footer(body: &str) -> &str {
    body.split_once(r#"<p class="colophon__legal">"#)
        .and_then(|(_, rest)| rest.split_once("</p>"))
        .map(|(footer, _)| footer)
        .unwrap_or_else(|| panic!("the page has no footer: {body}"))
}

/// Extracts the numbers from `MySQL 14` (`SQLite 14` on SQLite sites) / `Redis 4` in the footer line
/// `19.1 ms · MySQL 14 · Redis 4`.
fn footer_number(body: &str, label: &str) -> usize {
    let footer = footer(body);
    let tail = footer
        .split_once(label)
        .unwrap_or_else(|| panic!("the footer has no {label}: {footer}"))
        .1;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("the footer {label} is not a number: {footer}"))
}

async fn assert_html_responses(db: &Database, cookie: &str) {
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    for (path, expected) in [
        ("/", StatusCode::OK),
        ("/ai", StatusCode::OK),
        ("/page-2", StatusCode::OK),
        ("/thread-103", StatusCode::OK),
        ("/search?q=debug_unique_search", StatusCode::OK),
        ("/?sort=invalid", StatusCode::BAD_REQUEST),
        ("/not-existing", StatusCode::NOT_FOUND),
        ("/missing/deeper/path", StatusCode::NOT_FOUND),
        ("/thread-999", StatusCode::NOT_FOUND),
    ] {
        for htmx in [false, true] {
            let (anonymous_status, _, anonymous) = support::fetch(&app, path, htmx).await;
            assert_eq!(anonymous_status, expected);
            assert_no_debug(&anonymous);
            if path == "/ai" {
                // The anonymous request primes Redis. Advance the authoritative
                // revision so this binding-display assertion observes a real SQL call.
                gono_forum_web::db::query(
                    "UPDATE gonof_forum_thread SET views = views WHERE tid = 101",
                )
                .execute(db.executor())
                .await
                .unwrap();
            }
            let (status, headers, body) = authorized_fetch(&app, path, htmx, cookie).await;
            assert_eq!(status, expected, "{path}");
            assert!(
                headers[header::CACHE_CONTROL]
                    .to_str()
                    .unwrap()
                    .contains("no-store")
            );
            assert_eq!(body.matches("id=\"gonof-debug\"").count(), 1);
            assert_eq!(body.matches("/static/css/debug.css").count(), 1);
            assert_eq!(body.matches("/static/js/debug.js").count(), 1);
            let panel = panel(&body);
            assert!(panel.contains(" open data-debug-queries="));
            assert!(panel.contains(&format!("HTTP {}", expected.as_u16())));
            assert!(panel.contains("RSS is the resident memory of the process"));
            assert!(panel.contains("both include concurrent requests and background tasks"));
            assert!(body.find("</footer>").unwrap() < body.find("id=\"gonof-debug\"").unwrap());
            assert_eq!(
                query_count(&body),
                panel.matches("data-debug-query=\"").count()
            );
            assert_eq!(
                query_count(&body),
                panel.matches("data-debug-index-status=\"").count(),
                "every SQL record shows its own index diagnostic"
            );
            assert!(panel.contains("Indexes (EXPLAIN plan): "));
            assert!(query_count(&body) > 0);
            // Readable forums come from the process cache: the panel lists the member slot separately.
            assert!(
                panel.contains("data-debug-block=\"forums-members\""),
                "{panel}"
            );
            assert!(!panel.contains("data-debug-expansion-error"));
            // The global process cache is listed separately: this request read the settings block; earlier requests in
            // the same application already loaded it, so this is a hit, and the SQL below no longer has the settings
            // read.
            assert!(panel.contains("In-process cache"), "{panel}");
            assert!(panel.contains("data-debug-block=\"settings\""), "{panel}");
            assert!(
                panel.contains("data-debug-block=\"settings\" data-debug-block-hit=\"true\""),
                "the anonymous request before loaded the settings block, so this one is a cache hit"
            );
            assert!(panel.contains("From the in-process cache"));
            if path == "/ai" {
                // The forum page is located by top-level forum ID (`family_fid`); the SQL in the panel already has
                // the parameters substituted.
                let fid: i64 = gono_forum_web::db::query_scalar(
                    "SELECT fid FROM gonof_forum_forum WHERE slug='ai'",
                )
                .fetch_one(db.executor())
                .await
                .unwrap();
                assert!(panel.contains(&format!("t.family_fid = {fid}")), "{panel}");
            }
        }
    }
    let (first, second) = tokio::join!(
        authorized_fetch(&app, "/search?q=http_scope_first", true, cookie),
        authorized_fetch(&app, "/search?q=http_scope_second", true, cookie)
    );
    assert!(panel(&first.2).contains("http_scope_first"));
    assert!(!panel(&first.2).contains("http_scope_second"));
    assert!(panel(&second.2).contains("http_scope_second"));
    assert!(!panel(&second.2).contains("http_scope_first"));
    let unsafe_text = "</code><script>alert('coins')</script>";
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", unsafe_text)
        .finish();
    let (_, _, escaped) = authorized_fetch(&app, &format!("/search?{query}"), false, cookie).await;
    assert!(!panel(&escaped).contains(unsafe_text));
    assert!(panel(&escaped).contains("&#60;script&#62;"));
    // The admin area turns off the debug panel (formerly `app_debug = false`): the same set of URLs no longer get the
    // panel injected even for administrators. The switch is in the database, so a newly built application reads it as
    // off on its first request.
    set_panel(db, false).await;
    let disabled = router::build(support::config_for(db), &project_root(), db.clone());
    for (path, expected) in [
        ("/", StatusCode::OK),
        ("/ai", StatusCode::OK),
        ("/page-2", StatusCode::OK),
        ("/thread-103", StatusCode::OK),
        ("/search?q=debug_unique_search", StatusCode::OK),
        ("/?sort=invalid", StatusCode::BAD_REQUEST),
        ("/not-existing", StatusCode::NOT_FOUND),
        ("/missing/deeper/path", StatusCode::NOT_FOUND),
        ("/thread-999", StatusCode::NOT_FOUND),
    ] {
        for htmx in [false, true] {
            let (off_status, _, off) = authorized_fetch(&disabled, path, htmx, cookie).await;
            assert_eq!(off_status, expected);
            for debug_output in [
                "gonof-debug",
                "/static/css/debug.css",
                "/static/js/debug.js",
                "FROM gonof_forum_forum",
            ] {
                assert!(
                    !off.contains(debug_output),
                    "disabled output: {debug_output}"
                );
            }
        }
    }
}

/// State for applications with the debug layer attached directly (not through the router): panel on, Redis on (same as
/// the former `(db, true)`).
async fn panel_state(db: &Database) -> debug::DebugState {
    set_panel(db, true).await;
    let config = support::config_for(db);
    debug::DebugState {
        database: db.clone(),
        redis_enabled: true,
        global: gono_forum_web::cache::GlobalCache::new(
            gono_forum_web::cache::Cache::new(&config.redis).unwrap(),
        ),
    }
}

/// The debug panel switch is the admin setting `settings.debug.panel` (docs/settings-and-caching.md, "Profiling").
/// Changes the database directly: the debug layer reads it from the application's process cache on each request, so
/// a newly built application reads the new value on its first request.
async fn set_panel(db: &Database, on: bool) {
    gono_forum_web::test_support::set_setting(
        db,
        "settings.debug.panel",
        if on { "true" } else { "false" },
    )
    .await
    .unwrap();
}

async fn assert_sql_carriage_returns_in_html(db: &Database) {
    // Literal CR/CRLF in SQL must survive HTML parsing; literal entity text stays text.
    const SQL: &str = "SELECT '<script>&#13;\rlone CR\r\nCRLF</script>' AS value";
    let diagnostics_db = db.clone();
    let db = db.clone();
    let app = Router::new()
        .route(
            "/literal-sql",
            get(move || {
                let db = db.clone();
                async move {
                    let row = gono_forum_web::db::query(SQL)
                        .fetch_one(db.executor())
                        .await
                        .unwrap();
                    assert_eq!(
                        gono_forum_web::db::text(&row, "value").unwrap(),
                        "<script>&#13;\rlone CR\r\nCRLF</script>"
                    );
                    let mut response = Html(
                        "<!doctype html><html><body><footer>SQL rendering check</footer></body></html>",
                    )
                    .into_response();
                    response.extensions_mut().insert(debug::Authorized);
                    response
                }
            }),
        )
        .layer(middleware::from_fn_with_state(
            panel_state(&diagnostics_db).await,
            debug::middleware,
        ));
    let (status, _, body) = support::fetch(&app, "/literal-sql", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        query_count(&body) - panel(&body).matches("SQLx driver batch").count(),
        1,
        "one application query; a cold connection setup stays a driver record"
    );
    assert_eq!(
        panel(&body)
            .matches("data-debug-index-status=\"none\"")
            .count(),
        1
    );
    let sql_html = panel(&body)
        .split("<code data-debug-sql>")
        .nth(1)
        .unwrap()
        .split("</code>")
        .next()
        .unwrap();
    assert_eq!(
        sql_html,
        "SELECT &#39;&#60;script&#62;&#38;#13;&#13;lone CR&#13;\nCRLF&#60;/script&#62;&#39; AS value"
    );
    assert!(!sql_html.contains('\r'));
    assert!(!sql_html.contains("<script>"));
}

async fn assert_non_html_responses(db: &Database, cookie: &str) {
    set_panel(db, true).await;
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    for path in [
        "/healthz",
        "/robots.txt",
        "/static/css/forum.css",
        "/static/js/debug.js",
    ] {
        let (status, _, body) = authorized_fetch(&app, path, false, cookie).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!body.contains("<details id=\"gonof-debug\""));
    }
    let (status, _, body) = authorized_fetch(&app, "/page-01", false, cookie).await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert!(!body.contains("gonof-debug"));
    let head = app
        .oneshot(
            Request::builder()
                .method("HEAD")
                .header(header::COOKIE, cookie)
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    // The current product has no JSON route; test the same installed middleware on a real DB handler.
    let diagnostics_db = db.clone();
    let db = db.clone();
    let json_app = Router::new()
        .route(
            "/json",
            get(move || {
                let db = db.clone();
                async move {
                    let mut response = Json(
                        serde_json::json!({"forums": content::forums(&db).await.unwrap().len()}),
                    )
                    .into_response();
                    response.extensions_mut().insert(debug::Authorized);
                    response
                }
            }),
        )
        .layer(middleware::from_fn_with_state(
            panel_state(&diagnostics_db).await,
            debug::middleware,
        ));
    let (status, headers, body) = support::fetch(&json_app, "/json", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(body, "{\"forums\":6}");
}

/// Database unreachable: the debug layer cannot read `settings.debug.panel` and treats it as off, so the error page
/// carries no debug output.
pub async fn assert_disconnected_debug_page(db: &Database) {
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    let (status, _, body) = support::fetch(&app, "/", false).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_no_debug(&body);
    for private in ["SELECT ", "gonof_forum_", "gonof_common_"] {
        assert!(!body.contains(private));
    }
}
