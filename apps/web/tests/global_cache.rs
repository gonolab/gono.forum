//! In-process global cache and cross-process invalidation (docs/settings-and-caching.md, "The process cache" and
//! "Invalidation").
//!
//! Multiple processes are simulated by two apps in the same test process: each has its own `GlobalCache` and they
//! share the fixture's Redis namespace.
#[path = "support/http_auth.rs"]
mod http_auth;
#[path = "support/mod.rs"]
mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    cache::{Block, Cache, GlobalCache},
    config::RedisConfig,
    db,
    debug::trace::{self, BlockOutcome, QuerySource, RequestProfile},
    identity::{self, Actor, IdentityError},
    router, runtime,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: Actor = Actor {
    uid: 9851,
    credentials_version: 1,
    api_key: None,
};

async fn fixture() -> TestFixture {
    let f = TestFixture::migrated().await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,'cache-admin','synthetic-hash','cache-admin@example.test',1,1,1)")
        .bind(ADMIN.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    f
}

fn app(f: &TestFixture, cache: Cache, global: GlobalCache) -> Router {
    router::build_with_global(
        support::config_for(&f.db),
        &gono_forum_web::project_root(),
        f.db.clone(),
        cache,
        global,
    )
}

async fn body(app: &Router, path: &str) -> (StatusCode, String) {
    let (status, _, body) = support::fetch(app, path, false).await;
    (status, body)
}

async fn rename(f: &TestFixture, global: &GlobalCache, name: &str) {
    runtime::update(&f.db, global, ADMIN, "site.settings.save", |s| {
        s.site.name = name.into();
    })
    .await
    .unwrap();
}

/// How many SQL statements one read of the settings block issued, and what the block record is.
async fn profiled_settings(f: &TestFixture, global: &GlobalCache) -> (usize, BlockOutcome) {
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), global.settings(&f.db))
        .await
        .unwrap();
    let sql = profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .count();
    let blocks = profile.block_snapshot();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].block, "settings");
    (sql, blocks[0].outcome)
}

async fn set_raw(f: &TestFixture, key: &str, value: &str) {
    db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
        .bind(value)
        .bind(key)
        .execute(f.db.executor())
        .await
        .unwrap();
}

/// Within the TTL a hit does not query the database; with the TTL set to 0 every read goes to the database and
/// nothing stays in memory (used when profiling SQL).
#[tokio::test]
async fn settings_are_cached_until_the_ttl_and_zero_turns_caching_off() {
    let f = fixture().await;
    let global = &f.global;
    assert_eq!(
        profiled_settings(&f, global).await,
        (1, BlockOutcome::Filled)
    );
    assert_eq!(profiled_settings(&f, global).await, (0, BlockOutcome::Hit));
    // A direct database change (not through a save function) is not visible within the TTL — proving it really is
    // cached; after clearing this process it is visible immediately.
    set_raw(&f, "settings.site.name", "Edited in the database").await;
    assert_eq!(
        global.settings(&f.db).await.unwrap().settings.site.name,
        "Sample Community"
    );
    global.clear_local();
    assert_eq!(
        global.settings(&f.db).await.unwrap().settings.site.name,
        "Edited in the database"
    );

    runtime::update(&f.db, global, ADMIN, "site.settings.save", |s| {
        s.cache.ttl_seconds = 0;
    })
    .await
    .unwrap();
    for _ in 0..3 {
        assert_eq!(
            profiled_settings(&f, global).await,
            (1, BlockOutcome::Uncached)
        );
    }
    assert_eq!(global.ttl_seconds(), Some(0));
    // With TTL 0 the other blocks also read the database every time.
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), async {
        global.policy(&f.db, "policy.terms", 0).await.unwrap();
        global.policy(&f.db, "policy.terms", 0).await.unwrap();
    })
    .await;
    assert_eq!(profile.snapshot().len(), 2);
    assert!(
        profile
            .block_snapshot()
            .iter()
            .all(|block| block.outcome == BlockOutcome::Uncached)
    );
    // Set back to non-zero: the next read fills, the one after hits.
    runtime::update(&f.db, global, ADMIN, "site.settings.save", |s| {
        s.cache.ttl_seconds = 30;
    })
    .await
    .unwrap();
    assert_eq!(
        profiled_settings(&f, global).await,
        (1, BlockOutcome::Filled)
    );
    assert_eq!(profiled_settings(&f, global).await, (0, BlockOutcome::Hit));
    f.cleanup().await.unwrap();
}

/// Save functions use the same handle: after commit this process clears synchronously, so the next request to the
/// same app is sure to see the new value.
#[tokio::test]
async fn a_save_takes_effect_in_the_same_process_immediately() {
    let f = fixture().await;
    let app = app(&f, f.cache.clone(), f.global.clone());
    let (status, html) = body(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Sample Community"));
    rename(&f, &f.global, "Takes effect at once").await;
    let (_, html) = body(&app, "/").await;
    assert!(
        html.contains("Takes effect at once"),
        "after a save the next request in the same process has the new value"
    );
    // Policy documents: one slot per kind; only the saved slot is cleared.
    let (_, rules) = body(&app, "/rules").await;
    assert!(rules.contains("Community rules"));
    runtime::save_document(
        &f.db,
        &f.global,
        ADMIN,
        "policy.rules",
        runtime::Document {
            title: "New version of the rules".into(),
            body: "Rules body".into(),
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    let (_, rules) = body(&app, "/rules").await;
    assert!(rules.contains("New version of the rules"));
    // Check-in rules: the member card and the rules page read the cache and switch to the new value right after a save.
    let (_, page) = body(&app, "/credits/rules").await;
    let signin = gono_forum_web::activities::rules(&f.db)
        .await
        .unwrap()
        .into_iter()
        .find(|rule| rule.key == "signin")
        .unwrap();
    gono_forum_web::activities::update_rule(
        &f.db,
        &f.global,
        ADMIN,
        "signin",
        true,
        7,
        70,
        signin.version,
        identity::now(),
    )
    .await
    .unwrap();
    let (_, after) = body(&app, "/credits/rules").await;
    assert_ne!(page, after, "the rules page changes with the save");
    assert!(after.contains("70"), "{after}");
    f.cleanup().await.unwrap();
}

/// Cross-process: with Redis enabled a save is followed by `PUBLISH`; the other app's standing subscription clears
/// on receipt, long before the 30-second TTL.
#[tokio::test]
async fn another_process_drops_its_copy_when_a_save_is_published() {
    let f = fixture().await;
    assert!(f.cache.enabled(), "the full suite runs with Redis enabled");
    let first = GlobalCache::start(f.cache.clone()).await.unwrap();
    let second = GlobalCache::start(Cache::connect(&f.config.redis).await.unwrap())
        .await
        .unwrap();
    assert_eq!(first.channel(), second.channel());
    assert!(first.channel().ends_with(":global-cache"));
    let a = app(&f, f.cache.clone(), first.clone());
    let b = app(&f, f.cache.clone(), second.clone());
    // Both sides fill the settings block.
    assert!(body(&a, "/").await.1.contains("Sample Community"));
    assert!(body(&b, "/").await.1.contains("Sample Community"));
    rename(&f, &first, "Across processes").await;
    assert!(body(&a, "/").await.1.contains("Across processes"));
    let mut seen = false;
    for _ in 0..50 {
        if body(&b, "/").await.1.contains("Across processes") {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen,
        "the other process should see the new value when the broadcast arrives, not after the TTL"
    );
    // A broadcast only clears the named blocks: the friend-links block on the other side is unaffected by the settings
    // save and still correct (here we only check that it does not error).
    first.changed(&[Block::Links]).await.unwrap();
    f.cleanup().await.unwrap();
}

/// After the subscription disconnects it reconnects in the background, and on a successful reconnect it clears all
/// blocks in this process (notifications during the outage are already lost).
#[tokio::test]
async fn reconnecting_the_subscription_clears_every_block() {
    let f = fixture().await;
    let global = GlobalCache::start(f.cache.clone()).await.unwrap();
    assert_eq!(
        profiled_settings(&f, &global).await,
        (1, BlockOutcome::Filled)
    );
    assert_eq!(profiled_settings(&f, &global).await, (0, BlockOutcome::Hit));
    let before = global.drop_subscription_for_test();
    let mut reconnected = false;
    for _ in 0..100 {
        if global.reconnects() > before {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        reconnected,
        "the subscription should reconnect after backing off"
    );
    assert_eq!(
        profiled_settings(&f, &global).await,
        (1, BlockOutcome::Filled),
        "a reconnect clears every block, so the next read goes to the source"
    );
    // The reconnected subscription receives broadcasts as usual.
    let other = GlobalCache::new(f.cache.clone());
    rename(&f, &other, "After reconnecting").await;
    let mut seen = false;
    for _ in 0..50 {
        if global.settings(&f.db).await.unwrap().settings.site.name == "After reconnecting" {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen);
    f.cleanup().await.unwrap();
}

fn unreachable_redis(namespace: &str) -> Cache {
    // Nothing listens on port 1: the connection is refused immediately. Use a separate namespace and touch no keys on
    // the shared instance.
    let config = RedisConfig {
        enabled: true,
        url: "redis://127.0.0.1:1/0".into(),
        namespace: namespace.into(),
        timeout_milliseconds: 500,
    };
    Cache::new(&config).unwrap()
}

/// With Redis enabled the subscription connection is a startup prerequisite: if it cannot be established, startup
/// is refused (docs/settings-and-caching.md, "Invalidation"); with Redis disabled no subscription is created.
#[tokio::test]
async fn startup_refuses_to_run_without_the_subscription() {
    let error = GlobalCache::start(unreachable_redis("gonof_test_unreachable"))
        .await
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("subscribe") && text.contains("global-cache"),
        "{text}"
    );
    let disabled = Cache::new(&RedisConfig {
        enabled: false,
        url: String::new(),
        ..RedisConfig::default()
    })
    .unwrap();
    GlobalCache::start(disabled).await.unwrap();
}

/// Publishing fails after commit: the data is already saved and this process already cleared, so answer 503 and
/// state that it was saved and the staleness bound (docs/settings-and-caching.md, "Invalidation").
#[tokio::test]
async fn a_failed_publish_answers_503_but_keeps_the_save() {
    let f = fixture().await;
    let broken = GlobalCache::new(unreachable_redis(&f.config.redis.namespace));
    // Service layer: the handle has never read the settings and this change modifies the TTL, yet it can still report
    // the old value read under lock.
    set_raw(&f, "settings.cache.ttl_seconds", "75").await;
    let error = runtime::update(&f.db, &broken, ADMIN, "site.settings.save", |s| {
        s.site.name = "Already saved".into();
        s.cache.ttl_seconds = 30;
    })
    .await
    .unwrap_err();
    let IdentityError::Internal(inner) = error else {
        panic!("a failed publish should be an internal error");
    };
    let unpublished = gono_forum_web::cache::unpublished(&inner).expect("should carry Unpublished");
    assert_eq!(unpublished.ttl_seconds, Some(75));
    assert_eq!(
        runtime::read(&f.db).await.unwrap().site.name,
        "Already saved"
    );

    // Page: the administrator saves site settings and Redis is unavailable after commit.
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let session = http_auth::login(&identity, &f.db, ADMIN.uid).await;
    let app = app(&f, f.cache.clone(), broken.clone());
    // As in the normal admin flow, open the form first and then submit; the current TTL is already 30 seconds.
    let request = Request::builder()
        .uri("/manage/settings")
        .header(
            header::COOKIE,
            format!("gonof_session={}", session.session_token),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::OK
    );
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("_csrf", &session.csrf_token);
    form.append_pair("site_name", "Saved from the page");
    form.append_pair(
        "read_only_message",
        "The forum is in read-only mode for a while. Please come back soon.",
    );
    form.append_pair("clock24", "true");
    let request = Request::builder()
        .method("POST")
        .uri("/manage/settings")
        .header(
            header::COOKIE,
            format!("gonof_session={}", session.session_token),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.finish()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        html.contains("Saved, but the other processes could not be notified"),
        "{html}"
    );
    assert!(
        html.contains("The cache lifetime read before saving was 30 seconds"),
        "{html}"
    );
    assert_eq!(
        runtime::read(&f.db).await.unwrap().site.name,
        "Saved from the page"
    );
    f.cleanup().await.unwrap();
}

/// When the command-line rescue hits a broadcast failure after the transaction commits, it must tell the operator
/// clearly that the new URL has taken effect.
#[tokio::test]
async fn cli_reports_a_committed_site_url_when_its_first_publish_fails() {
    let f = fixture().await;
    set_raw(&f, "settings.cache.ttl_seconds", "75").await;
    let config = f.root.join("cli.toml");
    std::fs::write(&config, "[redis]\nenabled = true\n").unwrap();
    let site_url = "https://recovered.example/bbs";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gono-forum-cli"))
        .args(["set-site-url", site_url])
        .env("GONOF_CONFIG", &config)
        .env("GONOF_DATABASE_URL", &f.config.database.url)
        .env("GONOF_REDIS_URL", "redis://127.0.0.1:1/0")
        .env("GONOF_REDIS_NAMESPACE", &f.config.redis.namespace)
        .env("GONOF_AUTH_COOKIE_KEY", "11".repeat(32))
        .env("GONOF_TWO_FACTOR_KEY", "22".repeat(32))
        .env_remove("GONOF_AUTH_COOKIE_PREVIOUS_KEYS")
        .env_remove("GONOF_TWO_FACTOR_PREVIOUS_KEYS")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("Saved")
            && error.contains("The cache lifetime read before saving was 75 seconds"),
        "{error}"
    );
    assert!(
        error.contains("old caches may stay for up to 86,400 seconds"),
        "{error}"
    );
    assert!(!error.contains("failed to change"), "{error}");
    assert_eq!(runtime::read(&f.db).await.unwrap().site.url, site_url);
    f.cleanup().await.unwrap();
}

/// Redis disabled (single-process deployment): no subscription, no publishing; a save only clears this process,
/// still takes effect immediately, and Redis is never touched.
#[tokio::test]
async fn with_redis_disabled_saves_only_clear_this_process() {
    let f = fixture().await;
    let disabled = Cache::new(&RedisConfig {
        enabled: false,
        url: String::new(),
        ..RedisConfig::default()
    })
    .unwrap();
    let global = GlobalCache::start(disabled.clone()).await.unwrap();
    let mut config = support::config_for(&f.db);
    config.redis.enabled = false;
    config.redis.url = String::new();
    let app = router::build_with_global(
        config,
        &gono_forum_web::project_root(),
        f.db.clone(),
        disabled,
        global.clone(),
    );
    assert!(body(&app, "/").await.1.contains("Sample Community"));
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), rename(&f, &global, "Single process")).await;
    assert!(
        profile.redis_snapshot().is_empty(),
        "nothing is published with Redis off"
    );
    assert!(body(&app, "/").await.1.contains("Single process"));
    // A hit within the TTL: the home page no longer issues the settings read.
    let profile = Arc::new(RequestProfile::new());
    let (status, _) = trace::scope(profile.clone(), body(&app, "/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !profile
            .snapshot()
            .iter()
            .any(|record| record.sql.contains("skey>='settings.policy/'")),
        "on a hit the page frame does not read the settings again"
    );
    assert!(
        profile
            .block_snapshot()
            .iter()
            .any(|block| block.block == "settings" && block.outcome == BlockOutcome::Hit)
    );
    assert!(profile.redis_snapshot().is_empty());
    f.cleanup().await.unwrap();
}

/// Subscription heartbeat: a healthy subscription gets every beat back and does not reconnect;
/// when a beat does not come back (with a half-broken network the connection still looks alive) it is treated as a
/// disconnect: reconnect and clear all blocks without waiting for the TTL.
#[tokio::test]
async fn heartbeat_reconnects_a_subscription_that_stops_hearing_itself() {
    let f = fixture().await;
    let global = GlobalCache::start_with_heartbeat(f.cache.clone(), Duration::from_millis(200))
        .await
        .unwrap();
    assert_eq!(
        profiled_settings(&f, &global).await,
        (1, BlockOutcome::Filled)
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        global.reconnects(),
        0,
        "a healthy heartbeat does not reconnect"
    );
    assert_eq!(profiled_settings(&f, &global).await, (0, BlockOutcome::Hit));

    let before = global.mute_heartbeat_once_for_test();
    let mut reconnected = false;
    for _ in 0..100 {
        if global.reconnects() > before {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        reconnected,
        "a heartbeat that does not come back counts as a disconnect"
    );
    assert_eq!(
        profiled_settings(&f, &global).await,
        (1, BlockOutcome::Filled),
        "a reconnect clears every block"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        global.reconnects(),
        before + 1,
        "the heartbeat is healthy again after reconnecting"
    );
    f.cleanup().await.unwrap();
}

/// The admin "Refresh caches" button: this process clears immediately; with Redis enabled it
/// broadcasts `*` and other processes also clear everything on receipt.
#[tokio::test]
async fn refreshing_everything_clears_this_process_and_tells_the_others() {
    let f = fixture().await;
    let first = GlobalCache::start(f.cache.clone()).await.unwrap();
    let second = GlobalCache::start(f.cache.clone()).await.unwrap();
    for global in [&first, &second] {
        assert_eq!(
            profiled_settings(&f, global).await,
            (1, BlockOutcome::Filled)
        );
        assert_eq!(profiled_settings(&f, global).await, (0, BlockOutcome::Hit));
    }
    first.refresh_all().await.unwrap();
    assert_eq!(
        profiled_settings(&f, &first).await,
        (1, BlockOutcome::Filled),
        "this process clears at once"
    );
    if f.cache.enabled() {
        let mut cleared = false;
        for _ in 0..50 {
            if profiled_settings(&f, &second).await.1 == BlockOutcome::Filled {
                cleared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            cleared,
            "other processes clear everything when the broadcast arrives"
        );
    } else {
        assert_eq!(
            profiled_settings(&f, &second).await,
            (0, BlockOutcome::Hit),
            "with Redis off there is one process and no broadcast"
        );
    }
    f.cleanup().await.unwrap();
}
