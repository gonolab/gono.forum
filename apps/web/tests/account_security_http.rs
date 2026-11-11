use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content,
    db::{self, Database},
    identity::{self, Identity, SessionGrant},
    router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const PASSWORD: &str = "Security-original-password-2026!";
const NEW_PASSWORD: &str = "Security-replacement-password-2026!";

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    root: PathBuf,
    db: Database,
    identity: Identity,
    app: Router,
    admin: SessionGrant,
    member: SessionGrant,
}

impl Fixture {
    /// `settings` are the site settings to change (key, value): the account security rate limits are the admin
    /// settings `settings.account.*`; they are changed directly in the database and the application reads them on
    /// the first request.
    async fn new(settings: &[(&str, &str)]) -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        for (key, value) in settings {
            gono_forum_web::test_support::set_setting(&db, key, value)
                .await
                .unwrap();
        }
        content::seed_forums(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let invite = identity
            .generate_invite(admin.member.uid, "security-member")
            .await
            .unwrap();
        let member = identity
            .register(
                "securitymember",
                "security@example.test",
                PASSWORD,
                &invite.code,
            )
            .await
            .unwrap();
        let app = router::build(config, &root, db.clone());
        Self {
            _services: services,
            root,
            db,
            identity,
            app,
            admin,
            member,
        }
    }

    async fn reset_token(&self) -> String {
        let row = gono_forum_web::db::query("SELECT payload FROM gonof_common_outbox WHERE event_type='password_reset' AND aggregate_id=? ORDER BY eventid DESC LIMIT 1").bind(self.member.member.uid).fetch_one(self.db.executor()).await.unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(&db::text(&row, "payload").unwrap()).unwrap();
        payload["token"].as_str().unwrap().into()
    }

    async fn scalar(&self, sql: &str) -> i64 {
        gono_forum_web::db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Default)]
struct Cookies(BTreeMap<String, String>);

impl Cookies {
    fn member(fixture: &Fixture) -> Self {
        Self::session(&fixture.member.session_token)
    }
    fn session(raw: &str) -> Self {
        Self(BTreeMap::from([("gonof_session".into(), raw.into())]))
    }
    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self
            .0
            .get("gonof_session")
            .or_else(|| self.0.get("__Host-gonof_session"))
        {
            identity
                .session(raw)
                .await
                .unwrap()
                .map(|session| session.csrf_token)
                .unwrap_or_default()
        } else {
            self.0
                .get("gonof_csrf")
                .or_else(|| self.0.get("__Host-gonof_csrf"))
                .cloned()
                .unwrap_or_default()
        }
    }
    fn update(&mut self, headers: &HeaderMap) {
        for cookie in headers.get_all(header::SET_COOKIE) {
            let (name, value) = cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap();
            if value.is_empty() {
                self.0.remove(name);
            } else {
                self.0.insert(name.into(), value.into());
            }
        }
    }
}

struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}
impl Response {
    fn status(&self, expected: StatusCode) {
        assert_eq!(self.status, expected);
        assert!(
            self.headers[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
    }
    fn redirect(&self, path: &str) {
        self.status(StatusCode::SEE_OTHER);
        assert_eq!(self.headers[header::LOCATION], path);
    }
    fn no_password(&self) {
        assert!(!self.html.contains(PASSWORD));
        assert!(!self.html.contains(NEW_PASSWORD));
    }
}

async fn request(
    fixture: &Fixture,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Response {
    request_from(
        fixture,
        "192.0.2.33:48321",
        method,
        path,
        cookies,
        fields,
        extra,
    )
    .await
}

/// Same as [`request`], but with a given peer address, to simulate different client IPs.
async fn request_from(
    fixture: &Fixture,
    peer: &str,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Response {
    let mut builder = Request::builder().method(method).uri(path).header(
        header::COOKIE,
        cookies
            .0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    );
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")) {
            builder = builder
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
        }
    }
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut req = builder.body(Body::from(form)).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let response = fixture.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    cookies.update(&headers);
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
    Response {
        status,
        headers,
        html,
    }
}
async fn get(fixture: &Fixture, path: &str, cookies: &mut Cookies) -> Response {
    request(fixture, "GET", path, cookies, &[], &[]).await
}
async fn post(
    fixture: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Response {
    let csrf = cookies.csrf(&fixture.identity).await;
    let mut fields = fields.to_vec();
    fields.push(("_csrf", &csrf));
    request(fixture, "POST", path, cookies, &fields, &[]).await
}

#[tokio::test]
async fn changing_password_requires_current_password_and_signs_out_every_device() {
    let fixture = Fixture::new(&[]).await;
    let second = fixture
        .identity
        .login("securitymember", PASSWORD)
        .await
        .unwrap();
    let mut cookies = Cookies::member(&fixture);
    let page = get(&fixture, "/my/account", &mut cookies).await;
    page.status(StatusCode::OK); // Members can protect their account even without a verified e-mail.
    assert!(page.html.contains("Change password and log out"));
    assert!(page.html.contains("hx-boost=\"false\""));
    let fields = [
        ("current_password", "Wrong-current-password"),
        ("password", NEW_PASSWORD),
        ("password2", NEW_PASSWORD),
    ];
    let wrong = post(&fixture, "/my/account/password", &mut cookies, &fields).await;
    wrong.status(StatusCode::UNPROCESSABLE_ENTITY);
    wrong.no_password();
    assert!(wrong.html.contains("The current password is wrong"));
    assert!(
        fixture
            .identity
            .session(&second.session_token)
            .await
            .unwrap()
            .is_some()
    );
    let mismatch = post(
        &fixture,
        "/my/account/password",
        &mut cookies,
        &[
            ("current_password", PASSWORD),
            ("password", NEW_PASSWORD),
            ("password2", "different-password"),
        ],
    )
    .await;
    mismatch.status(StatusCode::UNPROCESSABLE_ENTITY);
    mismatch.no_password();
    let changed = post(
        &fixture,
        "/my/account/password",
        &mut cookies,
        &[
            ("current_password", PASSWORD),
            ("password", NEW_PASSWORD),
            ("password2", NEW_PASSWORD),
        ],
    )
    .await;
    changed.redirect("/login?password_changed=true");
    assert!(!cookies.0.contains_key("gonof_session"));
    assert!(!cookies.0.contains_key("gonof_csrf"));
    for sid in [&fixture.member.session_token, &second.session_token] {
        assert!(fixture.identity.session(sid).await.unwrap().is_none());
    }
    assert!(
        fixture
            .identity
            .login("securitymember", PASSWORD)
            .await
            .is_err()
    );
    assert!(
        fixture
            .identity
            .login("securitymember", NEW_PASSWORD)
            .await
            .is_ok()
    );
    let login = get(&fixture, "/login?password_changed=true", &mut cookies).await;
    assert!(login.html.contains("role=\"status\""));
    assert!(
        login
            .html
            .contains("Your password has changed and all devices are logged out")
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn forgot_password_masks_account_eligibility_and_normalizes_throttle_keys() {
    let fixture = Fixture::new(&[]).await;
    let mut cookies = Cookies::default();
    get(&fixture, "/forgot-password", &mut cookies).await;
    for email in [
        "security@example.test",
        "unknown@example.test",
        "admin@example.test",
        "not-an-email",
    ] {
        let started = Instant::now();
        let response = post(
            &fixture,
            "/forgot-password",
            &mut cookies,
            &[("email", email)],
        )
        .await;
        response.redirect("/forgot-password?sent=true");
        assert!(started.elapsed() >= Duration::from_millis(240));
    }
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='password_reset'")
            .await,
        1
    );
    let token = fixture.reset_token().await;
    // Case, spaces and compatibility full-width characters must share the same e-mail rate limit bucket (the
    // escapes are the full-width "security@example.test").
    for email in [
        " SECURITY@EXAMPLE.TEST ",
        "\u{ff53}\u{ff45}\u{ff43}\u{ff55}\u{ff52}\u{ff49}\u{ff54}\u{ff59}\u{ff20}\u{ff45}\u{ff58}\u{ff41}\u{ff4d}\u{ff50}\u{ff4c}\u{ff45}\u{ff0e}\u{ff54}\u{ff45}\u{ff53}\u{ff54}",
    ] {
        post(
            &fixture,
            "/forgot-password",
            &mut cookies,
            &[("email", email)],
        )
        .await
        .status(StatusCode::TOO_MANY_REQUESTS);
        assert!(
            fixture
                .identity
                .password_reset_status(&token)
                .await
                .unwrap()
        );
    }
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='password_reset'")
            .await,
        1
    );
    gono_forum_web::db::query("UPDATE gonof_common_member SET freeze=1 WHERE uid=?")
        .bind(fixture.member.member.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    // The e-mail bucket is full; this part needs the frozen account after the window has passed: a new router
    // instance has an empty in-process count; with Redis enabled the two applications share the count in Redis, so
    // first clear this fixture's namespace keys.
    // The business database keeps the same frozen-account facts.
    fixture._services.cache.clear_namespace().await.unwrap();
    let config: Config = fixture._services.config.clone();
    let frozen_app = router::build(config, &fixture.root, fixture.db.clone());
    let mut builder = Request::builder()
        .method("POST")
        .uri("/forgot-password")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, ORIGIN)
        .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
    let csrf = cookies.csrf(&fixture.identity).await;
    builder = builder.header(header::COOKIE, format!("gonof_csrf={csrf}"));
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([("_csrf", csrf.as_str()), ("email", "security@example.test")])
        .finish();
    let response = frozen_app
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/forgot-password?sent=true"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='password_reset'")
            .await,
        1
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn reset_get_is_read_only_post_is_single_use_and_clears_cookies() {
    let fixture = Fixture::new(&[]).await;
    fixture
        .identity
        .request_password_reset("security@example.test")
        .await
        .unwrap();
    let token = fixture.reset_token().await;
    let mut cookies = Cookies::member(&fixture);
    for _ in 0..2 {
        let page = get(
            &fixture,
            &format!("/reset-password?token={token}"),
            &mut cookies,
        )
        .await;
        page.status(StatusCode::OK);
        assert_eq!(page.headers[header::REFERRER_POLICY], "strict-origin");
        assert!(
            page.html
                .contains(&format!("name=\"token\" value=\"{token}\""))
        );
        assert!(
            !page
                .html
                .contains(&format!("href=\"{ORIGIN}/reset-password?token="))
        );
        assert!(
            fixture
                .identity
                .password_reset_status(&token)
                .await
                .unwrap()
        );
    }
    let mismatch = post(
        &fixture,
        "/reset-password",
        &mut cookies,
        &[
            ("token", &token),
            ("password", NEW_PASSWORD),
            ("password2", PASSWORD),
        ],
    )
    .await;
    mismatch.status(StatusCode::UNPROCESSABLE_ENTITY);
    mismatch.no_password();
    assert!(
        fixture
            .identity
            .password_reset_status(&token)
            .await
            .unwrap()
    );
    post(
        &fixture,
        "/reset-password",
        &mut cookies,
        &[
            ("token", &token),
            ("password", NEW_PASSWORD),
            ("password2", NEW_PASSWORD),
        ],
    )
    .await
    .redirect("/login?password_reset=true");
    assert!(!cookies.0.contains_key("gonof_session"));
    assert!(!cookies.0.contains_key("gonof_csrf"));
    assert!(
        fixture
            .identity
            .session(&fixture.member.session_token)
            .await
            .unwrap()
            .is_none()
    );
    // The member never verified the address; opening the link mailed to it proves it.
    assert_eq!(
        fixture
            .scalar("SELECT emailstatus FROM gonof_common_member WHERE username='securitymember'")
            .await,
        1
    );
    get(&fixture, "/login", &mut cookies).await;
    let replay = post(
        &fixture,
        "/reset-password",
        &mut cookies,
        &[
            ("token", &token),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ],
    )
    .await;
    replay.status(StatusCode::UNPROCESSABLE_ENTITY);
    replay.no_password();
    assert!(!replay.html.contains("name=\"token\""));
    assert!(
        fixture
            .identity
            .login("securitymember", NEW_PASSWORD)
            .await
            .is_ok()
    );
    fixture
        .identity
        .request_password_reset("security@example.test")
        .await
        .unwrap();
    let expired = fixture.reset_token().await;
    gono_forum_web::db::query("UPDATE gonof_common_member_token SET dateline=dateline-7200, expires_at=expires_at-7200 WHERE token_hash=?").bind(identity::token_hash(&expired)).execute(fixture.db.executor()).await.unwrap();
    get(
        &fixture,
        &format!("/reset-password?token={expired}"),
        &mut cookies,
    )
    .await
    .status(StatusCode::BAD_REQUEST);
    post(
        &fixture,
        "/reset-password",
        &mut cookies,
        &[
            ("token", &expired),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn local_logout_clears_only_this_browser_and_account_logout_invalidates_every_login() {
    let fixture = Fixture::new(&[]).await;
    let mut cookies = Cookies::member(&fixture);
    let original = fixture.member.session_token.clone();
    let second = fixture
        .identity
        .login("securitymember", PASSWORD)
        .await
        .unwrap();
    let page = get(&fixture, "/my/account", &mut cookies).await;
    page.status(StatusCode::OK);
    assert!(page.html.contains("action=\"/my/account/logout-all\""));
    assert!(!page.html.contains("/settings/sessions"));
    for (method, path) in [
        ("GET", "/settings/sessions"),
        ("POST", "/settings/sessions/revoke-others"),
        ("POST", "/settings/sessions/old-device/revoke"),
    ] {
        request(&fixture, method, path, &mut cookies, &[], &[])
            .await
            .status(StatusCode::NOT_FOUND);
    }
    post(&fixture, "/logout", &mut cookies, &[])
        .await
        .redirect("/");
    assert!(!cookies.0.contains_key("gonof_session"));
    // Clearing the cookie does not revoke copied credentials; a single device cannot be revoked on its own.
    for raw in [&original, &second.session_token] {
        assert!(fixture.identity.session(raw).await.unwrap().is_some());
    }
    cookies = Cookies::session(&second.session_token);
    let previous_version = fixture
        .scalar(
            "SELECT credentials_version FROM gonof_common_member WHERE username='securitymember'",
        )
        .await;
    post(&fixture, "/my/account/logout-all", &mut cookies, &[])
        .await
        .redirect("/login?logged_out_all=true");
    assert!(!cookies.0.contains_key("gonof_session"));
    assert_eq!(
        fixture
            .scalar(
                "SELECT credentials_version FROM gonof_common_member WHERE username='securitymember'"
            )
            .await,
        previous_version + 1
    );
    for raw in [&original, &second.session_token] {
        assert!(fixture.identity.session(raw).await.unwrap().is_none());
    }
    assert!(
        fixture
            .identity
            .session(&fixture.admin.session_token)
            .await
            .unwrap()
            .is_some()
    );
    let mut stale = Cookies::session(&original);
    get(&fixture, "/my/account", &mut stale)
        .await
        .redirect("/login?return_to=%2Fmy%2Faccount");
    assert!(!stale.0.contains_key("gonof_session"));
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn every_security_write_rejects_missing_cross_origin_and_null_origin_csrf() {
    let fixture = Fixture::new(&[]).await;
    fixture
        .identity
        .request_password_reset("security@example.test")
        .await
        .unwrap();
    let token = fixture.reset_token().await;
    let mut cookies = Cookies::member(&fixture);
    for path in [
        "/my/account/password".to_owned(),
        "/forgot-password".into(),
        "/reset-password".into(),
        "/my/account/logout-all".into(),
    ] {
        let csrf = cookies.csrf(&fixture.identity).await;
        let fields = [
            ("_csrf", csrf.as_str()),
            ("token", token.as_str()),
            ("email", "security@example.test"),
            ("current_password", PASSWORD),
            ("password", NEW_PASSWORD),
            ("password2", NEW_PASSWORD),
        ];
        request(&fixture, "POST", &path, &mut cookies, &fields[1..], &[])
            .await
            .status(StatusCode::FORBIDDEN);
        for headers in [
            [("Origin", "https://attacker.example")],
            [("Origin", "null")],
            [("Sec-Fetch-Site", "cross-site")],
        ] {
            let response = request(&fixture, "POST", &path, &mut cookies, &fields, &headers).await;
            response.status(StatusCode::FORBIDDEN);
            response.no_password();
        }
        get(&fixture, &path, &mut cookies)
            .await
            .status(if path == "/forgot-password" {
                StatusCode::OK
            } else if path == "/reset-password" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::METHOD_NOT_ALLOWED
            });
    }
    assert!(
        fixture
            .identity
            .password_reset_status(&token)
            .await
            .unwrap()
    );
    assert!(
        fixture
            .identity
            .session(&fixture.member.session_token)
            .await
            .unwrap()
            .is_some()
    );
    let mut anonymous = Cookies::default();
    for (path, to) in [("/my/account", "/login?return_to=%2Fmy%2Faccount")] {
        get(&fixture, path, &mut anonymous).await.redirect(to);
    }
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn all_seven_security_throttles_are_configurable_without_consuming_reset_tokens() {
    // Set each item to its hard minimum in turn and the others to their maximum: requests within the minimum go
    // through, one more gets 429.
    const LIMITS: [(&str, i64, i64); 7] = [
        ("settings.account.password_change_account_per_5min", 3, 20),
        ("settings.account.password_change_ip_per_5min", 5, 100),
        ("settings.account.forgot_email_per_minute", 1, 5),
        ("settings.account.forgot_email_per_hour", 1, 20),
        ("settings.account.forgot_ip_per_hour", 5, 100),
        ("settings.account.reset_token_per_5min", 3, 30),
        ("settings.account.reset_ip_per_5min", 5, 100),
    ];
    for (dimension, (_, minimum, _)) in LIMITS.iter().enumerate() {
        let values: Vec<(&str, String)> = LIMITS
            .iter()
            .enumerate()
            .map(|(index, (key, min, max))| {
                (*key, if index == dimension { min } else { max }.to_string())
            })
            .collect();
        let settings: Vec<(&str, &str)> = values
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let fixture = Fixture::new(&settings).await;
        fixture
            .identity
            .request_password_reset("security@example.test")
            .await
            .unwrap();
        let token = fixture.reset_token().await;
        let mut cookies = Cookies::member(&fixture);
        let (path, fields, first_status) = match dimension {
            // The two password-change buckets only count when the current password is wrong, so the current
            // password is deliberately wrong here.
            0 | 1 => (
                "/my/account/password",
                vec![
                    ("current_password", "Not-the-current-password-2026!"),
                    ("password", NEW_PASSWORD),
                    ("password2", NEW_PASSWORD),
                ],
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            2..=4 => (
                "/forgot-password",
                vec![("email", "absent@example.test")],
                StatusCode::SEE_OTHER,
            ),
            _ => (
                "/reset-password",
                vec![
                    ("token", token.as_str()),
                    ("password", PASSWORD),
                    ("password2", NEW_PASSWORD),
                ],
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        };
        for _ in 0..*minimum {
            post(&fixture, path, &mut cookies, &fields)
                .await
                .status(first_status);
        }
        post(&fixture, path, &mut cookies, &fields)
            .await
            .status(StatusCode::TOO_MANY_REQUESTS);
        assert!(
            fixture
                .identity
                .password_reset_status(&token)
                .await
                .unwrap()
        );
        assert!(
            fixture
                .identity
                .session(&fixture.member.session_token)
                .await
                .unwrap()
                .is_some()
        );
        fixture.db.pool.close().await;
    }
}

/// Both routes share the account bucket; concurrent requests over the limit must be rejected before the password
/// hash runs.
#[tokio::test]
async fn concurrent_password_guesses_share_the_failure_budget() {
    let fixture = std::sync::Arc::new(Fixture::new(&[]).await);
    let mut jobs = tokio::task::JoinSet::new();
    let gate = std::sync::Arc::new(tokio::sync::Barrier::new(12));
    for i in 0..12 {
        let fixture = fixture.clone();
        let gate = gate.clone();
        jobs.spawn(async move {
            let mut cookies = Cookies::member(&fixture);
            let csrf = cookies.csrf(&fixture.identity).await;
            let fields = [
                ("_csrf", csrf.as_str()),
                ("current_password", "Wrong-current-password"),
                ("password", NEW_PASSWORD),
                ("password2", NEW_PASSWORD),
            ];
            let path = if i % 2 == 0 {
                "/my/account/password"
            } else {
                "/my/account/two-factor/setup"
            };
            gate.wait().await;
            request(&fixture, "POST", path, &mut cookies, &fields, &[])
                .await
                .status
        });
    }
    let mut verified = 0;
    let mut rejected = 0;
    while let Some(job) = jobs.join_next().await {
        match job.unwrap() {
            StatusCode::UNPROCESSABLE_ENTITY => verified += 1,
            StatusCode::TOO_MANY_REQUESTS => rejected += 1,
            status => panic!("unexpected status {status}"),
        }
    }
    assert_eq!((verified, rejected), (5, 7));
    fixture._services.cleanup_ref().await.unwrap();
}

/// The two buckets shared by password change and two-factor setup only count when the current password is wrong:
/// mismatched new passwords and starting two-factor setup with the correct password use no
/// attempts; only after one wrong current password is the next attempt blocked.
#[tokio::test]
async fn password_change_buckets_count_only_wrong_current_passwords() {
    // Set the per-account bucket to its hard minimum of 3 and the per-IP bucket to its maximum.
    let fixture = Fixture::new(&[
        ("settings.account.password_change_account_per_5min", "3"),
        ("settings.account.password_change_ip_per_5min", "100"),
    ])
    .await;
    // Two-step verification can only be set up once the email is verified.
    gono_forum_web::db::query(
        "UPDATE gonof_common_member SET emailstatus=1 WHERE username='securitymember'",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let mut cookies = Cookies::member(&fixture);
    let mismatch = [
        ("current_password", PASSWORD),
        ("password", NEW_PASSWORD),
        ("password2", "Security-different-confirmation-2026!"),
    ];
    for _ in 0..2 {
        post(&fixture, "/my/account/password", &mut cookies, &mismatch)
            .await
            .status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    for _ in 0..2 {
        post(
            &fixture,
            "/my/account/two-factor/setup",
            &mut cookies,
            &[("current_password", PASSWORD)],
        )
        .await
        .status(StatusCode::OK);
    }
    for _ in 0..3 {
        post(
            &fixture,
            "/my/account/two-factor/setup",
            &mut cookies,
            &[("current_password", "Not-the-current-password-2026!")],
        )
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    post(&fixture, "/my/account/password", &mut cookies, &mismatch)
        .await
        .status(StatusCode::TOO_MANY_REQUESTS);
    post(
        &fixture,
        "/my/account/two-factor/setup",
        &mut cookies,
        &[("current_password", PASSWORD)],
    )
    .await
    .status(StatusCode::TOO_MANY_REQUESTS);
    assert!(
        fixture
            .identity
            .session(&fixture.member.session_token)
            .await
            .unwrap()
            .is_some()
    );
    fixture.db.pool.close().await;
}

/// The three password reset rate limits short-circuit in order: requests stopped by IP do not count toward the
/// e-mail. The submitter can make up any e-mail, so without short-circuiting a single IP could stuff the rate limit
/// table with new keys without end.
#[tokio::test]
async fn forgot_password_counts_an_email_only_after_its_ip_passes() {
    // Set the per-IP reset count to its hard minimum of 5.
    let fixture = Fixture::new(&[("settings.account.forgot_ip_per_hour", "5")]).await;
    let forgot = async |peer: &str, email: &str| {
        let mut cookies = Cookies::member(&fixture);
        let csrf = cookies.csrf(&fixture.identity).await;
        request_from(
            &fixture,
            peer,
            "POST",
            "/forgot-password",
            &mut cookies,
            &[("email", email), ("_csrf", &csrf)],
            &[],
        )
        .await
    };
    forgot("192.0.2.33:48321", "first@example.test")
        .await
        .redirect("/forgot-password?sent=true");
    for index in 2..=5 {
        forgot("192.0.2.33:48321", &format!("filler{index}@example.test"))
            .await
            .redirect("/forgot-password?sent=true");
    }
    forgot("192.0.2.33:48321", "second@example.test")
        .await
        .status(StatusCode::TOO_MANY_REQUESTS);
    // The previous attempt was stopped by IP, so second's e-mail quota is untouched and it can still apply from
    // another IP.
    forgot("192.0.2.34:48321", "second@example.test")
        .await
        .redirect("/forgot-password?sent=true");
    // The e-mail rate limit itself still applies across IPs, and spelling variants (here a full-width "FIRST") count
    // as the same e-mail.
    forgot(
        "192.0.2.35:48321",
        "\u{ff26}\u{ff29}\u{ff32}\u{ff33}\u{ff34}@example.test",
    )
    .await
    .status(StatusCode::TOO_MANY_REQUESTS);
    fixture.db.pool.close().await;
}

#[tokio::test]
async fn token_pages_preserve_referrer_protection_when_identity_database_fails() {
    let fixture = Fixture::new(&[]).await;
    fixture.db.pool.close().await;
    let token = identity::random_token();
    for path in ["/reset-password", "/verify-email", "/confirm-email-change"] {
        let mut cookies = Cookies::member(&fixture);
        let response = get(&fixture, &format!("{path}?token={token}"), &mut cookies).await;
        assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers[header::REFERRER_POLICY], "strict-origin");
        assert!(!response.html.contains(&token));
    }
    fixture._services.cleanup_ref().await.unwrap();
}
