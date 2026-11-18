//! Rate limiting with Redis enabled: it uses a sliding log, key `{namespace}:ratelimit:{purpose}:{key}`, with one
//! count shared by multiple processes; when Redis is unavailable it is 503 — no letting requests through and no
//! fallback to in-process counting. The in-process counting of the Redis-disabled shape is covered by the
//! `rate_limit` unit tests.
//!
//! The first five tests read and write the fixture's Redis namespace directly and depend on Redis by design: they fail
//! under a test config with `[redis] enabled = false`.
use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    cache::{Cache, Unavailable},
    config::RedisConfig,
    metrics::{self, RequestMetrics},
    rate_limit::{Limits, Rule},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Looks at keys in Redis directly: over a separate connection, bypassing the app and not counted in any request.
struct Probe {
    connection: redis::aio::MultiplexedConnection,
    namespace: String,
}

impl Probe {
    async fn new(services: &TestFixture) -> Self {
        let client = redis::Client::open(services.config.redis.url.as_str()).unwrap();
        Self {
            connection: client.get_multiplexed_async_connection().await.unwrap(),
            namespace: services.config.redis.namespace.clone(),
        }
    }

    fn key(&self, scope: &str, key: &str) -> String {
        format!("{}:ratelimit:{scope}:{key}", self.namespace)
    }

    async fn entries(&self, scope: &str, key: &str) -> i64 {
        redis::cmd("ZCARD")
            .arg(self.key(scope, key))
            .query_async(&mut self.connection.clone())
            .await
            .unwrap()
    }

    async fn ttl_milliseconds(&self, scope: &str, key: &str) -> i64 {
        redis::cmd("PTTL")
            .arg(self.key(scope, key))
            .query_async(&mut self.connection.clone())
            .await
            .unwrap()
    }
}

async fn allow(limits: &Limits, rules: &[Rule]) -> bool {
    limits.allow(rules).await.unwrap()
}

#[tokio::test]
async fn redis_counts_only_allowed_hits_under_the_namespace() {
    let services = TestFixture::new().await.unwrap();
    assert!(
        services.cache.enabled(),
        "this case needs Redis enabled in the test configuration"
    );
    let probe = Probe::new(&services).await;
    let limits = Limits::new(services.cache.clone());
    let rule = || Rule::new("login-ip", "192.0.2.10", 3, 300);

    for _ in 0..3 {
        assert!(allow(&limits, &[rule()]).await);
    }
    // Rejections are not recorded in the window: continued requests do not keep pushing the block further out.
    for _ in 0..3 {
        assert!(!allow(&limits, &[rule()]).await);
    }
    assert_eq!(probe.entries("login-ip", "192.0.2.10").await, 3);
    // The key's expiry equals the window.
    let ttl = probe.ttl_milliseconds("login-ip", "192.0.2.10").await;
    assert!(
        (290_000..=300_000).contains(&ttl),
        "the expiry equals the window: {ttl}"
    );

    // Peek without counting: true when full, false when never recorded, and no key is created.
    assert!(limits.blocked(&[rule()]).await.unwrap());
    let fresh = Rule::new("login-ip", "192.0.2.11", 3, 300);
    assert!(!limits.blocked(&[fresh]).await.unwrap());
    assert_eq!(probe.ttl_milliseconds("login-ip", "192.0.2.11").await, -2);

    // Purposes and keys are independent of each other.
    assert!(allow(&limits, &[Rule::new("login-name", "192.0.2.10", 3, 300)]).await);
    assert!(allow(&limits, &[Rule::new("login-ip", "192.0.2.12", 3, 300)]).await);
    services.cleanup().await.unwrap();
}

/// Sliding window: once the earliest hit slides out of the window, one more is allowed, unlike a fixed window that
/// resets entirely at a fixed time.
#[tokio::test]
async fn redis_window_slides_instead_of_resetting() {
    let services = TestFixture::new().await.unwrap();
    let limits = Limits::new(services.cache.clone());
    let rule = || Rule::new("resend", 42, 2, 1);

    assert!(allow(&limits, &[rule()]).await);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(allow(&limits, &[rule()]).await);
    assert!(!allow(&limits, &[rule()]).await);
    // The first hit has slid out of the 1-second window and the second is still in: only one slot frees up. A fixed
    // window would allow two here.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(allow(&limits, &[rule()]).await);
    assert!(!allow(&limits, &[rule()]).await);
    services.cleanup().await.unwrap();
}

/// Multiple buckets short-circuit in order (same as in process): earlier ones allow and count, later ones only reject
/// when full, and counts already recorded by earlier ones are not withdrawn;
/// when the first is already full, later ones are neither checked nor counted.
#[tokio::test]
async fn redis_rules_short_circuit_in_order() {
    let services = TestFixture::new().await.unwrap();
    let probe = Probe::new(&services).await;
    let limits = Limits::new(services.cache.clone());

    let sender = || Rule::new("message-sender", 7, 5, 60);
    let peer = || Rule::new("message-peer", "7:8", 1, 60);
    assert!(allow(&limits, &[sender(), peer()]).await);
    assert!(!allow(&limits, &[sender(), peer()]).await);
    assert_eq!(probe.entries("message-sender", "7").await, 2);
    assert_eq!(probe.entries("message-peer", "7:8").await, 1);

    let ip = || Rule::new("forgot-ip", "192.0.2.20", 1, 3600);
    let email = || Rule::new("forgot-email-short", "digest", 5, 60);
    assert!(allow(&limits, &[ip(), email()]).await);
    assert!(!allow(&limits, &[ip(), email()]).await);
    assert_eq!(probe.entries("forgot-ip", "192.0.2.20").await, 1);
    assert_eq!(probe.entries("forgot-email-short", "digest").await, 1);

    services.cleanup().await.unwrap();
}

/// Slots in two buckets must be reserved together. Success only returns this call's slots; aborted placeholders expire
/// with the window, and a late return across windows must not delete a new placeholder. Both modes, single process and
/// Redis across connections, are verified separately.
#[tokio::test]
async fn failure_reservations_keep_concurrent_requests_within_the_budget() {
    let services = TestFixture::new().await.unwrap();
    let local = Limits::new(
        Cache::new(&RedisConfig {
            enabled: false,
            ..Default::default()
        })
        .unwrap(),
    );
    let first = Limits::new(services.cache.clone());
    let second = Limits::new(Cache::connect(&services.config.redis).await.unwrap());
    let mut pairs = vec![(&local, &local)];
    if services.cache.enabled() {
        pairs.push((&first, &second));
    }
    for (left, right) in pairs {
        let account = || Rule::new("password-change-account", 7, 1, 300);
        let other_account = || Rule::new("password-change-account", 8, 1, 300);
        let ip = || Rule::new("password-change-ip", "192.0.2.30", 1, 300);
        let request = left
            .reserve_failure(vec![account(), ip()])
            .await
            .unwrap()
            .unwrap();
        assert!(
            right
                .reserve_failure(vec![other_account(), ip()])
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !right.blocked(&[other_account()]).await.unwrap(),
            "when the later bucket is full, the earlier one is not charged either"
        );
        request.release().await.unwrap();
        assert!(!right.blocked(&[account(), ip()]).await.unwrap());
        // A 1-second short window backstops cancelled requests; a late return can only affect the original window.
        let short = || Rule::new("api-auth-failure", "192.0.2.31", 2, 1);
        let late = left.reserve_failure(vec![short()]).await.unwrap().unwrap();
        drop(right.reserve_failure(vec![short()]).await.unwrap().unwrap());
        assert!(right.blocked(&[short()]).await.unwrap());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        drop(right.reserve_failure(vec![short()]).await.unwrap().unwrap());
        drop(left.reserve_failure(vec![short()]).await.unwrap().unwrap());
        late.release().await.unwrap();
        assert!(right.blocked(&[short()]).await.unwrap());
    }
    services.cleanup().await.unwrap();
}

/// Two processes (each with its own Redis connection, same namespace) share one count, so the threshold does not
/// scale with the number of processes.
#[tokio::test]
async fn redis_counts_are_shared_across_processes() {
    let services = TestFixture::new().await.unwrap();
    let first = Limits::new(services.cache.clone());
    let second = Limits::new(Cache::connect(&services.config.redis).await.unwrap());
    let rule = || Rule::new("reset-token", "token-digest", 3, 300);

    assert!(allow(&first, &[rule()]).await);
    assert!(allow(&second, &[rule()]).await);
    assert!(allow(&first, &[rule()]).await);
    assert!(!allow(&second, &[rule()]).await);
    assert!(!allow(&first, &[rule()]).await);
    services.cleanup().await.unwrap();
}

/// However many buckets, it is one script and one round trip, counted in the footer's Redis count.
#[tokio::test]
async fn redis_rules_cost_one_round_trip_per_call() {
    let services = TestFixture::new().await.unwrap();
    let limits = Limits::new(services.cache.clone());
    let counted = Arc::new(RequestMetrics::new());
    metrics::scope(counted.clone(), async {
        let rules = [
            Rule::new("forgot-ip", "192.0.2.40", 20, 3600),
            Rule::new("forgot-email-short", "digest", 1, 60),
            Rule::new("forgot-email-long", "digest", 5, 3600),
        ];
        assert!(limits.allow(&rules).await.unwrap());
        assert!(!limits.allow(&rules).await.unwrap());
        assert!(limits.blocked(&rules).await.unwrap());
    })
    .await;
    assert_eq!(counted.redis(), 3);
    assert_eq!(counted.sql(), 0);
    services.cleanup().await.unwrap();
}

/// Redis enabled but unavailable: the rate limit call fails with the [`Unavailable`] marker; both web pages and the
/// member API give 503, without letting requests through or falling back to in-process counting.
#[tokio::test]
async fn unavailable_redis_rejects_rate_limited_requests_with_503() {
    let services = TestFixture::migrated().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    let broken = Cache::new(&RedisConfig {
        url: format!("redis://{endpoint}/0"),
        namespace: services.config.redis.namespace.clone(),
        timeout_milliseconds: 100,
        ..RedisConfig::default()
    })
    .unwrap();
    assert!(broken.enabled());

    let limits = Limits::new(broken.clone());
    let rule = || Rule::new("login-ip", "192.0.2.50", 30, 300);
    for error in [
        limits.allow(&[rule()]).await.unwrap_err(),
        limits.blocked(&[rule()]).await.unwrap_err(),
        limits
            .reserve_failure(vec![rule()])
            .await
            .err()
            .expect("Redis is unavailable"),
    ] {
        assert!(error.downcast_ref::<Unavailable>().is_some(), "{error:?}");
    }

    let app = router::build_with_cache(
        services.config.clone(),
        &services.root,
        services.db.clone(),
        broken,
    );
    let origin = gono_forum_web::test_support::TEST_SITE_URL;
    let host = gono_forum_web::test_support::host_of(origin);
    let send = |request: Request<Body>| {
        let app = app.clone();
        async move {
            let mut request = request;
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "127.0.0.1:41000".parse::<std::net::SocketAddr>().unwrap(),
            ));
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, headers, String::from_utf8_lossy(&body).into_owned())
        }
    };

    // The login page itself does not touch Redis; submission goes through rate limiting first, and with Redis down it
    // is 503 without entering authentication.
    let (status, headers, _) = send(
        Request::builder()
            .uri("/login")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let csrf = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie| {
            cookie
                .split(';')
                .next()
                .and_then(|pair| pair.strip_prefix("gonof_csrf__port58833="))
                .map(str::to_owned)
        })
        .expect("the login page sets the CSRF cookie");
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("username", "nobody"),
            ("password", "Not-the-password-2026!"),
            ("_csrf", csrf.as_str()),
        ])
        .finish();
    let (status, headers, body) = send(
        Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::HOST, host)
            .header(header::ORIGIN, origin)
            .header(header::COOKIE, format!("gonof_csrf__port58833={csrf}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(headers[header::RETRY_AFTER], "3");
    assert!(
        !body.contains(gono_forum_web::i18n::Lang::EN.account_error_login_failed()),
        "{body}"
    );

    // Member API: the per-IP failure quota is checked before authentication, likewise 503, with a JSON error body.
    let (status, headers, body) = send(
        Request::builder()
            .uri("/api/v1/me")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, "Bearer not-a-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(headers[header::RETRY_AFTER], "3");
    assert!(body.contains("\"code\":\"unavailable\""), "{body}");
    services.cleanup().await.unwrap();
}
