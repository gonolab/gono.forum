use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
};
use gono_forum_web::{
    cache::{Cache, CacheKey, registry},
    config::RedisConfig,
    test_support::TestFixture,
};
use tower::ServiceExt;

struct RedisProxy {
    unavailable: Arc<AtomicBool>,
    /// Upstream connections the proxy has accepted: incremented whenever a connection is discarded and rebuilt.
    connections: Arc<AtomicUsize>,
    disconnect: Arc<tokio::sync::Notify>,
    worker: tokio::task::JoinHandle<()>,
    config: RedisConfig,
}

impl RedisProxy {
    async fn new(config: &RedisConfig) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = url::Url::parse(&config.url).unwrap();
        assert_eq!(
            upstream.scheme(),
            "redis",
            "the fault-injection proxy only handles the plain TCP connection to the test Redis"
        );
        let host = upstream.host_str().unwrap().to_owned();
        let port = upstream.port().unwrap_or(6379);
        let unavailable = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let disconnect = Arc::new(tokio::sync::Notify::new());
        let blocked = unavailable.clone();
        let accepted = connections.clone();
        let signal = disconnect.clone();
        let worker = tokio::spawn(async move {
            while let Ok((mut incoming, _)) = listener.accept().await {
                if blocked.load(Ordering::Acquire) {
                    continue;
                }
                accepted.fetch_add(1, Ordering::SeqCst);
                let host = host.clone();
                let signal = signal.clone();
                tokio::spawn(async move {
                    if let Ok(mut outgoing) =
                        tokio::net::TcpStream::connect((host.as_str(), port)).await
                    {
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing) => {},
                            _ = signal.notified() => {},
                        }
                    }
                });
            }
        });
        let mut config = config.clone();
        let mut proxied = upstream;
        proxied.set_host(Some("127.0.0.1")).unwrap();
        proxied.set_port(Some(address.port())).unwrap();
        config.url = proxied.to_string();
        config.timeout_milliseconds = 300;
        Self {
            unavailable,
            connections,
            disconnect,
            worker,
            config,
        }
    }
}

impl Drop for RedisProxy {
    fn drop(&mut self) {
        self.worker.abort();
        self.disconnect.notify_waiters();
    }
}

/// TTLs for tests: statistics segmented by 30 seconds (the setting's minimum), everything else 60 seconds. Production
/// values come from the site settings `settings.cache.*` (`cache::Lifetimes::from_settings`).
const LIFETIMES: gono_forum_web::cache::Lifetimes = gono_forum_web::cache::Lifetimes {
    member: 30,
    page: 60,
    list: 60,
    statistics: 30,
    forum_statistics: 30,
};

#[tokio::test]
async fn cache_revisions_variants_and_namespaces_are_isolated() {
    let services = TestFixture::new().await.unwrap();
    let first = CacheKey::new(&registry::THREAD_LIST, 10, &("daily", 1, "normal", false)).unwrap();
    let newer = CacheKey::new(&registry::THREAD_LIST, 11, &("daily", 1, "normal", false)).unwrap();
    let htmx = CacheKey::new(&registry::THREAD_LIST, 10, &("daily", 1, "normal", true)).unwrap();
    let page_two =
        CacheKey::new(&registry::THREAD_LIST, 10, &("daily", 2, "normal", false)).unwrap();
    services
        .cache
        .set_json(&first, &vec![1_i64, 2], LIFETIMES)
        .await
        .unwrap();
    assert_eq!(
        services.cache.get_json::<Vec<i64>>(&first).await.unwrap(),
        Some(vec![1, 2])
    );
    for key in [&newer, &htmx, &page_two] {
        assert!(
            services
                .cache
                .get_json::<Vec<i64>>(key)
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut peer_config = services.config.redis.clone();
    peer_config.namespace.push_str(":peer");
    let peer = Cache::connect(&peer_config).await.unwrap();
    assert!(peer.get_json::<Vec<i64>>(&first).await.unwrap().is_none());

    // Simulate an old request finishing after the new version was committed: it may only backfill the old version and
    // must not overwrite the new version's key.
    services
        .cache
        .set_json(&newer, &vec![3_i64], LIFETIMES)
        .await
        .unwrap();
    services
        .cache
        .set_json(&first, &vec![99_i64], LIFETIMES)
        .await
        .unwrap();
    assert_eq!(
        services.cache.get_json::<Vec<i64>>(&newer).await.unwrap(),
        Some(vec![3])
    );
    services.cleanup().await.unwrap();
}

/// When the cache structure changes (deploying a new version, old and new processes writing to each other during a
/// rolling release), an undecodable old format may be read: treat it as a miss and delete the key; it must not
/// become an error.
#[tokio::test]
async fn undecodable_cache_entries_are_misses_and_get_removed() {
    let services = TestFixture::new().await.unwrap();
    let key = CacheKey::new(&registry::FLOORS, 1, &"schema-change").unwrap();
    // The old version wrote a string; the new version reads a list.
    services
        .cache
        .set_json(&key, &"legacy shape", LIFETIMES)
        .await
        .unwrap();
    assert_eq!(
        services.cache.get_json::<Vec<i64>>(&key).await.unwrap(),
        None
    );
    assert_eq!(
        services.cache.get_json::<String>(&key).await.unwrap(),
        None,
        "an undecodable key is deleted, not left until its TTL expires"
    );
    services.cleanup().await.unwrap();
}

/// Anonymous pages still return 200 when every cache entry has been written in the old format, and after going to
/// the source they replace those keys with the new format.
#[tokio::test]
async fn anonymous_pages_survive_cache_entries_in_an_old_format() {
    let services = TestFixture::migrated().await.unwrap();
    gono_forum_web::content::seed_forums(&services.db)
        .await
        .unwrap();
    gono_forum_web::content::seed_fixture(&services.db)
        .await
        .unwrap();
    // Attach the fixture's process cache handle: site-wide statistics and the anonymous newest member check the
    // process cache first (docs/settings-and-caching.md, "The process cache"), so after corrupting Redis this
    // process's layer must be cleared first for the second round of requests to read those two Redis keys.
    let app = gono_forum_web::router::build_with_global(
        services.config.clone(),
        &gono_forum_web::project_root(),
        services.db.clone(),
        services.cache.clone(),
        services.global.clone(),
    );
    let paths = ["/", "/thread-103", "/thread-103?view=flat"];
    let get = |path: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    for path in paths {
        assert_eq!(get(path).await, StatusCode::OK, "{path}");
    }

    // Only touch cache keys under this fixture's namespace.
    let mut redis = redis::Client::open(services.config.redis.url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let pattern = format!("{}:cache:*", services.config.redis.namespace);
    // A shared instance, so do not scan the whole database with KEYS.
    let mut keys: Vec<String> = Vec::new();
    let mut cursor = 0_u64;
    loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(100)
            .query_async(&mut redis)
            .await
            .unwrap();
        keys.extend(batch);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    assert!(
        keys.len() >= 4,
        "lists, sidebar, floors and rendered posts must all be cached: {keys:?}"
    );
    const LEGACY: &str = r#"{"schema":"legacy"}"#;
    for key in &keys {
        redis::cmd("SET")
            .arg(key)
            .arg(LEGACY)
            .arg("EX")
            .arg(60)
            .query_async::<()>(&mut redis)
            .await
            .unwrap();
    }
    services.global.clear_local();

    for path in paths {
        assert_eq!(get(path).await, StatusCode::OK, "{path}");
    }
    let mut legacy = 0;
    for key in &keys {
        let value: Option<String> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut redis)
            .await
            .unwrap();
        legacy += usize::from(value.as_deref() == Some(LEGACY));
    }
    // Sidebar keys are segmented by 30 seconds; across a segment boundary the second round reads a new key and the old
    // key stays as is, so at most one remains.
    assert!(
        legacy <= 1,
        "keys in the old format are rewritten after reloading: {legacy}/{}",
        keys.len()
    );
    services.cleanup().await.unwrap();
}

/// The contract when Redis is unavailable: only calls that actually use the cache fail; all other requests are served
/// as usual.
///
/// Redis only holds rebuildable data and write paths never touch it, so there is no request-level gate any more.
/// Cache failures carry the [`Unavailable`] marker, which the response layer turns into a 503.
#[tokio::test]
async fn unavailable_redis_fails_cached_reads_only() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    let cache = Cache::new(&RedisConfig {
        url: format!("redis://{endpoint}/0"),
        timeout_milliseconds: 100,
        ..RedisConfig::default()
    })
    .unwrap();

    // Reading the cache fails, and the error chain carries the marker, so the response layer gives a 503 rather than a
    // generic 500.
    let key = CacheKey::new(&registry::FLOORS, 1, &"outage").unwrap();
    let error = cache.get_json::<i64>(&key).await.unwrap_err();
    assert!(
        error
            .downcast_ref::<gono_forum_web::cache::Unavailable>()
            .is_some(),
        "cache failures must be recognizable: {error:?}"
    );

    // Requests that do not read the cache are no longer affected: Redis only holds rebuildable display data.
    let handled = Arc::new(AtomicUsize::new(0));
    let count = handled.clone();
    let app = gono_forum_web::cache::protect(
        Router::new()
            .route(
                "/business",
                get(move || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    "business"
                })
                .post(|| async { StatusCode::CREATED }),
            )
            .route("/static/style.css", get(|| async { "body {}" })),
        cache,
    );
    for path in ["/business", "/static/style.css"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    assert_eq!(handled.load(Ordering::SeqCst), 1);

    // Write requests are not affected: Redis is not part of the write path.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/business")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn outage_after_sql_commit_keeps_the_write_and_recovers_the_connection() {
    let services = TestFixture::migrated().await.unwrap();
    let proxy = RedisProxy::new(&services.config.redis).await;
    let cache = Cache::connect(&proxy.config).await.unwrap();
    let database = services.db.clone();
    let blocked = proxy.unavailable.clone();
    let disconnect = proxy.disconnect.clone();
    let fail_once = Arc::new(AtomicBool::new(true));
    let app = gono_forum_web::cache::protect(Router::new().route("/commit", post(move || {
        let database = database.clone();
        let blocked = blocked.clone();
        let disconnect = disconnect.clone();
        let fail_once = fail_once.clone();
        async move {
            gono_forum_web::db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES('cache.commit.contract','committed') ON DUPLICATE KEY UPDATE svalue=svalue")
                .execute(database.executor()).await.unwrap();
            if fail_once.swap(false, Ordering::AcqRel) {
                blocked.store(true, Ordering::Release);
                disconnect.notify_waiters();
            }
            StatusCode::CREATED
        }
    })), cache.clone());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/commit")
            .body(Body::empty())
            .unwrap()
    };
    // Redis is only a cache and write paths never touch it, so a failure midway does not change this response.
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let committed: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_settings WHERE skey='cache.commit.contract'",
    )
    .fetch_one(services.db.executor())
    .await
    .unwrap();
    assert_eq!(
        committed, 1,
        "a dependency failure must not undo committed SQL"
    );
    proxy.unavailable.store(false, Ordering::Release);
    let mut recovered = false;
    for _ in 0..30 {
        if cache.ensure_available().await.is_ok() {
            recovered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        recovered,
        "after the proxy recovers the connection manager reconnects to Redis"
    );
    assert_eq!(
        app.oneshot(request()).await.unwrap().status(),
        StatusCode::CREATED
    );
    let committed: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_settings WHERE skey='cache.commit.contract'",
    )
    .fetch_one(services.db.executor())
    .await
    .unwrap();
    assert_eq!(committed, 1);
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn prolonged_disconnect_rebuilds_the_connection_without_an_application_restart() {
    let services = TestFixture::new().await.unwrap();
    let proxy = RedisProxy::new(&services.config.redis).await;
    let cache = Cache::connect(&proxy.config).await.unwrap();
    let key = CacheKey::new(&registry::FLOORS, 1, &"prolonged-disconnect").unwrap();
    cache.set_json(&key, &42_i64, LIFETIMES).await.unwrap();
    proxy.unavailable.store(true, Ordering::Release);
    proxy.disconnect.notify_waiters();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while tokio::time::Instant::now() < until {
        assert!(cache.ensure_available().await.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    proxy.unavailable.store(false, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(2), cache.ensure_available())
        .await
        .expect("once Redis is back the connection is rebuilt at once, without a long backoff")
        .unwrap();
    assert_eq!(cache.get_json::<i64>(&key).await.unwrap(), Some(42));
    services.cleanup().await.unwrap();
}

/// Redis returned a logical error (WRONGTYPE; OOM and READONLY are alike): with Redis enabled it is still a 503, but
/// the connection itself is fine and is neither discarded nor reconnected; the original error stays in the error
/// chain, visible in both the logs and the 503 page.
#[tokio::test]
async fn redis_reply_errors_keep_the_connection_and_the_original_error() {
    let services = TestFixture::new().await.unwrap();
    let proxy = RedisProxy::new(&services.config.redis).await;
    let cache = Cache::connect(&proxy.config).await.unwrap();
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 1);
    let key = CacheKey::new(&registry::FLOORS, 1, &"wrong-type").unwrap();
    cache.set_json(&key, &1_i64, LIFETIMES).await.unwrap();
    // Turn this key into a list: GET then gets WRONGTYPE. Only touch keys under this fixture's namespace.
    let mut redis = redis::Client::open(services.config.redis.url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    // A shared instance, so do not scan the whole database with KEYS; one round of SCAN may return nothing, so scan
    // until the cursor returns to zero.
    let mut keys: Vec<String> = Vec::new();
    let mut cursor = 0_u64;
    loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("{}:cache:*", services.config.redis.namespace))
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut redis)
            .await
            .unwrap();
        keys.extend(batch);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    assert_eq!(keys.len(), 1, "{keys:?}");
    redis::pipe()
        .del(&keys[0])
        .rpush(&keys[0], "not-json")
        .expire(&keys[0], 60)
        .query_async::<()>(&mut redis)
        .await
        .unwrap();

    let error = cache.get_json::<i64>(&key).await.unwrap_err();
    assert!(
        error
            .downcast_ref::<gono_forum_web::cache::Unavailable>()
            .is_some(),
        "with Redis enabled a failed command still counts as unavailable: {error:#}"
    );
    assert!(
        format!("{error:#}").contains("WRONGTYPE"),
        "the original error must be kept: {error:#}"
    );
    assert!(gono_forum_web::debug::visible_error(&error).contains("WRONGTYPE"));
    let other = CacheKey::new(&registry::FLOORS, 1, &"after-wrong-type").unwrap();
    cache.set_json(&other, &2_i64, LIFETIMES).await.unwrap();
    assert_eq!(cache.get_json::<i64>(&other).await.unwrap(), Some(2));
    assert_eq!(
        proxy.connections.load(Ordering::SeqCst),
        1,
        "an error reply does not mean the connection is broken, so it must not reconnect"
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn healthz_reports_mysql_and_redis_availability() {
    let services = TestFixture::migrated().await.unwrap();
    let proxy = RedisProxy::new(&services.config.redis).await;
    let mut config = services.config.clone();
    config.redis = proxy.config.clone();
    config.redis.enabled = true;
    let app = gono_forum_web::router::build(config, &services.root, services.db.clone());
    let check = |app: Router| async move {
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    };
    assert_eq!(check(app.clone()).await, (StatusCode::OK, "ok\n".into()));

    // Redis disconnected: 503, saying which dependency it is.
    proxy.unavailable.store(true, Ordering::Release);
    proxy.disconnect.notify_waiters();
    let (status, body) = check(app.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.starts_with("Redis unavailable: "), "{body}");
    proxy.unavailable.store(false, Ordering::Release);
    assert_eq!(
        check(app.clone()).await.0,
        StatusCode::OK,
        "no restart is needed after recovery"
    );

    // The database connection pool is unavailable: also 503, with this site's backend (MySQL or SQLite) in the
    // reason.
    let backend = services.db.backend().label();
    services.db.pool.close().await;
    let (status, body) = check(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.starts_with(&format!("{backend} unavailable: ")),
        "{body}"
    );
    services.cleanup().await.unwrap();
}

/// When the same key misses concurrently, only one goes to the source; the other requests wait for it
/// to write and then read the cache.
#[tokio::test]
async fn concurrent_misses_fill_a_key_once() {
    let services = TestFixture::new().await.unwrap();
    let key = CacheKey::new(&registry::STATISTICS, 0, &"single-fill").unwrap();
    let fills = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let cache = services.cache.clone();
            let key = key.clone();
            let fills = fills.clone();
            let release = release.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fill_json(&key, LIFETIMES, || async move {
                        fills.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(vec![7_i64])
                    })
                    .await
                    .unwrap()
            })
        })
        .collect();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while fills.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first request must start loading from the source");
    // Leave time for the other requests to reach the go-to-source step too: with queuing they can only wait.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        fills.load(Ordering::SeqCst),
        1,
        "one process loads one key from the source only once"
    );
    release.notify_waiters();
    for task in tasks {
        assert_eq!(task.await.unwrap(), vec![7]);
    }
    assert_eq!(fills.load(Ordering::SeqCst), 1);
    services.cleanup().await.unwrap();
}

/// Sidebar statistics are not invalidated with the content version but recomputed per statistics
/// segment (`settings.cache.statistics_seconds`, here at its minimum of 30 seconds); the hot and latest cards still
/// follow the content version, so titles are masked right after withdrawal.
#[tokio::test]
async fn sidebar_statistics_outlive_content_revisions_but_topics_do_not() {
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    gono_forum_web::content::seed_forums(db).await.unwrap();
    gono_forum_web::content::seed_fixture(db).await.unwrap();
    let start = gono_forum_web::identity::now() / 30 * 30;
    let before = gono_forum_web::content::cached_statistics(
        db,
        &services.cache.timed(LIFETIMES),
        start,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    let overview = gono_forum_web::content::cached_sidebar(
        db,
        &services.cache.timed(LIFETIMES),
        gono_forum_web::test_support::SEEDED_SIDEBAR,
    )
    .await
    .unwrap();
    let tid = overview.latest[0].id;
    let revision = gono_forum_web::content::cache_revision(db).await.unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_thread SET deleted_at=? WHERE tid=?")
        .bind(gono_forum_web::identity::now())
        .bind(tid as i64)
        .execute(db.executor())
        .await
        .unwrap();
    // The withdraw entry point increments the site-wide version in the same transaction; after raw SQL, sync with the
    // same code.
    gono_forum_web::test_support::resync_thread(db, tid as i64)
        .await
        .unwrap();
    assert!(gono_forum_web::content::cache_revision(db).await.unwrap() > revision);
    let cached = gono_forum_web::content::cached_statistics(
        db,
        &services.cache.timed(LIFETIMES),
        start + 5,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    assert_eq!(
        cached.total, before.total,
        "within one 30-second segment a new content revision does not rerun the statistics"
    );
    let next = gono_forum_web::content::cached_statistics(
        db,
        &services.cache.timed(LIFETIMES),
        start + 30,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    assert_eq!(
        next.total,
        before.total - 1,
        "the next segment counts again"
    );
    let overview = gono_forum_web::content::cached_sidebar(
        db,
        &services.cache.timed(LIFETIMES),
        gono_forum_web::test_support::SEEDED_SIDEBAR,
    )
    .await
    .unwrap();
    let card = overview.latest.iter().find(|t| t.id == tid).unwrap();
    assert_eq!(
        card.title, "Thread withdrawn",
        "the thread card follows the content revision at once"
    );
    services.cleanup().await.unwrap();
}

/// Cached values contain formatted times, so changing the time zone or date style must switch to a
/// new cache copy.
#[tokio::test]
async fn cached_lists_and_replies_follow_the_site_time_format() {
    use gono_forum_web::clock::{self, DateStyle, TimeFormat};
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    gono_forum_web::content::seed_forums(db).await.unwrap();
    gono_forum_web::content::seed_fixture(db).await.unwrap();
    let formats = [
        TimeFormat {
            lang: gono_forum_web::i18n::Lang::EN,
            offset_minutes: 0,
            date: DateStyle::YmdDash,
            clock24: true,
            relative_hours: None,
        },
        TimeFormat {
            lang: gono_forum_web::i18n::Lang::EN,
            offset_minutes: 600,
            date: DateStyle::MdySlash,
            clock24: false,
            relative_hours: None,
        },
    ];
    let mut lists = Vec::new();
    let mut replies = Vec::new();
    for format in formats {
        let cache = services.cache.clone();
        let (list, posts) = clock::scope(async move {
            clock::set(format);
            let list = gono_forum_web::content::cached_list_threads(
                db,
                "",
                gono_forum_web::urls::Sort::Latest,
                None,
                None,
                &cache.timed(LIFETIMES),
                1,
                20,
            )
            .await
            .unwrap();
            let mut thread = gono_forum_web::content::thread(db, 103)
                .await
                .unwrap()
                .unwrap();
            gono_forum_web::content::cached_replies(
                db,
                &cache.timed(LIFETIMES),
                &mut thread,
                1,
                false,
                None,
                (1, gono_forum_web::test_support::SEEDED_PAGING),
            )
            .await
            .unwrap();
            (list, thread.posts)
        })
        .await;
        let first = &list.threads[0];
        assert_eq!(
            first.time,
            clock::short_datetime(first.updated as i64, &format),
            "list times follow the current format"
        );
        lists.push(first.time.clone());
        replies.push(posts[0].time.clone());
    }
    assert_ne!(lists[0], lists[1]);
    assert_ne!(
        replies[0], replies[1],
        "floor times follow the current format too"
    );
    services.cleanup().await.unwrap();
}

/// Floor and sidebar times are recomputed by this request's definition after being read from the cache
/// (docs/settings-and-caching.md, "The Redis cache"): the copy in the cached value was formatted when loaded and goes
/// stale with relative times on, lagging by up to one cache TTL. A direct database change does not change the key, so
/// the second read must hit the cache while the times still move.
#[tokio::test]
async fn cached_replies_and_sidebar_recompute_relative_times_on_read() {
    use gono_forum_web::clock::{self, DateStyle, TimeFormat};
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    gono_forum_web::content::seed_forums(db).await.unwrap();
    gono_forum_web::content::seed_fixture(db).await.unwrap();
    let format = TimeFormat {
        lang: gono_forum_web::i18n::Lang::EN,
        offset_minutes: 480,
        date: DateStyle::YmdDash,
        clock24: true,
        relative_hours: Some(24),
    };
    let posted = gono_forum_web::identity::now() - 55;
    for sql in [
        "UPDATE gonof_forum_post SET dateline=? WHERE tid=103",
        "UPDATE gonof_forum_thread SET dateline=?, lastpost=? WHERE tid=103",
    ] {
        let mut query = gono_forum_web::db::query(sql).bind(posted);
        if sql.contains("lastpost") {
            query = query.bind(posted);
        }
        query.execute(db.executor()).await.unwrap();
    }
    let read = || {
        let cache = services.cache.clone();
        clock::scope(async move {
            clock::set(format);
            let mut thread = gono_forum_web::content::thread(db, 103)
                .await
                .unwrap()
                .unwrap();
            gono_forum_web::content::cached_replies(
                db,
                &cache.timed(LIFETIMES),
                &mut thread,
                1,
                false,
                None,
                (1, gono_forum_web::test_support::SEEDED_PAGING),
            )
            .await
            .unwrap();
            let sidebar = gono_forum_web::content::cached_sidebar(
                db,
                &cache.timed(LIFETIMES),
                gono_forum_web::test_support::SEEDED_SIDEBAR,
            )
            .await
            .unwrap();
            let latest = sidebar
                .latest
                .into_iter()
                .find(|thread| thread.id == 103)
                .expect("the thread just made the latest is in the sidebar latest list");
            (thread.posts, latest)
        })
    };
    let (posts, latest) = read().await;
    let first = posts[0].clone();
    if gono_forum_web::identity::now() - posted < 60 {
        assert_eq!(first.time, "just now");
        assert_eq!(latest.time, "just now");
    }
    // Changing the database does not change the key: the next read is still the cached floor (with the old body),
    // proving it came from the cache.
    gono_forum_web::db::query(
        "UPDATE gonof_forum_post SET message='Body changed in the database' WHERE pid=?",
    )
    .bind(first.pid as i64)
    .execute(db.executor())
    .await
    .unwrap();
    while gono_forum_web::identity::now() - posted < 61 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let (posts, latest) = read().await;
    assert_eq!(
        posts[0].body, first.body,
        "the second read hits the floor cache"
    );
    assert_eq!(
        posts[0].time, "1 minute ago",
        "floor times are computed for this request"
    );
    assert_eq!(
        latest.time, "1 minute ago",
        "sidebar times are computed for this request"
    );
    assert_eq!(latest.created_time, "1 minute ago");
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn bypassing_redis_skips_content_revision_queries() {
    use gono_forum_web::{content, debug::trace, urls::Sort};
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    content::seed_fixture(&f.db).await.unwrap();
    let cache = Cache::new(&RedisConfig {
        enabled: false,
        ..f.config.redis.clone()
    })
    .unwrap()
    .timed(LIFETIMES);
    let forums = content::anonymous_forums(&f.db).await.unwrap();
    let profile = Arc::new(trace::RequestProfile::new());
    trace::scope(profile.clone(), async {
        content::cached_list_threads(&f.db, "", Sort::Latest, None, None, &cache, 1, 20)
            .await
            .unwrap();
        content::feeds::cached_items(&f.db, &cache, &content::ListScope::All, &forums, 10)
            .await
            .unwrap();
        let mut thread = content::thread(&f.db, 103).await.unwrap().unwrap();
        content::cached_replies(
            &f.db,
            &cache,
            &mut thread,
            1,
            false,
            None,
            (1, gono_forum_web::test_support::SEEDED_PAGING),
        )
        .await
        .unwrap();
    })
    .await;
    let queries = profile.snapshot();
    assert!(!queries.is_empty());
    assert!(
        !queries
            .iter()
            .any(|q| q.sql.contains("gonof_common_content_revision"))
    );
}

/// Anonymous hot threads of the `threads` data source are keyed by the window length, not by its start: requests in
/// adjacent seconds of one list-lifetime segment share one copy instead of ranking again.
#[tokio::test]
async fn hot_data_source_copies_serve_the_whole_segment() {
    use gono_forum_web::content::{
        self, ListScope, ListViewer,
        sources::{SourceList, SourceSort},
    };
    let services = TestFixture::migrated().await.unwrap();
    if !services.cache.enabled() {
        return services.cleanup().await.unwrap();
    }
    let db = &services.db;
    content::seed_fixture(db).await.unwrap();
    // The first second of a 60-second list segment, so the next second is in the same segment.
    let start = gono_forum_web::identity::now() / 60 * 60;
    gono_forum_web::db::query(
        "UPDATE gonof_forum_thread SET lastpost=? WHERE replies>0 AND visibility='public'",
    )
    .bind(start - 3600)
    .execute(db.executor())
    .await
    .unwrap();
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    let forums = content::anonymous_forums(db).await.unwrap();
    let cache = services.cache.timed(LIFETIMES);
    let mut statements = Vec::new();
    let mut copies = Vec::new();
    for now in [start, start + 1, start + 59] {
        let list = SourceList {
            scope: &ListScope::All,
            sort: SourceSort::Hot { days: 30 },
            category: None,
            limit: 10,
            thumbnails: false,
        };
        let viewer = ListViewer {
            forums: &forums,
            ..ListViewer::default()
        };
        let counts = Arc::new(gono_forum_web::metrics::RequestMetrics::new());
        let threads = gono_forum_web::metrics::scope(
            counts.clone(),
            content::sources::threads(db, &cache, list, viewer, now),
        )
        .await
        .unwrap();
        statements.push(counts.sql());
        copies.push(threads.iter().map(|thread| thread.id).collect::<Vec<_>>());
    }
    assert!(!copies[0].is_empty(), "the fixture has hot threads");
    assert!(copies.iter().all(|copy| *copy == copies[0]), "{copies:?}");
    // The first request reads the list version and ranks; the others only read the list version.
    assert!(statements[0] >= 2, "{statements:?}");
    assert_eq!(&statements[1..], [1, 1], "{statements:?}");
    services.cleanup().await.unwrap();
}

/// This week's contributors count from Monday 00:00 in the site time zone, also when one statistics segment straddles
/// it: on a UTC+8 site with day-long segments, the minute after local Monday midnight is in the same UTC day as the
/// minute before it, yet it must not keep last week's figure. The Redis copy is new at once; the process copy is read
/// ahead, so the first request of the week may still get last week's figure while the new one is read.
#[tokio::test]
async fn weekly_contributors_start_again_at_local_monday_midnight() {
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    gono_forum_web::content::seed_fixture(db).await.unwrap();
    const OFFSET: i64 = 480;
    let day = gono_forum_web::cache::Lifetimes {
        statistics: 86_400,
        ..LIFETIMES
    };
    // A local Monday 00:00 three weeks back, where the fixture has no posts of its own.
    let monday =
        gono_forum_web::clock::week_start(gono_forum_web::identity::now(), OFFSET) - 21 * 86_400;
    let (before, after) = (monday - 30, monday + 30);
    assert_eq!(
        before.div_euclid(86_400),
        after.div_euclid(86_400),
        "both minutes are in one day-long segment"
    );
    // One visible public reply posted in the last minute of the Sunday before.
    let pid: i64 = gono_forum_web::db::query_scalar(
        "SELECT p.pid FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid
         WHERE p.first=0 AND p.invisible=0 AND p.deleted_at=0 AND p.root_invisible=0 AND p.authorid IS NOT NULL
           AND t.visibility='public' AND t.displayorder>=0 ORDER BY p.pid LIMIT 1",
    )
    .fetch_one(db.executor())
    .await
    .unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_post SET dateline=? WHERE pid=?")
        .bind(monday - 60)
        .bind(pid)
        .execute(db.executor())
        .await
        .unwrap();
    let fresh = |at: i64| async move {
        gono_forum_web::content::statistics_at(db, at as u64, OFFSET)
            .await
            .unwrap()
            .contributors
    };
    let expected = [(before, fresh(before).await), (after, fresh(after).await)];
    assert!(
        expected[0].1 > expected[1].1,
        "the reply counts in the week before only: {expected:?}"
    );
    let cache = services.cache.timed(day);
    for (at, expected) in expected {
        let redis = gono_forum_web::content::cached_statistics(db, &cache, at, OFFSET)
            .await
            .unwrap();
        assert_eq!(redis.contributors, expected, "Redis copy at {at}");
        let mut seen = Vec::new();
        for _ in 0..50 {
            let process = gono_forum_web::content::layered_statistics(
                db,
                &services.global,
                &cache,
                at,
                OFFSET,
            )
            .await
            .unwrap();
            seen.push(process.contributors);
            if process.contributors == expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            seen.last(),
            Some(&expected),
            "process copy at {at}: {seen:?}"
        );
    }
    services.cleanup().await.unwrap();
}

/// Site statistics are read ahead (docs/settings-and-caching.md, "The process cache"): once the process has figures,
/// the first request of a new segment gets them without running the aggregates, and a background task reads the new
/// segment, whose figures the following requests get.
#[tokio::test]
async fn a_new_statistics_segment_is_read_in_the_background() {
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    gono_forum_web::content::seed_fixture(db).await.unwrap();
    let cache = services.cache.timed(LIFETIMES);
    let offset = gono_forum_web::test_support::SEEDED_OFFSET_MINUTES;
    let start = gono_forum_web::identity::now() / 30 * 30;
    let read = |at: i64| {
        let cache = cache.clone();
        let global = services.global.clone();
        async move {
            let counts = Arc::new(gono_forum_web::metrics::RequestMetrics::new());
            let statistics = gono_forum_web::metrics::scope(
                counts.clone(),
                gono_forum_web::content::layered_statistics(db, &global, &cache, at, offset),
            )
            .await
            .unwrap();
            (statistics.total, counts.sql())
        }
    };
    let (before, cold) = read(start).await;
    assert!(cold > 0, "the first figures are read by the request");
    // One listed public thread fewer, from the next segment on.
    let tid: i64 = gono_forum_web::db::query_scalar(
        "SELECT MAX(tid) FROM gonof_forum_thread WHERE deleted_at=0 AND visibility='public' AND displayorder>=0",
    )
    .fetch_one(db.executor())
    .await
    .unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_thread SET deleted_at=? WHERE tid=?")
        .bind(start)
        .bind(tid)
        .execute(db.executor())
        .await
        .unwrap();
    let fresh = gono_forum_web::content::statistics_at(db, (start + 30) as u64, offset)
        .await
        .unwrap()
        .total;
    assert_eq!(fresh + 1, before, "the change shows in a recount");
    let (stale, statements) = read(start + 30).await;
    assert_eq!(
        (stale, statements),
        (before, 0),
        "the new segment's first request gets the previous figures and reads nothing"
    );
    let mut seen = Vec::new();
    for _ in 0..50 {
        let (total, _) = read(start + 31).await;
        seen.push(total);
        if total == fresh {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(seen.last(), Some(&fresh), "{seen:?}");
    assert_eq!(read(start + 32).await, (fresh, 0), "then a hit");
    services.cleanup().await.unwrap();
}

/// A domain scope's first figures are read once however many requests ask at the same time: the others wait for the
/// result instead of running the aggregates too.
#[tokio::test]
async fn concurrent_first_loads_of_a_scope_are_coalesced() {
    let services = TestFixture::new().await.unwrap();
    let fills = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let global = services.global.clone();
        let fills = fills.clone();
        tasks.push(tokio::spawn(async move {
            global
                .statistics(&[7, 9], (1, 2), move || async move {
                    fills.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(gono_forum_web::content::Statistics {
                        total: 3,
                        replies: 4,
                        contributors: 5,
                    })
                })
                .await
                .unwrap()
                .total
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), 3);
    }
    assert_eq!(fills.load(Ordering::SeqCst), 1);
    services.cleanup().await.unwrap();
}
