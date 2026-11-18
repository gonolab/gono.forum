//! Thread view counts (docs/queries.md, "Views"; docs/settings-and-caching.md, "View counting"): which requests count,
//! once per member or guest per 30 minutes with Redis on and off, shared across processes through Redis, and the
//! buffered write: one flush adds every pending view, leaves `cache_revision` alone, ignores threads that are gone and
//! runs once more when the writing task stops.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use gono_forum_web::{
    cache::{Cache, GlobalCache},
    content::{
        self,
        views::{ADD_VIEWS, FLUSH_PERIOD, Views, Visitor, WINDOW_SECONDS},
    },
    db::{self, Database},
    identity, router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use sqlx::{Column, Row};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// A public thread of the development fixture.
const TID: i64 = 103;
const MEMBER: i64 = 9101;
const FIREFOX: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.6; rv:132.0) Gecko/20100101 Firefox/132.0";
const SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15";

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

async fn views_and_revision(db: &Database, tid: i64) -> (i64, i64) {
    (
        scalar(
            db,
            &format!("SELECT views FROM gonof_forum_thread WHERE tid={tid}"),
        )
        .await,
        scalar(
            db,
            &format!("SELECT cache_revision FROM gonof_forum_thread WHERE tid={tid}"),
        )
        .await,
    )
}

/// A request for `path` with the given method, session and headers; returns the status and the body.
async fn send(
    app: &Router,
    method: Method,
    path: &str,
    session: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(session) = session {
        request = request.header(
            header::COOKIE,
            format!("gonof_session__port58833={session}"),
        );
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// An integer the way English writes it (`12,345`), as the page formats counts.
fn grouped(value: i64) -> String {
    let digits = value.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

async fn open(app: &Router, session: Option<&str>, headers: &[(&str, &str)]) -> StatusCode {
    send(
        app,
        Method::GET,
        &format!("/thread-{TID}"),
        session,
        headers,
    )
    .await
    .0
}

/// A migrated fixture with the development content and one member who can sign in.
async fn fixture() -> TestFixture {
    let services = TestFixture::migrated().await.unwrap();
    content::seed_forums(&services.db).await.unwrap();
    content::seed_fixture(&services.db).await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,1,?)")
        .bind(MEMBER)
        .bind("view-counter")
        .bind("view-counter@example.test")
        .bind(identity::now() - 172_800)
        .execute(services.db.executor())
        .await
        .unwrap();
    services
}

/// An application on the fixture's database with its own view counter; `redis` false builds it as a deployment
/// without Redis.
fn app(services: &TestFixture, redis: bool) -> (Router, Views) {
    let mut config = services.config.clone();
    config.redis.enabled = redis;
    let cache = if redis {
        services.cache.clone()
    } else {
        Cache::new(&config.redis).unwrap()
    };
    let views = Views::new(cache.clone());
    let global = GlobalCache::new(cache.clone());
    let app = router::build_with_views(
        config,
        &services.root,
        services.db.clone(),
        cache,
        global,
        views.clone(),
    );
    (app, views)
}

async fn session(services: &TestFixture) -> String {
    let identity = identity::Identity::new(
        services.db.clone(),
        services.config.identity.clone(),
        services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    http_auth::login(&identity, &services.db, MEMBER)
        .await
        .session_token
}

/// The counting rules, the same with Redis on and off.
async fn counts_members_and_guests_once(redis: bool) {
    let services = fixture().await;
    let db = services.db.clone();
    let (app, views) = app(&services, redis);
    let session = session(&services).await;
    let (before, revision) = views_and_revision(&db, TID).await;
    let browser = [("user-agent", FIREFOX)];

    // A guest counts once; opening the thread again, as a full page or a boosted navigation, does not count.
    assert_eq!(open(&app, None, &browser).await, StatusCode::OK);
    assert_eq!(views.pending(), 1);
    assert_eq!(open(&app, None, &browser).await, StatusCode::OK);
    assert_eq!(
        open(
            &app,
            None,
            &[
                ("user-agent", FIREFOX),
                ("hx-request", "true"),
                ("hx-boosted", "true")
            ]
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(views.pending(), 1);
    // Another browser on the same address is another guest.
    assert_eq!(
        open(&app, None, &[("user-agent", SAFARI)]).await,
        StatusCode::OK
    );
    assert_eq!(views.pending(), 2);
    // A member counts once by uid, whichever browser they use.
    assert_eq!(open(&app, Some(&session), &browser).await, StatusCode::OK);
    assert_eq!(
        open(&app, Some(&session), &[("user-agent", SAFARI)]).await,
        StatusCode::OK
    );
    assert_eq!(views.pending(), 3);

    // None of these count, although a new guest would (a user agent not seen yet).
    let fresh = "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";
    for (method, headers) in [
        (Method::HEAD, vec![("user-agent", fresh)]),
        (
            Method::GET,
            vec![("user-agent", fresh), ("hx-request", "true")],
        ),
        (
            Method::GET,
            vec![("user-agent", fresh), ("sec-purpose", "prefetch")],
        ),
        (
            Method::GET,
            vec![("user-agent", fresh), ("purpose", "prefetch")],
        ),
        (
            Method::GET,
            vec![(
                "user-agent",
                "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
            )],
        ),
        (Method::GET, vec![("user-agent", "")]),
        (Method::GET, vec![]),
    ] {
        let (status, _) = send(
            &app,
            method.clone(),
            &format!("/thread-{TID}"),
            None,
            &headers,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method} {headers:?}");
        assert_eq!(views.pending(), 3, "{method} {headers:?} does not count");
    }
    // Only a page that showed the thread counts: not a missing thread or page, nor a redirect to the canonical address.
    let unseen = [("user-agent", fresh)];
    for path in [
        "/thread-999999".to_owned(),
        format!("/thread-{TID}-99"),
        format!("/thread-{TID}-1"),
    ] {
        let (status, _) = send(&app, Method::GET, &path, None, &unseen).await;
        assert_ne!(status, StatusCode::OK, "{path}");
    }
    assert_eq!(views.pending(), 3);

    // Nothing is written until the flush, which writes the three views in one statement for the thread and leaves
    // the thread version alone.
    assert_eq!(views_and_revision(&db, TID).await, (before, revision));
    assert_eq!(views.flush(&db).await.unwrap(), 1);
    assert_eq!(views.pending(), 0);
    assert_eq!(views_and_revision(&db, TID).await, (before + 3, revision));
    // The page shows the written number; the deduplication still holds after the flush.
    let (_, html) = send(&app, Method::GET, &format!("/thread-{TID}"), None, &browser).await;
    assert!(
        html.contains(&format!(
            "<span class=\"thread-stats__views\">{} views</span>",
            grouped(before + 3)
        )),
        "the thread head shows the views"
    );
    assert_eq!(views.pending(), 0);
    assert_eq!(views.flush(&db).await.unwrap(), 0);

    let mut conn = db.pool.acquire().await.unwrap();
    let drift = gono_forum_web::test_support::content_drift(&mut conn)
        .await
        .unwrap();
    drop(conn);
    assert!(drift.iter().all(|(_, amount)| *amount == 0), "{drift:?}");
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn members_and_guests_count_once_with_redis() {
    counts_members_and_guests_once(true).await;
}

#[tokio::test]
async fn members_and_guests_count_once_without_redis() {
    counts_members_and_guests_once(false).await;
}

/// With Redis on, the deduplication key is shared by every process and lives for the window; so is the daily key
/// guests are hashed with. Without Redis, each process remembers its own visitors.
#[tokio::test]
async fn processes_share_the_window_through_redis_only() {
    let services = fixture().await;
    let browser = [("user-agent", FIREFOX)];
    let (first, first_views) = app(&services, true);
    let (second, second_views) = app(&services, true);
    assert_eq!(open(&first, None, &browser).await, StatusCode::OK);
    assert_eq!(open(&second, None, &browser).await, StatusCode::OK);
    assert_eq!(
        (first_views.pending(), second_views.pending()),
        (1, 0),
        "a guest seen by one process is not counted again by another"
    );
    let session = session(&services).await;
    assert_eq!(
        open(&second, Some(&session), &browser).await,
        StatusCode::OK
    );
    assert_eq!(open(&first, Some(&session), &browser).await, StatusCode::OK);
    assert_eq!((first_views.pending(), second_views.pending()), (1, 1));

    let client = redis::Client::open(services.config.redis.url.as_str()).unwrap();
    let mut redis = client.get_multiplexed_async_connection().await.unwrap();
    let namespace = &services.config.redis.namespace;
    let ttl: i64 = redis::cmd("TTL")
        .arg(format!("{namespace}:view:{TID}:u{MEMBER}"))
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!(
        (WINDOW_SECONDS - 60..=WINDOW_SECONDS).contains(&ttl),
        "the member's key expires with the window: {ttl}"
    );
    // SCAN rather than KEYS: the Redis is shared with other projects.
    let mut keys = Vec::new();
    let mut cursor = 0_u64;
    loop {
        let (next, found): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("{namespace}:view*"))
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut redis)
            .await
            .unwrap();
        keys.extend(found);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    let guests: Vec<_> = keys
        .iter()
        .filter(|key| key.starts_with(&format!("{namespace}:view:{TID}:g")))
        .collect();
    assert_eq!(guests.len(), 1, "{keys:?}");
    // The guest part is a digest: neither the address nor the browser appears in it.
    assert_eq!(guests[0].rsplit(':').next().unwrap().len(), 33);
    let day = identity::now().div_euclid(86_400);
    let daily: i64 = redis::cmd("TTL")
        .arg(format!("{namespace}:view-key:{day}"))
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!(
        daily > 0 && daily <= 86_400 + 300,
        "the daily key expires shortly after its day: {daily}"
    );

    let (third, third_views) = app(&services, false);
    let (fourth, fourth_views) = app(&services, false);
    assert_eq!(open(&third, None, &browser).await, StatusCode::OK);
    assert_eq!(open(&fourth, None, &browser).await, StatusCode::OK);
    assert_eq!(
        (third_views.pending(), fourth_views.pending()),
        (1, 1),
        "without Redis each process counts a visitor once (docs/settings-and-caching.md, \"View counting\")"
    );
    services.cleanup().await.unwrap();
}

/// The writing task flushes on its period and once more when it is stopped; a thread that no longer exists is not an
/// error, and the other threads of the batch are written.
#[tokio::test]
async fn the_writing_task_flushes_periodically_and_at_shutdown() {
    let services = fixture().await;
    let db = services.db.clone();
    let views = Views::new(services.cache.clone());
    let now = identity::now();
    assert_eq!(FLUSH_PERIOD, Duration::from_secs(60));

    // Stopped before its first period: the shutdown flush writes everything.
    let (before, revision) = views_and_revision(&db, TID).await;
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(views.clone().run(
        db.clone(),
        Duration::from_secs(3600),
        shutdown.clone(),
    ));
    for uid in 1..=4 {
        assert!(
            views
                .record(TID as u64, Visitor::Member(uid), now)
                .await
                .unwrap()
        );
    }
    // A thread that is gone (rows are never deleted by the forum, so a missing id stands in for it).
    assert!(
        views
            .record(999_999, Visitor::Member(1), now)
            .await
            .unwrap()
    );
    assert_eq!(views.pending(), 5);
    shutdown.cancel();
    task.await.unwrap();
    assert_eq!(
        views.pending(),
        0,
        "the gone thread's view is dropped quietly"
    );
    assert_eq!(views_and_revision(&db, TID).await, (before + 4, revision));

    // Running: the period flush writes without being stopped.
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(views.clone().run(
        db.clone(),
        Duration::from_millis(100),
        shutdown.clone(),
    ));
    assert!(
        views
            .record(TID as u64, Visitor::Member(5), now)
            .await
            .unwrap()
    );
    let mut written = false;
    for _ in 0..100 {
        if views_and_revision(&db, TID).await.0 == before + 5 {
            written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(written, "the periodic flush wrote the view");
    shutdown.cancel();
    task.await.unwrap();
    assert_eq!(views_and_revision(&db, TID).await, (before + 5, revision));
    services.cleanup().await.unwrap();
}

/// The default skin shows the count in list rows and the thread head (docs/skins.md, "List rows").
#[tokio::test]
async fn lists_and_the_thread_head_show_the_count() {
    let services = fixture().await;
    let db = services.db.clone();
    let (app, _) = app(&services, true);
    let views = scalar(
        &db,
        &format!("SELECT views FROM gonof_forum_thread WHERE tid={TID}"),
    )
    .await;
    let shown = grouped(views);
    let (_, home) = send(&app, Method::GET, "/", None, &[]).await;
    let at = home
        .find(&format!("data-thread-id=\"{TID}\""))
        .expect("the thread is on the home page");
    let row = &home[at..at + home[at..].find("</li>").unwrap()];
    assert!(
        row.contains(&format!(
            "<span class=\"row-meta__views\" aria-label=\"{shown} views\">"
        )) && row.contains(&format!(
            "<span class=\"stat\"><span class=\"stat__value\">{shown}</span></span>"
        )),
        "{row}"
    );
    db::query("UPDATE gonof_forum_thread SET views=1 WHERE tid=?")
        .bind(TID)
        .execute(db.executor())
        .await
        .unwrap();
    let (_, html) = send(&app, Method::GET, &format!("/thread-{TID}"), None, &[]).await;
    assert!(html.contains("<span class=\"thread-stats__views\">1 view</span>"));
    db::query("UPDATE gonof_forum_thread SET views=12345 WHERE tid=?")
        .bind(TID)
        .execute(db.executor())
        .await
        .unwrap();
    let (_, html) = send(&app, Method::GET, &format!("/thread-{TID}"), None, &[]).await;
    assert!(html.contains("<span class=\"thread-stats__views\">12,345 views</span>"));
    services.cleanup().await.unwrap();
}

/// The view update seeks the primary key and changes one row on MySQL 5.7 and 8.4 (docs/queries.md, "Views"); `views`
/// is in no index, so no index is rewritten.
#[tokio::test]
async fn the_view_update_is_a_primary_key_update() {
    gono_forum_web::mysql_only!();
    let services = fixture().await;
    let statement = format!("EXPLAIN {ADD_VIEWS}");
    let rows = sqlx::query(&statement)
        .persistent(false)
        .bind(1_i64)
        .bind(TID)
        .fetch_all(services.db.pool.as_mysql().expect("MySQL only"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let plan: Vec<(String, String)> = row
        .columns()
        .iter()
        .map(|column| {
            // Integer and float columns first: decoded as text they come out as raw bytes.
            let at = column.ordinal();
            let value = match column.name() {
                "id" | "rows" => row
                    .try_get_unchecked::<Option<u64>, _>(at)
                    .unwrap()
                    .map(|value| value.to_string()),
                "filtered" => row
                    .try_get_unchecked::<Option<f64>, _>(at)
                    .unwrap()
                    .map(|value| value.to_string()),
                _ => row.try_get_unchecked::<Option<String>, _>(at).unwrap(),
            }
            .unwrap_or_else(|| "NULL".to_owned());
            (column.name().to_owned(), value)
        })
        .collect();
    eprintln!("{plan:?}");
    let get = |name: &str| {
        plan.iter()
            .find(|(column, _)| column == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("EXPLAIN has no {name} column"))
    };
    assert_eq!(get("select_type"), "UPDATE");
    assert_eq!(get("type"), "range");
    assert_eq!(get("key"), "PRIMARY");
    assert_eq!(get("rows"), "1");
    let indexed: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='gonof_forum_thread' AND COLUMN_NAME='views'",
    )
    .fetch_one(services.db.executor())
    .await
    .unwrap();
    assert_eq!(indexed, 0, "views is in no index");
    services.cleanup().await.unwrap();
}
