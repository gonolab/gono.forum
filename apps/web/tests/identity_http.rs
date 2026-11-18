use std::{collections::BTreeMap, path::PathBuf};

use axum::{
    Router,
    body::Body,
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
const PASSWORD: &str = "Http-contract-password-4839";

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    root: PathBuf,
    db: Database,
    identity: Identity,
    config: Config,
    app: Router,
    admin_password: String,
    /// This fixture's forum URL (`settings.site.url`); the request's Host, Origin and forwarded protocol all
    /// follow it.
    origin: String,
}

impl Fixture {
    async fn new(debug: bool, origin: &str) -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        // The debug panel and the forum URL are both admin-area settings.
        gono_forum_web::test_support::set_setting(
            &db,
            "settings.debug.panel",
            if debug { "true" } else { "false" },
        )
        .await
        .unwrap();
        gono_forum_web::test_support::set_site_url(&db, origin)
            .await
            .unwrap();
        content::seed_forums(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let app = router::build_with_global(
            config.clone(),
            &root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            _services: services,
            root,
            db,
            identity,
            config,
            app,
            admin_password,
            origin: origin.into(),
        }
    }

    async fn admin(&self) -> SessionGrant {
        self.identity
            .login("admin", &self.admin_password)
            .await
            .unwrap()
    }

    async fn token(&self, username: &str) -> String {
        let row = gono_forum_web::db::query("SELECT o.payload FROM gonof_common_outbox o
            JOIN gonof_common_member m ON m.uid = o.aggregate_id WHERE m.username = ? AND o.event_type = 'email_verification' ORDER BY o.eventid DESC LIMIT 1")
            .bind(username).fetch_one(self.db.executor()).await.unwrap();
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

    async fn execute(&self, sql: &str) {
        gono_forum_web::db::query(sql)
            .execute(self.db.executor())
            .await
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Default, Clone)]
struct Cookies(BTreeMap<String, String>);

impl Cookies {
    fn for_session(raw: &str, secure: bool) -> Self {
        Self(BTreeMap::from([(
            if secure {
                "__Host-gonof_session__port58833"
            } else {
                "gonof_session__port58833"
            }
            .into(),
            raw.into(),
        )]))
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn update(&mut self, headers: &HeaderMap) {
        for cookie in headers.get_all(header::SET_COOKIE) {
            let cookie = cookie.to_str().unwrap();
            let (name, value) = cookie.split(';').next().unwrap().split_once('=').unwrap();
            if value.is_empty() {
                self.0.remove(name);
            } else {
                self.0.insert(name.into(), value.into());
            }
        }
    }

    /// The session cookie of a forum at the root of the test site's port, or of one on a default port.
    fn session(&self) -> Option<&str> {
        [
            "gonof_session__port58833",
            "__Host-gonof_session__port58833",
            "gonof_session",
            "__Host-gonof_session",
        ]
        .iter()
        .find_map(|name| self.0.get(*name))
        .map(String::as_str)
    }

    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self.session() {
            identity
                .session(raw)
                .await
                .unwrap()
                .map(|session| session.csrf_token)
                .unwrap_or_default()
        } else {
            [
                "gonof_csrf__port58833",
                "__Host-gonof_csrf__port58833",
                "gonof_csrf",
                "__Host-gonof_csrf",
            ]
            .iter()
            .find_map(|name| self.0.get(*name))
            .cloned()
            .unwrap_or_default()
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
        assert_eq!(self.status, expected, "response status differs");
        assert!(
            self.headers[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
    }

    fn redirect(&self, location: &str) {
        self.status(StatusCode::SEE_OTHER);
        assert_eq!(self.headers[header::LOCATION], location);
    }

    fn debug(&self, expected: bool) {
        assert_eq!(self.html.contains("id=\"gonof-debug\""), expected);
        assert_eq!(self.html.contains("/static/css/debug.css"), expected);
        assert_eq!(self.html.contains("/static/js/debug.js"), expected);
        if !expected {
            assert!(!self.html.contains("data-debug-sql"));
        }
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
    // A forum under a sub-path has its address with the path; Host and Origin carry only the scheme, host and port.
    let site_origin = url::Url::parse(&fixture.origin)
        .unwrap()
        .origin()
        .ascii_serialization();
    let mut builder = Request::builder().method(method).uri(path).header(
        header::HOST,
        gono_forum_web::test_support::host_of(&site_origin),
    );
    // The Secure flag and the __Host- prefix of the cookie depend on the connection protocol, not on the site
    // URL: an HTTPS site has to forward X-Forwarded-Proto like a reverse proxy does, plus a loopback peer, before
    // it is trusted.
    if fixture.origin.starts_with("https://") {
        builder = builder.header("x-forwarded-proto", "https");
    }
    if !cookies.0.is_empty() {
        builder = builder.header(header::COOKIE, cookies.header());
    }
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if !extra
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("origin"))
        {
            builder = builder.header(header::ORIGIN, &site_origin);
        }
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut request = builder.body(Body::from(form)).unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41000".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = fixture.app.clone().oneshot(request).await.unwrap();
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
async fn native_forms_connect_invitation_registration_verification_login_and_logout() {
    let fixture = Fixture::new(false, ORIGIN).await;
    let mut admin = Cookies::default();
    let login = get(&fixture, "/login", &mut admin).await;
    login.status(StatusCode::OK);
    assert!(login.html.contains("method=\"post\" action=\"/login\""));
    assert!(login.html.contains("hx-boost=\"false\""));
    assert!(login.html.contains("hx-history=\"false\""));
    post(
        &fixture,
        "/login",
        &mut admin,
        &[("username", "admin"), ("password", &fixture.admin_password)],
    )
    .await
    .redirect("/");
    assert!(admin.session().is_some());
    post(
        &fixture,
        "/my/invites",
        &mut admin,
        &[("request_key", "http-first-invite")],
    )
    .await
    .redirect("/my/invites");
    post(
        &fixture,
        "/my/invites",
        &mut admin,
        &[("request_key", "http-first-invite")],
    )
    .await
    .redirect("/my/invites");
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_invite")
            .await,
        1
    );
    let invite = fixture
        .identity
        .invites(
            fixture
                .scalar("SELECT uid FROM gonof_common_member WHERE username='admin'")
                .await,
            1,
        )
        .await
        .unwrap()
        .rows
        .remove(0);
    assert!(
        get(&fixture, "/my/invites", &mut admin)
            .await
            .html
            .contains(&invite.code)
    );

    let mut member = Cookies::default();
    let form = get(&fixture, "/register", &mut member).await;
    form.status(StatusCode::OK);
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&form.html);
    let fields = [
        ("username", "HttpMember"),
        ("email", "Member@Example.test"),
        ("password", PASSWORD),
        ("password2", "different-password"),
        ("invite", &invite.code),
    ];
    // A long-lived form may outlive its anonymous cookie. Keep its non-password inputs
    // and issue a fresh CSRF token so re-entering the password is enough to recover.
    let stale_csrf = member.csrf(&fixture.identity).await;
    member.0.remove("gonof_csrf__port58833");
    let mut stale_fields = fields.to_vec();
    stale_fields.push(("_csrf", &stale_csrf));
    let expired_form = request(
        &fixture,
        "POST",
        "/register",
        &mut member,
        &stale_fields,
        &[],
    )
    .await;
    expired_form.status(StatusCode::FORBIDDEN);
    assert!(expired_form.html.contains("value=\"HttpMember\""));
    assert!(expired_form.html.contains("value=\"Member@Example.test\""));
    assert!(expired_form.html.contains(&invite.code));
    assert!(!expired_form.html.contains(PASSWORD));
    assert!(!expired_form.html.contains("different-password"));
    assert_ne!(member.csrf(&fixture.identity).await, stale_csrf);
    assert!(expired_form.html.contains(&format!(
        "name=\"_csrf\" value=\"{}\"",
        member.csrf(&fixture.identity).await
    )));
    let mismatch = post(&fixture, "/register", &mut member, &fields).await;
    mismatch.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(mismatch.html.contains("value=\"HttpMember\""));
    assert!(mismatch.html.contains("value=\"Member@Example.test\""));
    assert!(mismatch.html.contains(&invite.code));
    assert!(!mismatch.html.contains(PASSWORD));
    assert!(!mismatch.html.contains("different-password"));
    let fields = [
        ("username", "HttpMember"),
        ("email", "Member@Example.test"),
        ("password", PASSWORD),
        ("password2", PASSWORD),
        ("invite", &invite.code),
        ("accept_terms", "1"),
        (terms.0, &terms.1),
        (privacy.0, &privacy.1),
    ];
    post(&fixture, "/register", &mut member, &fields)
        .await
        .redirect("/verify-email");
    let original_session = member.session().unwrap().to_owned();
    let home = get(&fixture, "/", &mut member).await;
    home.status(StatusCode::OK);
    assert!(home.html.contains("httpmember"));
    assert!(home.html.contains("banner-row"));
    assert!(home.html.contains("action=\"/logout\""));
    get(&fixture, "/my/invites", &mut member)
        .await
        .status(StatusCode::FORBIDDEN);
    post(
        &fixture,
        "/my/invites",
        &mut member,
        &[("request_key", "unverified-attempt")],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    let token = fixture.token("httpmember").await;
    let verify = get(
        &fixture,
        &format!("/verify-email?token={token}"),
        &mut member,
    )
    .await;
    verify.status(StatusCode::OK);
    assert_eq!(verify.headers[header::REFERRER_POLICY], "strict-origin");
    assert!(verify.html.contains("Confirm email verification"));
    assert_eq!(
        fixture
            .scalar("SELECT emailstatus FROM gonof_common_member WHERE username='httpmember'")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE consumed_at IS NOT NULL")
            .await,
        0
    );
    request(
        &fixture,
        "POST",
        "/verify-email",
        &mut member,
        &[("token", &token), ("_csrf", "bad-csrf")],
        &[],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    post(&fixture, "/verify-email", &mut member, &[("token", &token)])
        .await
        .redirect("/verify-email?verified=true");
    assert_eq!(
        fixture
            .scalar("SELECT emailstatus FROM gonof_common_member WHERE username='httpmember'")
            .await,
        1
    );
    post(&fixture, "/verify-email", &mut member, &[("token", &token)])
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        !get(&fixture, "/", &mut member)
            .await
            .html
            .contains("banner-row")
    );
    post(
        &fixture,
        "/my/invites",
        &mut member,
        &[("request_key", "member-invite")],
    )
    .await
    .redirect("/my/invites");
    let member_invite_id = fixture
        .scalar("SELECT id FROM gonof_common_invite WHERE request_key='member-invite'")
        .await;
    let admin_private = fixture
        .identity
        .generate_invite(invite.uid, "private-admin-code")
        .await
        .unwrap();
    let own_invites = get(
        &fixture,
        &format!("/my/invites?uid={}", invite.uid),
        &mut member,
    )
    .await;
    own_invites.status(StatusCode::OK);
    assert!(!own_invites.html.contains(&admin_private.code));
    post(
        &fixture,
        &format!("/my/invites/{member_invite_id}/revoke"),
        &mut admin,
        &[],
    )
    .await
    .status(StatusCode::NOT_FOUND);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM gonof_common_invite WHERE request_key='member-invite' AND status='available'").await, 1);
    post(
        &fixture,
        &format!("/my/invites/{member_invite_id}/revoke"),
        &mut member,
        &[],
    )
    .await
    .redirect("/my/invites");
    get(&fixture, "/logout", &mut member)
        .await
        .status(StatusCode::METHOD_NOT_ALLOWED);
    post(&fixture, "/logout", &mut member, &[])
        .await
        .redirect("/");
    assert!(member.session().is_none());
    let mut old_cookie = Cookies::for_session(&original_session, false);
    assert!(
        get(&fixture, "/", &mut old_cookie)
            .await
            .html
            .contains("action=\"/logout\"")
    );
    assert!(old_cookie.session().is_some());
    get(&fixture, "/login", &mut member)
        .await
        .status(StatusCode::OK);
    post(
        &fixture,
        "/login",
        &mut member,
        &[("username", "MEMBER@example.test"), ("password", PASSWORD)],
    )
    .await
    .redirect("/");
    assert_ne!(member.session().unwrap(), original_session);

    let mut second = Cookies::default();
    let form = get(&fixture, "/register", &mut second).await;
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&form.html);
    let duplicate = [
        ("username", "secondmember"),
        ("email", "second@example.test"),
        ("password", PASSWORD),
        ("password2", PASSWORD),
        ("invite", &invite.code),
        ("accept_terms", "1"),
        (terms.0, &terms.1),
        (privacy.0, &privacy.1),
    ];
    post(&fixture, "/register", &mut second, &duplicate)
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username='secondmember'")
            .await,
        0
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

/// Registration requires ticking agreement to the terms of use and the privacy notice:
/// without the tick it is rejected, no account is created and no invite code is consumed. The form carries the
/// versions it showed; when either document changed before the form was sent (a new save, or the built-in privacy
/// notice following the username-history setting), the form comes back with the new versions and an unticked box.
/// The registration log records the versions agreed to: the last save time and a digest of the text shown.
#[tokio::test]
async fn registration_requires_consent_and_logs_the_policy_versions() {
    let fixture = Fixture::new(false, ORIGIN).await;
    let admin = fixture.admin().await;
    let terms = gono_forum_web::runtime::save_document(
        &fixture.db,
        &fixture._services.global,
        admin.member.actor(),
        "policy.terms",
        gono_forum_web::runtime::Document {
            title: "Terms of Use".into(),
            body: "The first version of the terms".into(),
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    assert!(terms.updated_at > 0);
    let invite = fixture
        .identity
        .generate_invite(admin.member.uid, &"c".repeat(64))
        .await
        .unwrap();

    let mut member = Cookies::default();
    let form = get(&fixture, "/register", &mut member).await;
    form.status(StatusCode::OK);
    assert!(
        form.html
            .contains("type=\"checkbox\" name=\"accept_terms\" value=\"1\" required")
    );
    assert!(form.html.contains("href=\"/terms\" target=\"_blank\""));
    assert!(form.html.contains("href=\"/privacy\" target=\"_blank\""));
    let mut fields = vec![
        ("username", "ConsentMember"),
        ("email", "consent@example.test"),
        ("password", PASSWORD),
        ("password2", PASSWORD),
        ("invite", invite.code.as_str()),
    ];
    let [shown_terms, shown_privacy] = gono_forum_web::test_support::policy_fields(&form.html);
    fields.push((shown_terms.0, &shown_terms.1));
    fields.push((shown_privacy.0, &shown_privacy.1));
    let refused = post(&fixture, "/register", &mut member, &fields).await;
    refused.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .html
            .contains("Please read and agree to the terms of use and the privacy notice first")
    );
    assert!(refused.html.contains("value=\"ConsentMember\""));
    assert!(!refused.html.contains(PASSWORD));
    assert!(member.session().is_none());
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username='consentmember'")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(&format!(
                "SELECT COUNT(*) FROM gonof_common_invite WHERE id={} AND status='available'",
                invite.id
            ))
            .await,
        1,
        "a refused registration does not consume the invite code"
    );
    // The checkbox accepts only 1; any other value is rejected as well.
    fields.push(("accept_terms", "on"));
    post(&fixture, "/register", &mut member, &fields)
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    fields.pop();
    fields.push(("accept_terms", "1"));

    // The administrator saves new terms while the form is open.
    let new_terms = gono_forum_web::runtime::save_document(
        &fixture.db,
        &fixture._services.global,
        admin.member.actor(),
        "policy.terms",
        gono_forum_web::runtime::Document {
            title: "Terms of Use".into(),
            body: "The second version of the terms".into(),
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    let changed = post(&fixture, "/register", &mut member, &fields).await;
    changed.status(StatusCode::CONFLICT);
    assert!(
        changed
            .html
            .contains("The terms of use or the privacy notice changed after this page was opened")
    );
    assert!(changed.html.contains("value=\"ConsentMember\""));
    assert!(
        !changed.html.contains("value=\"1\" checked"),
        "the box must be ticked again for the new terms"
    );
    let [current_terms, current_privacy] =
        gono_forum_web::test_support::policy_fields(&changed.html);
    assert_ne!(current_terms.1, shown_terms.1);
    assert_eq!(current_privacy.1, shown_privacy.1);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username='consentmember'")
            .await,
        0
    );

    // The built-in privacy notice names the username-history period, so changing it changes the notice.
    gono_forum_web::test_support::set_setting(&fixture.db, "settings.username.history_days", "120")
        .await
        .unwrap();
    fixture._services.global.clear_local();
    let mut resent = fields.clone();
    resent.retain(|(name, _)| !name.ends_with("_version"));
    resent.push((current_terms.0, &current_terms.1));
    resent.push((current_privacy.0, &current_privacy.1));
    let changed = post(&fixture, "/register", &mut member, &resent).await;
    changed.status(StatusCode::CONFLICT);
    let [latest_terms, latest_privacy] = gono_forum_web::test_support::policy_fields(&changed.html);
    assert_eq!(latest_terms.1, current_terms.1);
    assert_ne!(latest_privacy.1, current_privacy.1);

    resent.retain(|(name, _)| !name.ends_with("_version"));
    resent.push((latest_terms.0, &latest_terms.1));
    resent.push((latest_privacy.0, &latest_privacy.1));
    post(&fixture, "/register", &mut member, &resent)
        .await
        .redirect("/verify-email");
    let data: String = db::query_scalar("SELECT l.data FROM gonof_common_log l JOIN gonof_common_member m ON m.uid=l.uid WHERE m.username='consentmember' AND l.type='identity.register'")
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert!(new_terms.updated_at >= terms.updated_at);
    assert_eq!(data["consent"]["terms_version"], new_terms.updated_at);
    assert_eq!(data["consent"]["terms_digest"], latest_terms.1.as_str());
    assert_eq!(
        data["consent"]["privacy_version"], 0,
        "the privacy notice was never saved, so its version is 0"
    );
    assert_eq!(data["consent"]["privacy_digest"], latest_privacy.1.as_str());
    assert_eq!(latest_privacy.1.len(), 64);
}

/// Two independent forums on one host, one at the root and one under `/bbs`, each with its own database: a browser
/// signed in to both keeps both sessions, because each forum names its cookies after its path (and port).
#[tokio::test]
async fn forums_on_different_paths_of_one_host_keep_their_own_sessions() {
    let root = Fixture::new(false, ORIGIN).await;
    let sub = Fixture::new(false, &format!("{ORIGIN}/bbs")).await;
    let mut browser = Cookies::default();
    get(&root, "/login", &mut browser)
        .await
        .status(StatusCode::OK);
    post(
        &root,
        "/login",
        &mut browser,
        &[("username", "admin"), ("password", &root.admin_password)],
    )
    .await
    .redirect("/");
    get(&sub, "/bbs/login", &mut browser)
        .await
        .status(StatusCode::OK);
    let sub_csrf = browser
        .0
        .get("gonof_csrf_bbs__port58833")
        .cloned()
        .expect("the sub-path forum issues its own CSRF cookie");
    let mut fields = vec![
        ("username", "admin"),
        ("password", sub.admin_password.as_str()),
    ];
    fields.push(("_csrf", &sub_csrf));
    request(&sub, "POST", "/bbs/login", &mut browser, &fields, &[])
        .await
        .redirect("/bbs/");
    assert!(browser.0.contains_key("gonof_session__port58833"));
    assert!(browser.0.contains_key("gonof_session_bbs__port58833"));
    let home = get(&root, "/", &mut browser).await;
    home.status(StatusCode::OK);
    assert!(
        home.html.contains("action=\"/logout\""),
        "the root forum still knows the member after signing in to the other forum"
    );
    let sub_home = get(&sub, "/bbs/", &mut browser).await;
    sub_home.status(StatusCode::OK);
    assert!(sub_home.html.contains("action=\"/bbs/logout\""));
}

/// When the return URL contains non-ASCII characters, a successful sign-in is still 303 and Location is the same
/// URL percent-encoded.
#[tokio::test]
async fn login_with_a_non_ascii_return_target_redirects_instead_of_failing() {
    let fixture = Fixture::new(false, ORIGIN).await;
    let mut member = Cookies::default();
    get(&fixture, "/login", &mut member).await;
    post(
        &fixture,
        "/login",
        &mut member,
        &[
            ("username", "admin"),
            ("password", &fixture.admin_password),
            // "\u{91d1}\u{5e01}" is the Chinese for "coins"; Location carries it percent-encoded.
            ("return_to", "/search?q=\u{91d1}\u{5e01}"),
        ],
    )
    .await
    .redirect("/search?q=%E9%87%91%E5%B8%81");
    assert!(member.session().is_some());
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

/// Sign-in applies NFKC normalization before querying the database, so full-width forms, mathematical letters
/// and case variants all sign in to the same account; the per-account rate-limit key must use the same
/// normalization, otherwise writing the name differently bypasses "10 attempts per account per 5 minutes".
#[tokio::test]
async fn login_throttle_counts_every_spelling_of_one_account_together() {
    let fixture = Fixture::new(false, ORIGIN).await;
    let mut member = Cookies::default();
    get(&fixture, "/login", &mut member).await;
    // First prove the full-width spelling really signs in as admin: these variants point to the same account.
    // The `\u{ff..}` escapes below are full-width Latin letters ("ADMIN", "admin", "Admin").
    post(
        &fixture,
        "/login",
        &mut member,
        &[
            ("username", "\u{ff21}\u{ff24}\u{ff2d}\u{ff29}\u{ff2e}"),
            ("password", &fixture.admin_password),
        ],
    )
    .await
    .redirect("/");
    // An overlong password is rejected before hashing and still counts toward the rate limit, so the test does
    // not have to run argon2 nine times.
    let long = "x".repeat(129);
    let mut probe = Cookies::default();
    get(&fixture, "/login", &mut probe).await;
    for username in [
        "\u{ff41}\u{ff44}\u{ff4d}\u{ff49}\u{ff4e}",
        "𝐚𝐝𝐦𝐢𝐧",
        "𝒶𝒹𝓂𝒾𝓃",
        "\u{ff21}\u{ff44}\u{ff4d}\u{ff49}\u{ff4e}",
        " ADMIN ",
        "Admin",
        "ⓐⓓⓜⓘⓝ",
        "\u{ff21}\u{ff24}\u{ff2d}\u{ff29}\u{ff2e}",
        "admin",
    ] {
        post(
            &fixture,
            "/login",
            &mut probe,
            &[("username", username), ("password", &long)],
        )
        .await
        .status(StatusCode::UNAUTHORIZED);
    }
    post(
        &fixture,
        "/login",
        &mut probe,
        &[("username", "admin"), ("password", &fixture.admin_password)],
    )
    .await
    .status(StatusCode::TOO_MANY_REQUESTS);
    assert!(probe.session().is_none());
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn csrf_and_primary_database_roles_control_forms_and_debug_output() {
    let fixture = Fixture::new(true, ORIGIN).await;
    let mut anonymous = Cookies::default();
    get(&fixture, "/login", &mut anonymous).await;
    let csrf = anonymous.csrf(&fixture.identity).await;
    let password_marker = "Never-echo-this-password";
    let fields = [
        ("username", "<script>alert(1)</script>"),
        ("password", password_marker),
        ("_csrf", &csrf),
    ];
    let failed = request(
        &fixture,
        "POST",
        "/login",
        &mut anonymous,
        &fields,
        &[("HX-Request", "true")],
    )
    .await;
    failed.status(StatusCode::UNAUTHORIZED);
    failed.debug(false);
    assert!(!failed.html.contains("<script>alert(1)</script>"));
    assert!(failed.html.contains("&#60;script&#62;"));
    assert!(!failed.html.contains(password_marker));
    request(
        &fixture,
        "POST",
        "/login",
        &mut anonymous,
        &fields,
        &[("Origin", "https://attacker.example")],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    request(
        &fixture,
        "POST",
        "/login",
        &mut anonymous,
        &fields,
        &[("Sec-Fetch-Site", "cross-site")],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    let missing_csrf = request(
        &fixture,
        "POST",
        "/login",
        &mut anonymous,
        &[
            ("username", "<script>alert(1)</script>"),
            ("password", password_marker),
        ],
        &[],
    )
    .await;
    missing_csrf.status(StatusCode::FORBIDDEN);
    assert!(missing_csrf.html.contains("&#60;script&#62;"));
    assert!(!missing_csrf.html.contains("<script>alert(1)</script>"));
    assert!(!missing_csrf.html.contains(password_marker));
    get(&fixture, "/my/invites", &mut anonymous)
        .await
        .redirect("/login");

    let admin_grant = fixture.admin().await;
    let admin_sid = admin_grant.session_token;
    let mut admin = Cookies::for_session(&admin_sid, false);
    let invite = fixture
        .identity
        .generate_invite(admin_grant.member.uid, "debug-member")
        .await
        .unwrap();
    let user_grant = fixture
        .identity
        .register("debugmember", "debug@example.test", PASSWORD, &invite.code)
        .await
        .unwrap();
    let mut ordinary = Cookies::for_session(&user_grant.session_token, false);
    for htmx in ["false", "true"] {
        for (cookies, expected) in [
            (&mut anonymous, false),
            (&mut ordinary, false),
            (&mut admin, true),
        ] {
            for path in [
                "/",
                "/page-0",
                "/verify-email?sent=invalid",
                "/my/invites?page=invalid",
            ] {
                let response = request(
                    &fixture,
                    "GET",
                    path,
                    cookies,
                    &[],
                    &[("HX-Request", htmx), ("X-Admin", "true")],
                )
                .await;
                response.status(match path {
                    "/" => StatusCode::OK,
                    "/page-0" => StatusCode::NOT_FOUND,
                    _ => StatusCode::BAD_REQUEST,
                });
                assert!(response.html.starts_with("<!doctype html>"));
                assert!(response.html.trim_end().ends_with("</html>"));
                if path.starts_with("/verify-email") {
                    assert_eq!(response.headers[header::REFERRER_POLICY], "strict-origin");
                }
                response.debug(expected && !path.starts_with("/verify-email"));
            }
        }
    }
    let token = fixture.token("debugmember").await;
    get(
        &fixture,
        &format!("/verify-email?token={token}"),
        &mut anonymous,
    )
    .await
    .status(StatusCode::OK);
    post(
        &fixture,
        "/verify-email",
        &mut anonymous,
        &[("token", &token)],
    )
    .await
    .redirect("/verify-email?verified=true");
    assert!(anonymous.session().is_none());
    let verified = get(&fixture, "/verify-email?verified=true", &mut anonymous).await;
    verified.status(StatusCode::OK);
    verified.debug(false);
    get(&fixture, "/", &mut ordinary).await.debug(false);
    // A session's CSRF token cannot be borrowed from another account.
    request(
        &fixture,
        "POST",
        "/my/invites",
        &mut admin,
        &[
            ("_csrf", &ordinary.csrf(&fixture.identity).await),
            ("request_key", "wrong-account-csrf"),
        ],
        &[],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    let overview_denied = get(&fixture, "/manage", &mut ordinary).await;
    overview_denied.status(StatusCode::FORBIDDEN);
    overview_denied.debug(false);
    let denied = get(&fixture, "/manage/content", &mut ordinary).await;
    denied.status(StatusCode::FORBIDDEN);
    denied.debug(false);
    let logged_out = post(&fixture, "/logout", &mut admin, &[]).await;
    logged_out.redirect("/");
    logged_out.debug(false);
    let mut stale = Cookies::for_session(&admin_sid, false);
    request(
        &fixture,
        "GET",
        "/page-0",
        &mut stale,
        &[],
        &[("HX-Request", "true")],
    )
    .await
    .debug(true);
    let fresh = fixture.admin().await;
    fixture
        .execute("UPDATE gonof_common_member SET freeze=1 WHERE username='admin'")
        .await;
    let mut frozen = Cookies::for_session(&fresh.session_token, false);
    let frozen_page = get(&fixture, "/", &mut frozen).await;
    frozen_page.status(StatusCode::OK);
    frozen_page.debug(false);
    assert!(frozen.session().is_none());
    fixture.execute("UPDATE gonof_common_member SET freeze=0, credentials_version=credentials_version+1 WHERE username='admin'").await;
    let mut changed = Cookies::for_session(&fresh.session_token, false);
    get(&fixture, "/", &mut changed).await.debug(false);
    assert!(changed.session().is_none());
    // Even an active administrator receives no profiler when the setting is off.
    gono_forum_web::test_support::set_setting(&fixture.db, "settings.debug.panel", "false")
        .await
        .unwrap();
    let config = fixture.config.clone();
    let app = router::build(config, &fixture.root, fixture.db.clone());
    let final_grant = fixture.admin().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(
                    header::COOKIE,
                    Cookies::for_session(&final_grant.session_token, false).header(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
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
    assert!(!html.contains("id=\"gonof-debug\""));
    assert!(!html.contains("/static/css/debug.css"));
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn https_uses_host_cookies_and_rejects_duplicate_session_cookies() {
    let fixture = Fixture::new(false, "https://forum.example.test").await;
    let mut cookies = Cookies::default();
    let page = get(&fixture, "/login", &mut cookies).await;
    page.status(StatusCode::OK);
    let cookie = page
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.starts_with("__Host-gonof_csrf="));
    for attribute in ["; Path=/", "; HttpOnly", "; SameSite=Lax", "; Secure"] {
        assert!(cookie.contains(attribute));
    }
    assert!(!cookie.contains("Domain="));
    let logged_in = post(
        &fixture,
        "/login",
        &mut cookies,
        &[("username", "admin"), ("password", &fixture.admin_password)],
    )
    .await;
    logged_in.redirect("/");
    let session_cookie = logged_in
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .find(|value| value.to_str().unwrap().starts_with("__Host-gonof_session="))
        .unwrap()
        .to_str()
        .unwrap();
    assert!(session_cookie.contains("; Secure"));
    let sid = cookies.session().unwrap().to_owned();
    for duplicate in [identity::random_token(), "garbage".into()] {
        let mut duplicate_jar = Cookies::default();
        let response = request(
            &fixture,
            "GET",
            "/",
            &mut duplicate_jar,
            &[],
            &[(
                "Cookie",
                &format!("__Host-gonof_session={sid}; __Host-gonof_session={duplicate}"),
            )],
        )
        .await;
        response.status(StatusCode::OK);
        assert!(!response.html.contains("action=\"/logout\""));
        response.debug(false);
    }
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn cookie_rotation_preserves_csrf_and_identity_changes_never_restore_old_login() {
    let mut fixture = Fixture::new(false, ORIGIN).await;
    let grant = fixture.admin().await;
    let original = grant.session_token.clone();
    let old_key = fixture.config.identity.cookie.active_key.clone();
    fixture.config.identity.cookie.active_key = identity::random_token();
    fixture.config.identity.cookie.previous_keys = vec![old_key];
    fixture.identity = Identity::new(
        fixture.db.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    fixture.app = router::build(fixture.config.clone(), &fixture.root, fixture.db.clone());
    let mut cookies = Cookies::for_session(&original, false);
    let page = get(&fixture, "/my/account", &mut cookies).await;
    page.status(StatusCode::OK);
    assert_ne!(cookies.session().unwrap(), original);
    assert!(
        page.html
            .contains(&format!("name=\"_csrf\" value=\"{}\"", grant.csrf_token))
    );
    assert_eq!(cookies.csrf(&fixture.identity).await, grant.csrf_token);
    let renewed = cookies.session().unwrap().to_owned();
    // Signing in again from the same browser only issues a new cookie; it does not invalidate earlier valid
    // sign-ins.
    post(
        &fixture,
        "/login",
        &mut cookies,
        &[("username", "admin"), ("password", &fixture.admin_password)],
    )
    .await
    .redirect("/");
    assert_ne!(cookies.session().unwrap(), renewed);
    assert!(fixture.identity.session(&renewed).await.unwrap().is_some());
    for path in ["/logout", "/my/account/logout-all"] {
        // The old key produces a renewal in the middleware; the sign-out response must keep the final result of
        // clearing the cookie.
        let mut original_jar = Cookies::for_session(&original, false);
        let response = post(&fixture, path, &mut original_jar, &[]).await;
        response.redirect(if path == "/logout" {
            "/"
        } else {
            "/login?logged_out_all=true"
        });
        let session_headers: Vec<_> = response
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter(|value| {
                value
                    .to_str()
                    .unwrap()
                    .starts_with("gonof_session__port58833=")
            })
            .collect();
        assert_eq!(session_headers.len(), 1);
        assert!(
            session_headers[0]
                .to_str()
                .unwrap()
                .starts_with("gonof_session__port58833=;")
        );
        assert!(session_headers[0].to_str().unwrap().contains("Max-Age=0"));
        assert!(original_jar.session().is_none());
    }
    assert!(fixture.identity.session(&renewed).await.unwrap().is_none());
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn malformed_encrypted_cookies_and_duplicates_cannot_establish_a_member_context() {
    let fixture = Fixture::new(true, ORIGIN).await;
    let grant = fixture.admin().await;
    assert!(grant.session_token.starts_with("v1."));
    assert!(grant.session_token.len() > 64 && grant.session_token.len() <= 2048);
    let mut changed = grant.session_token.clone().into_bytes();
    let at = changed.len() - 8;
    changed[at] = if changed[at] == b'A' { b'B' } else { b'A' };
    for raw in [
        identity::random_token(),
        grant.session_token.replacen("v1.", "v2.", 1),
        String::from_utf8(changed).unwrap(),
        "x".repeat(2049),
    ] {
        let mut jar = Cookies::for_session(&raw, false);
        let response = get(&fixture, "/my/account", &mut jar).await;
        response.redirect("/login?return_to=%2Fmy%2Faccount");
        response.debug(false);
    }
    let cookie = format!("gonof_session__port58833={}", grant.session_token);
    let response = request(
        &fixture,
        "GET",
        "/my/account",
        &mut Cookies::default(),
        &[],
        &[("Cookie", &cookie), ("Cookie", &cookie)],
    )
    .await;
    response.redirect("/login?return_to=%2Fmy%2Faccount");
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/my/account")
                .header(header::COOKIE, &cookie)
                .header(
                    header::COOKIE,
                    axum::http::HeaderValue::from_bytes(b"gonof_session__port58833=\xff").unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "/login?return_to=%2Fmy%2Faccount"
    );
    let response = request(
        &fixture,
        "GET",
        "/login",
        &mut Cookies::default(),
        &[],
        &[(
            "Cookie",
            &format!("gonof_csrf__port58833={}", grant.session_token),
        )],
    )
    .await;
    response.status(StatusCode::OK);
    let csrf = response
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .find_map(|header| {
            header
                .to_str()
                .unwrap()
                .strip_prefix("gonof_csrf__port58833=")
        })
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(csrf.len(), 64);
    assert!(csrf.bytes().all(|byte| byte.is_ascii_hexdigit()));
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

/// The cookie's Secure flag and the CSRF same-origin check look only at this request, not at the site URL.
///
/// This is what makes an editable forum URL safe: since the site URL can be changed in the
/// admin area, a wrong value should only make outgoing links point to the wrong place; it must not make every
/// form fail to submit — including the form used to change the URL back.
#[tokio::test]
async fn transport_and_csrf_ignore_the_configured_site_address() {
    // The site URL is deliberately set to another domain completely unrelated to the actual access address.
    let fixture = Fixture::new(false, "https://configured-elsewhere.example.test").await;
    let host = "127.0.0.1:58833";

    let send = |method: &'static str,
                origin: Option<&'static str>,
                forwarded: Option<&'static str>,
                cookie: Option<String>,
                body: String| {
        let app = fixture.app.clone();
        async move {
            let mut builder = Request::builder()
                .method(method)
                .uri("/login")
                .header(header::HOST, host);
            if let Some(origin) = origin {
                builder = builder
                    .header(header::ORIGIN, origin)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            }
            if let Some(proto) = forwarded {
                builder = builder.header("x-forwarded-proto", proto);
            }
            if let Some(cookie) = cookie {
                builder = builder.header(header::COOKIE, cookie);
            }
            let mut request = builder.body(Body::from(body)).unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "127.0.0.1:41000".parse::<std::net::SocketAddr>().unwrap(),
            ));
            app.oneshot(request).await.unwrap()
        }
    };

    // The site URL is https, but this hop has no forwarded headers: the cookie is sent as for plain text, without
    // Secure and without the __Host- prefix. The URL plays no part in the decision.
    let plain = send("GET", None, None, None, String::new()).await;
    let issued = plain
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(issued.starts_with("gonof_csrf__port58833="), "got {issued}");
    assert!(
        !issued.contains("Secure"),
        "a plain-text connection gets no Secure cookie"
    );

    // Same config, with the reverse proxy declaring this hop as HTTPS: it immediately switches to __Host- and
    // Secure.
    let secure = send("GET", None, Some("https"), None, String::new()).await;
    let issued_secure = secure
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(issued_secure.starts_with("__Host-gonof_csrf__port58833="));
    assert!(issued_secure.contains("; Secure"));

    // CSRF: passes when Origin matches this request's Host, even though it is completely different from the site
    // URL.
    let token = issued
        .split_once('=')
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0
        .to_owned();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("_csrf", &token)
        .append_pair("username", "nobody")
        .append_pair("password", "wrong-password-value")
        .finish();
    let accepted = send(
        "POST",
        Some("http://127.0.0.1:58833"),
        None,
        Some(format!("gonof_csrf__port58833={token}")),
        form.clone(),
    )
    .await;
    assert_ne!(
        accepted.status(),
        StatusCode::FORBIDDEN,
        "a request from the same Host passes the same-origin check"
    );

    // With a foreign Origin it must be blocked, even if the token is correct.
    let rejected = send(
        "POST",
        Some("https://evil.example.test"),
        None,
        Some(format!("gonof_csrf__port58833={token}")),
        form,
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}
