//! Google and GitHub login: starting authorization and the callback, `state` and expiry, completing registration for
//! third-party sign-up, no merging on the same email, closed registration, binding and unbinding in account
//! security, no unbinding when it is the only recovery path left, setting a password for an account without
//! one after confirming through a third party, participation requirements recognizing third-party links, admin
//! credentials and the callback URL list, and callback URLs on other domains (with a sub-path).
//!
//! Both providers' token and user-info endpoints point at local fake services (`Config::oauth` can only be changed in
//! code): the authorization code is simply an identity the test made up.
use std::{collections::BTreeMap, collections::HashMap, net::SocketAddr};

use axum::{
    Form, Json, Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::{get as route_get, post as route_post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    },
    db::{self, Database},
    i18n::Lang,
    identity::{self, Identity, SessionGrant},
    login::Participation,
    router,
    test_support::{self, TEST_SITE_URL, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "External-login-password-2026!";
const GOOGLE_ID: &str = "gono-test.apps.googleusercontent.com";
const GOOGLE_SECRET: &str = "google-secret-value";
const GITHUB_ID: &str = "Iv23liGonoTest";
const GITHUB_SECRET: &str = "github-secret-value";
const DEMO2: &str = "http://demo2.test:58833/feedback";

/// Fake provider: Google's token endpoint signs the authorization code (base64url JSON) into the ID Token as-is;
/// GitHub's token is the authorization code, and `/user` and `/user/emails` read the identity from the token. Wrong
/// credentials or PKCE verifiers produce errors the way the provider would.
async fn fake_provider() -> String {
    async fn google_token(Form(form): Form<HashMap<String, String>>) -> axum::response::Response {
        if form.get("client_secret").map(String::as_str) != Some(GOOGLE_SECRET)
            || form.get("code_verifier").is_none_or(|v| v.len() < 43)
            || form.get("grant_type").map(String::as_str) != Some("authorization_code")
            || !form
                .get("redirect_uri")
                .is_some_and(|uri| uri.ends_with("/auth/google/callback"))
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_grant"})),
            )
                .into_response();
        }
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&form["code"]).unwrap()).unwrap();
        let mut token = serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": form["client_id"],
            "exp": identity::now() + 3600,
            "sub": claims["sub"],
            "nonce": claims["nonce"],
            "email_verified": claims["verified"],
        });
        if let Some(email) = claims.get("email") {
            token["email"] = email.clone();
        }
        if let Some(name) = claims.get("name") {
            token["name"] = name.clone();
        }
        let id_token = format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(token.to_string())
        );
        Json(serde_json::json!({"access_token": "unused", "id_token": id_token})).into_response()
    }
    async fn github_token(Form(form): Form<HashMap<String, String>>) -> axum::response::Response {
        if form.get("client_secret").map(String::as_str) != Some(GITHUB_SECRET)
            || form.get("code_verifier").is_none_or(|v| v.len() < 43)
        {
            return Json(serde_json::json!({"error": "bad_verification_code"})).into_response();
        }
        Json(serde_json::json!({"access_token": form["code"], "token_type": "bearer"}))
            .into_response()
    }
    fn github_identity(headers: &HeaderMap) -> serde_json::Value {
        assert!(
            headers.contains_key(header::USER_AGENT),
            "the GitHub API requires a User-Agent"
        );
        let token = headers[header::AUTHORIZATION]
            .to_str()
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap();
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token).unwrap()).unwrap()
    }
    async fn github_user(headers: HeaderMap) -> Json<serde_json::Value> {
        let identity = github_identity(&headers);
        Json(serde_json::json!({"id": identity["id"], "login": identity["login"]}))
    }
    async fn github_emails(headers: HeaderMap) -> axum::response::Response {
        let identity = github_identity(&headers);
        match identity.get("email") {
            Some(email) => Json(serde_json::json!([
                {"email": "old@example.test", "primary": false, "verified": true},
                {"email": email, "primary": true, "verified": true}
            ]))
            .into_response(),
            // The App has no email permission.
            None => StatusCode::FORBIDDEN.into_response(),
        }
    }
    let app = Router::new()
        .route("/google/token", route_post(google_token))
        .route("/github/token", route_post(github_token))
        .route("/github/api/user", route_get(github_user))
        .route("/github/api/user/emails", route_get(github_emails));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

struct Fixture {
    services: test_support::TestFixture,
    db: Database,
    identity: Identity,
    app: Router,
    admin: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let services = test_support::TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        let root = services.root.clone();
        content::seed_forums(&db).await.unwrap();
        set_setting(&db, "settings.login.methods", "google,github")
            .await
            .unwrap();
        set_setting(&db, "settings.signup.mode", "open")
            .await
            .unwrap();
        let fake = fake_provider().await;
        let mut config = services.config.clone();
        config.common.encryption_key = "5a".repeat(32);
        config.oauth = gono_forum_web::oauth::Endpoints {
            google_token: format!("{fake}/google/token"),
            github_token: format!("{fake}/github/token"),
            github_api: format!("{fake}/github/api"),
            loopback: true,
            ..Default::default()
        };
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let app = router::build_with_global(
            config,
            &root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            db,
            identity,
            app,
            admin,
        }
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }

    async fn execute(&self, sql: &str) {
        db::query(sql).execute(self.db.executor()).await.unwrap();
    }

    /// Saves one provider's credentials in the admin area.
    async fn configure(&self, provider: &str, client_id: &str, secret: &str) {
        let mut admin = Cookies::session(&self.admin.session_token);
        let revision = self
            .scalar("SELECT COUNT(*) FROM gonof_common_settings WHERE skey='login.providers'")
            .await;
        let revision = if revision == 0 {
            "0".to_owned()
        } else {
            let raw: String = db::query_scalar(
                "SELECT svalue FROM gonof_common_settings WHERE skey='login.providers'",
            )
            .fetch_one(self.db.executor())
            .await
            .unwrap();
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["revision"].to_string()
        };
        post(
            self,
            &format!("/manage/login/oauth/{provider}"),
            &mut admin,
            &[
                ("revision", &revision),
                ("client_id", client_id),
                ("client_secret", secret),
            ],
        )
        .await
        .redirect(&format!("/manage/login?saved={provider}"));
    }

    /// A member with a password and a verified email.
    async fn password_member(&self, username: &str, email: &str) -> SessionGrant {
        let invite = self
            .identity
            .generate_invite(self.admin.member.uid, &identity::random_token()[..24])
            .await
            .unwrap();
        let grant = self
            .identity
            .register(username, email, PASSWORD, &invite.code)
            .await
            .unwrap();
        self.execute(&format!(
            "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={}",
            grant.member.uid
        ))
        .await;
        self.identity.login(username, PASSWORD).await.unwrap()
    }
}

#[derive(Clone, Default)]
struct Cookies(BTreeMap<String, String>);

impl Cookies {
    fn session(raw: &str) -> Self {
        Self(BTreeMap::from([("gonof_session".into(), raw.into())]))
    }
    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self.0.get("gonof_session") {
            identity
                .session(raw)
                .await
                .unwrap()
                .map(|session| session.csrf_token)
                .unwrap_or_default()
        } else {
            self.0.get("gonof_csrf").cloned().unwrap_or_default()
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

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

impl Reply {
    fn redirect(&self, path: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.html);
        assert_eq!(self.headers[header::LOCATION], path, "{}", self.html);
    }
}

async fn request(
    fixture: &Fixture,
    origin: &str,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(
            header::HOST,
            test_support::host_of(origin).split('/').next().unwrap(),
        )
        .header(
            header::COOKIE,
            cookies
                .0
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 Chrome/141.0.0.0 Safari/537.36",
        );
    if method == "POST" {
        let scheme_host = origin.split('/').take(3).collect::<Vec<_>>().join("/");
        builder = builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, scheme_host);
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut req = builder.body(Body::from(form)).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.62:48321".parse::<SocketAddr>().unwrap(),
    ));
    let response = fixture.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    cookies.update(&headers);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        html: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(fixture: &Fixture, path: &str, cookies: &mut Cookies) -> Reply {
    request(fixture, TEST_SITE_URL, "GET", path, cookies, &[]).await
}

async fn post(
    fixture: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let csrf = cookies.csrf(&fixture.identity).await;
    let mut fields = fields.to_vec();
    fields.push(("_csrf", &csrf));
    request(fixture, TEST_SITE_URL, "POST", path, cookies, &fields).await
}

/// Starts authorization: a 302 to the provider; returns the query parameters of the authorization URL.
async fn start(
    fixture: &Fixture,
    origin: &str,
    path: &str,
    cookies: &mut Cookies,
) -> HashMap<String, String> {
    let reply = request(fixture, origin, "GET", path, cookies, &[]).await;
    assert_eq!(reply.status, StatusCode::FOUND, "{}", reply.html);
    assert!(
        cookies.0.contains_key("gonof_oauth"),
        "the authorization start cookie is set"
    );
    let location = url::Url::parse(reply.headers[header::LOCATION].to_str().unwrap()).unwrap();
    location.query_pairs().into_owned().collect()
}

fn google_code(sub: &str, email: Option<&str>, verified: bool, nonce: &str) -> String {
    let mut claims = serde_json::json!({"sub": sub, "verified": verified, "nonce": nonce, "name": "google member"});
    if let Some(email) = email {
        claims["email"] = email.into();
    }
    URL_SAFE_NO_PAD.encode(claims.to_string())
}

fn github_code(id: u64, login: &str, email: Option<&str>) -> String {
    let mut claims = serde_json::json!({"id": id, "login": login});
    if let Some(email) = email {
        claims["email"] = email.into();
    }
    URL_SAFE_NO_PAD.encode(claims.to_string())
}

/// Runs one full authorization: start, provider consent, callback. Returns the callback response.
async fn authorize(
    fixture: &Fixture,
    origin: &str,
    provider: &str,
    purpose: &str,
    cookies: &mut Cookies,
    code: impl FnOnce(&str) -> String,
) -> Reply {
    let query = if purpose.is_empty() {
        String::new()
    } else {
        format!("?purpose={purpose}")
    };
    let prefix = url::Url::parse(origin)
        .unwrap()
        .path()
        .trim_end_matches('/')
        .to_owned();
    let params = start(
        fixture,
        origin,
        &format!("{prefix}/auth/{provider}{query}"),
        cookies,
    )
    .await;
    assert_eq!(
        params["redirect_uri"],
        format!("{}/auth/{provider}/callback", origin.trim_end_matches('/'))
    );
    let nonce = params.get("nonce").cloned().unwrap_or_default();
    let callback = format!(
        "{prefix}/auth/{provider}/callback?state={}&code={}",
        params["state"],
        code(&nonce)
    );
    let reply = request(fixture, origin, "GET", &callback, cookies, &[]).await;
    assert!(
        !cookies.0.contains_key("gonof_oauth"),
        "the callback voids the authorization start cookie"
    );
    reply
}

async fn member_row(
    fixture: &Fixture,
    username: &str,
) -> (i64, Option<String>, i64, Option<String>) {
    let row = db::query(
        "SELECT uid,email,emailstatus,password FROM gonof_common_member WHERE username=?",
    )
    .bind(username)
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    (
        row.try_get("uid").unwrap(),
        row.try_get("email").unwrap(),
        row.try_get("emailstatus").unwrap(),
        row.try_get("password").unwrap(),
    )
}

#[tokio::test]
async fn google_registers_with_its_verified_email_and_logs_in_again() {
    let f = Fixture::new().await;
    // Both ticked, only Google configured: the login page only shows Google, the admin area warns that GitHub is not
    // configured, and the callback URL list has the main site.
    f.configure("google", GOOGLE_ID, GOOGLE_SECRET).await;
    let stored: String =
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey='login.providers'")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert!(
        !stored.contains(GOOGLE_SECRET),
        "the Client Secret is stored encrypted"
    );
    assert!(stored.contains(GOOGLE_ID));
    let mut admin = Cookies::session(&f.admin.session_token);
    let page = get(&f, "/manage/login", &mut admin).await;
    assert!(page.html.contains("data-oauth-unconfigured=\"github\""));
    assert!(!page.html.contains("data-oauth-unconfigured=\"google\""));
    assert!(
        page.html
            .contains(&format!("{TEST_SITE_URL}/auth/google/callback")),
        "{}",
        page.html
    );
    let mut visitor = Cookies::default();
    let login = get(&f, "/login", &mut visitor).await;
    assert!(
        login.html.contains("data-auth-provider=\"google\""),
        "{}",
        login.html
    );
    assert!(!login.html.contains("data-auth-provider=\"github\""));
    assert_eq!(
        get(&f, "/auth/github", &mut visitor).await.status,
        StatusCode::NOT_FOUND,
        "a provider without credentials cannot start"
    );

    // Start authorization: carries state, nonce, PKCE and this site's callback URL.
    let params = start(&f, TEST_SITE_URL, "/auth/google", &mut visitor).await;
    assert_eq!(params["client_id"], GOOGLE_ID);
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["scope"], "openid email profile");
    // Mismatched state, or no cookie from starting authorization: both refused, and the cookie is voided.
    let forged = get(
        &f,
        &format!(
            "/auth/google/callback?state=forged&code={}",
            google_code("g-1", None, false, &params["nonce"])
        ),
        &mut visitor,
    )
    .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN, "{}", forged.html);
    let replay = get(
        &f,
        &format!(
            "/auth/google/callback?state={}&code={}",
            params["state"],
            google_code("g-1", None, false, &params["nonce"])
        ),
        &mut visitor,
    )
    .await;
    assert_eq!(
        replay.status,
        StatusCode::FORBIDDEN,
        "one authorization is used only once"
    );
    // Mismatched nonce: what the provider signed is not for this attempt, so it counts as authorization not completed.
    let wrong_nonce = authorize(&f, TEST_SITE_URL, "google", "", &mut visitor, |_| {
        google_code("g-1", Some("first@example.test"), true, "other-nonce")
    })
    .await;
    assert_eq!(wrong_nonce.status, StatusCode::BAD_GATEWAY);
    assert!(
        wrong_nonce
            .html
            .contains("Google authorization did not complete")
    );
    // The member cancels on Google's side.
    let params = start(&f, TEST_SITE_URL, "/auth/google", &mut visitor).await;
    let denied = get(
        &f,
        &format!(
            "/auth/google/callback?state={}&error=access_denied",
            params["state"]
        ),
        &mut visitor,
    )
    .await;
    assert!(
        denied.html.contains("Google authorization was cancelled"),
        "{}",
        denied.html
    );

    // Never bound before: go complete registration. The email was verified by Google, so it is used directly and
    // counted as verified, and the username is prefilled.
    authorize(&f, TEST_SITE_URL, "google", "", &mut visitor, |nonce| {
        google_code("g-1", Some("first@example.test"), true, nonce)
    })
    .await
    .redirect("/register/external");
    let register = get(&f, "/register/external", &mut visitor).await;
    assert_eq!(register.status, StatusCode::OK, "{}", register.html);
    assert!(register.html.contains("data-verified-email"));
    assert!(register.html.contains("first@example.test"));
    assert!(
        register.html.contains("value=\"googlemember\""),
        "the prefilled username drops the space: {}",
        register.html
    );
    assert!(!register.html.contains("name=\"email\""));
    let no_terms = post(
        &f,
        "/register/external",
        &mut visitor,
        &[("username", "googlemember")],
    )
    .await;
    assert_eq!(no_terms.status, StatusCode::UNPROCESSABLE_ENTITY);
    post(
        &f,
        "/register/external",
        &mut visitor,
        &[
            ("username", "googlemember"),
            ("accept_terms", "1"),
            ("email", "ignored@example.test"),
        ],
    )
    .await
    .redirect("/");
    assert!(
        !visitor.0.contains_key("gonof_external"),
        "the pending identity is used up"
    );
    let (uid, email, emailstatus, password) = member_row(&f, "googlemember").await;
    assert_eq!(email.as_deref(), Some("first@example.test"));
    assert_eq!(emailstatus, 1);
    assert!(password.is_none());
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid} AND provider='google' AND subject='g-1' AND display='first@example.test' AND last_login_at=0"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid={uid} AND purpose='email_verification'"
        ))
        .await,
        0,
        "an email verified by the provider gets no verification mail"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.register' AND data LIKE '%\"method\":\"google\"%'"
        ))
        .await,
        1
    );
    let home = get(&f, "/", &mut visitor).await;
    assert!(
        !home.html.contains("banner__actions"),
        "the email is verified, so the member can take part"
    );

    // Signing in again with the same Google account: logs in directly and records the last login time.
    let mut again = Cookies::default();
    authorize(&f, TEST_SITE_URL, "google", "", &mut again, |nonce| {
        google_code("g-1", Some("first@example.test"), true, nonce)
    })
    .await
    .redirect("/");
    assert!(again.0.contains_key("gonof_session"));
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid} AND last_login_at>0"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.login' AND data LIKE '%\"method\":\"google\"%'"
        ))
        .await,
        1
    );
    // Going to login while already signed in: back to the home page.
    assert_eq!(
        get(&f, "/auth/google", &mut again).await.status,
        StatusCode::SEE_OTHER
    );

    // Another Google account carrying an already registered email: not merged.
    let mut other = Cookies::default();
    let taken = authorize(&f, TEST_SITE_URL, "google", "", &mut other, |nonce| {
        google_code("g-2", Some("FIRST@example.test"), true, nonce)
    })
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT, "{}", taken.html);
    assert!(taken.html.contains(Lang::EN.account_error_email_taken()));

    // Registration closed: unbound accounts cannot register, bound ones sign in as usual.
    set_setting(&f.db, "settings.signup.mode", "closed")
        .await
        .unwrap();
    f.services.global.clear_local();
    let closed = authorize(&f, TEST_SITE_URL, "google", "", &mut other, |nonce| {
        google_code("g-3", None, false, nonce)
    })
    .await;
    assert!(
        closed.html.contains("sign-ups are closed for now"),
        "{}",
        closed.html
    );
    authorize(
        &f,
        TEST_SITE_URL,
        "google",
        "",
        &mut Cookies::default(),
        |nonce| google_code("g-1", None, false, nonce),
    )
    .await
    .redirect("/");
    // A frozen account cannot sign in through a third party.
    f.execute(&format!(
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid={uid}"
    ))
    .await;
    let frozen = authorize(
        &f,
        TEST_SITE_URL,
        "google",
        "",
        &mut Cookies::default(),
        |nonce| google_code("g-1", None, false, nonce),
    )
    .await;
    assert_eq!(frozen.status, StatusCode::UNAUTHORIZED, "{}", frozen.html);
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn github_only_accounts_participate_by_binding_and_set_a_password() {
    let f = Fixture::new().await;
    f.configure("github", GITHUB_ID, GITHUB_SECRET).await;
    // The GitHub App has no email permission: completing registration can optionally take an email, and registering
    // without one also works.
    let mut member = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut member, |_| {
        github_code(4242, "octo-gono", None)
    })
    .await
    .redirect("/register/external");
    let register = get(&f, "/register/external", &mut member).await;
    assert!(
        register.html.contains("name=\"email\""),
        "{}",
        register.html
    );
    assert!(register.html.contains("value=\"octo-gono\""));
    post(
        &f,
        "/register/external",
        &mut member,
        &[("username", "octo-gono"), ("accept_terms", "1")],
    )
    .await
    .redirect("/");
    let (uid, email, emailstatus, password) = member_row(&f, "octo-gono").await;
    assert!(email.is_none() && emailstatus == 0 && password.is_none());

    // The site-wide participation requirement only accepts email: prompt to verify; after the admin area changes it to
    // "email or GitHub" the member can participate, and write transactions judge by it too.
    let home = get(&f, "/", &mut member).await;
    assert!(home.html.contains("banner__actions"), "{}", home.html);
    set_setting(&f.db, "settings.login.participation", "email,github")
        .await
        .unwrap();
    f.services.global.clear_local();
    let home = get(&f, "/", &mut member).await;
    assert!(
        !home.html.contains("banner__actions"),
        "a linked GitHub account is enough to take part"
    );
    assert_eq!(
        get(&f, "/my/invites", &mut member).await.status,
        StatusCode::OK
    );
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let topic = || NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid: 1,
        title: "Third-party participation".into(),
        body: "Body text".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    };
    let site = |set: &str| gono_forum_web::site::Site {
        participation: Participation::parse(set),
        ..Default::default()
    };
    let blocked =
        gono_forum_web::site::scope(site("email,google"), writer.publish(uid, topic())).await;
    assert!(
        matches!(
            blocked,
            Err(ContentWriteError::Forbidden(
                "Please verify your email or link a Google account first"
            ))
        ),
        "{blocked:?}"
    );
    gono_forum_web::site::scope(site("email,github"), writer.publish(uid, topic()))
        .await
        .unwrap();

    // No password: binding and unbinding both require setting a password first; set the password after confirming
    // identity with GitHub.
    let account = get(&f, "/my/account", &mut member).await;
    assert!(
        account.html.contains("data-set-password"),
        "{}",
        account.html
    );
    assert!(
        account
            .html
            .contains("data-external-status=\"github-bound\"")
    );
    assert!(account.html.contains("data-external-confirm=\"github\""));
    assert!(!account.html.contains("/my/account/external/unlink"));
    // Confirming with a different GitHub account does not work.
    let wrong = authorize(&f, TEST_SITE_URL, "github", "confirm", &mut member, |_| {
        github_code(9999, "someone-else", None)
    })
    .await;
    assert!(
        wrong
            .html
            .contains("The authorized GitHub account is not the one linked to this account"),
        "{}",
        wrong.html
    );
    authorize(&f, TEST_SITE_URL, "github", "confirm", &mut member, |_| {
        github_code(4242, "octo-gono", None)
    })
    .await
    .redirect("/my/account#password");
    let confirmed = get(&f, "/my/account", &mut member).await;
    assert!(
        confirmed.html.contains("data-external-confirmed"),
        "{}",
        confirmed.html
    );
    let mismatch = post(
        &f,
        "/my/account/password/external",
        &mut member,
        &[("password", PASSWORD), ("password2", "different")],
    )
    .await;
    assert!(mismatch.html.contains("The two passwords do not match"));
    post(
        &f,
        "/my/account/password/external",
        &mut member,
        &[("password", PASSWORD), ("password2", PASSWORD)],
    )
    .await
    .redirect("/login?password_set=true");
    assert!(
        !member.0.contains_key("gonof_session"),
        "setting the password signs out every device"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.password.set'"
        ))
        .await,
        1
    );

    // A password but no email or mobile number: GitHub is the only recovery path, so unbinding is not allowed.
    let grant = f.identity.login("octo-gono", PASSWORD).await.unwrap();
    let mut member = Cookies::session(&grant.session_token);
    let refused = post(
        &f,
        "/my/account/external/unlink",
        &mut member,
        &[("provider", "github"), ("current_password", PASSWORD)],
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.html
    );
    assert!(
        refused
            .html
            .contains("after unlinking you could not recover the password"),
        "{}",
        refused.html
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid}"
        ))
        .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn password_members_link_and_unlink_github() {
    let f = Fixture::new().await;
    f.configure("github", GITHUB_ID, GITHUB_SECRET).await;
    let grant = f
        .password_member("password-member", "pw-member@example.test")
        .await;
    let uid = grant.member.uid;
    let mut member = Cookies::session(&grant.session_token);
    let account = get(&f, "/my/account", &mut member).await;
    assert!(
        account
            .html
            .contains("data-external-status=\"github-none\""),
        "{}",
        account.html
    );
    assert!(account.html.contains("data-external-link=\"github\""));
    assert!(
        !account.html.contains("data-external-status=\"google"),
        "Google is not listed when it is neither enabled nor linked"
    );

    // Binding: after authorization comes back it waits for confirmation, and is only written once the current password
    // is entered correctly.
    authorize(&f, TEST_SITE_URL, "github", "link", &mut member, |_| {
        github_code(777, "pw-octo", Some("pw-octo@example.test"))
    })
    .await
    .redirect("/my/account#external");
    let pending = get(&f, "/my/account", &mut member).await;
    assert!(
        pending.html.contains("data-external-pending"),
        "{}",
        pending.html
    );
    assert!(pending.html.contains("pw-octo"));
    let wrong = post(
        &f,
        "/my/account/external/link",
        &mut member,
        &[("current_password", "wrong-password-0000")],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        wrong.html.contains(Lang::EN.account_error_wrong_password()),
        "{}",
        wrong.html
    );
    post(
        &f,
        "/my/account/external/link",
        &mut member,
        &[("current_password", PASSWORD)],
    )
    .await
    .redirect("/my/account?external_linked=github#external");
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid} AND provider='github' AND subject='777' AND display='pw-octo'"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.external.link'"
        ))
        .await,
        1
    );
    let linked = get(&f, "/my/account?external_linked=github", &mut member).await;
    assert!(
        linked
            .html
            .contains("Your GitHub account is linked; you can log in with it from now on.")
    );
    assert!(
        linked
            .html
            .contains("data-external-status=\"github-bound\"")
    );

    // Binding again when already bound: the current account already has GitHub. Another account binding the same
    // GitHub: already bound to another account.
    let again = authorize(&f, TEST_SITE_URL, "github", "link", &mut member, |_| {
        github_code(778, "pw-octo-2", None)
    })
    .await;
    assert!(
        again
            .html
            .contains("This account already has a GitHub account linked"),
        "{}",
        again.html
    );
    let other = f
        .password_member("other-member", "other-member@example.test")
        .await;
    let mut other = Cookies::session(&other.session_token);
    let duplicate = authorize(&f, TEST_SITE_URL, "github", "link", &mut other, |_| {
        github_code(777, "pw-octo", None)
    })
    .await;
    assert!(
        duplicate
            .html
            .contains("This GitHub account is linked to another account"),
        "{}",
        duplicate.html
    );

    // Signing in with GitHub lands on this member.
    let mut fresh = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut fresh, |_| {
        github_code(777, "pw-octo", None)
    })
    .await
    .redirect("/");
    let session = f
        .identity
        .session(&fresh.0["gonof_session"])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.member.uid, uid);

    // Unbinding: with an email the password can be recovered, so it is allowed; afterwards signing in with the same
    // GitHub counts as a new registration.
    post(
        &f,
        "/my/account/external/unlink",
        &mut member,
        &[("provider", "github"), ("current_password", PASSWORD)],
    )
    .await
    .redirect("/my/account?external_unlinked=github#external");
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid}"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.external.unlink'"
        ))
        .await,
        1
    );
    authorize(
        &f,
        TEST_SITE_URL,
        "github",
        "",
        &mut Cookies::default(),
        |_| github_code(777, "pw-octo", None),
    )
    .await
    .redirect("/register/external");
    // Binding and unbinding both queued security alert emails (the email is verified).
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE aggregate_id={uid} AND event_type='security_notice'"
        ))
        .await,
        2
    );
    f.services.cleanup().await.unwrap();
}

/// The raw token of the newest password reset mail queued for this member.
async fn reset_token(fixture: &Fixture, uid: i64) -> String {
    let payload: String = db::query_scalar(
        "SELECT payload FROM gonof_common_outbox WHERE event_type='password_reset' AND aggregate_id=? ORDER BY eventid DESC LIMIT 1",
    )
    .bind(uid)
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    serde_json::from_str::<serde_json::Value>(&payload).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The owner of an address asks for a reset link and uses it; returns the new password.
async fn recover(fixture: &Fixture, uid: i64, email: &str, claims: bool) -> &'static str {
    let mut owner = Cookies::default();
    get(fixture, "/forgot-password", &mut owner).await;
    post(fixture, "/forgot-password", &mut owner, &[("email", email)])
        .await
        .redirect("/forgot-password?sent=true");
    let token = reset_token(fixture, uid).await;
    let page = get(
        fixture,
        &format!("/reset-password?token={token}"),
        &mut owner,
    )
    .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.html);
    assert_eq!(
        page.html.contains("data-reset-claims"),
        claims,
        "{}",
        page.html
    );
    let new_password = "Owner-recovered-password-2026!";
    post(
        fixture,
        "/reset-password",
        &mut owner,
        &[
            ("token", &token),
            ("password", new_password),
            ("password2", new_password),
        ],
    )
    .await
    .redirect("/login?password_reset=true");
    new_password
}

/// Account pre-hijacking: someone signs up with another person's address, which they cannot verify, and sets up their
/// own ways to sign in. An unverified address allows none to be added; and when the address's owner takes the account
/// over through password recovery, nothing set up before (here a GitHub account linked at sign-up, plus a phone
/// number and two-step verification as an older site may hold them) still opens it.
#[tokio::test]
async fn recovering_an_unverified_account_shuts_out_whoever_set_it_up() {
    let f = Fixture::new().await;
    f.configure("github", GITHUB_ID, GITHUB_SECRET).await;

    // Signing up with a password and someone else's address: nothing more can be added until it is verified.
    let mut squatter = Cookies::default();
    get(&f, "/register", &mut squatter).await;
    post(
        &f,
        "/register",
        &mut squatter,
        &[
            ("username", "squatter"),
            ("email", "owner@example.test"),
            ("password", PASSWORD),
            ("password2", PASSWORD),
            ("accept_terms", "1"),
        ],
    )
    .await
    .redirect("/verify-email");
    let (uid, _, emailstatus, _) = member_row(&f, "squatter").await;
    assert_eq!(emailstatus, 0, "the address was never verified");
    let account = get(&f, "/my/account", &mut squatter).await;
    for section in ["external", "mobile", "two-factor"] {
        assert!(
            account
                .html
                .contains(&format!("data-verify-email-first=\"{section}\"")),
            "{section}: {}",
            account.html
        );
    }
    assert!(!account.html.contains("data-external-link="));
    assert!(!account.html.contains("/my/account/mobile/send"));
    assert!(!account.html.contains("/my/account/two-factor/setup"));
    let refused = request(
        &f,
        TEST_SITE_URL,
        "GET",
        "/auth/github?purpose=link",
        &mut squatter,
        &[],
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.html);
    assert!(
        refused
            .html
            .contains(Lang::EN.account_error_verify_email_first())
    );
    assert!(
        !squatter.0.contains_key("gonof_oauth"),
        "the provider is not even asked"
    );
    let two_factor = post(
        &f,
        "/my/account/two-factor/setup",
        &mut squatter,
        &[("current_password", PASSWORD)],
    )
    .await;
    assert_eq!(
        two_factor.status,
        StatusCode::CONFLICT,
        "{}",
        two_factor.html
    );
    assert!(
        two_factor
            .html
            .contains(Lang::EN.account_error_verify_email_first())
    );
    // The confirmation under the member row lock refuses too, even with a pending link from before the rule.
    let pending = f
        .identity
        .prepare_link(
            &f.identity
                .session(&squatter.0["gonof_session"])
                .await
                .unwrap()
                .unwrap()
                .member,
            gono_forum_web::oauth::Provider::Github,
            gono_forum_web::oauth::Profile {
                subject: "6060".into(),
                display: "squatter-octo".into(),
                email: None,
                suggestion: "squatter-octo".into(),
            },
        )
        .await;
    assert!(matches!(pending, Err(identity::IdentityError::Conflict(_))));
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={uid}"
        ))
        .await,
        0
    );

    // A GitHub account linked at sign-up still comes with an address typed in by hand, and a password can be set
    // after confirming with GitHub. A phone number and two-step verification are written directly: an older site may
    // hold them on an unverified account.
    let mut other = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut other, |_| {
        github_code(6060, "squatter-octo", None)
    })
    .await
    .redirect("/register/external");
    post(
        &f,
        "/register/external",
        &mut other,
        &[
            ("username", "squatter2"),
            ("email", "second-owner@example.test"),
            ("accept_terms", "1"),
        ],
    )
    .await
    .redirect("/verify-email");
    let (uid, _, emailstatus, _) = member_row(&f, "squatter2").await;
    assert_eq!(emailstatus, 0);
    authorize(&f, TEST_SITE_URL, "github", "confirm", &mut other, |_| {
        github_code(6060, "squatter-octo", None)
    })
    .await
    .redirect("/my/account#password");
    post(
        &f,
        "/my/account/password/external",
        &mut other,
        &[("password", PASSWORD), ("password2", PASSWORD)],
    )
    .await
    .redirect("/login?password_set=true");
    let mut squatter = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut squatter, |_| {
        github_code(6060, "squatter-octo", None)
    })
    .await
    .redirect("/");
    assert!(squatter.0.contains_key("gonof_session"));
    for sql in [
        format!("UPDATE gonof_common_member SET mobile='+8613800006060' WHERE uid={uid}"),
        format!(
            "INSERT INTO gonof_common_member_two_factor(uid,totp_secret,totp_step,totp_enabled_at,enabled_at) VALUES({uid},'unreadable',0,1,1)"
        ),
        format!(
            "INSERT INTO gonof_common_member_recovery_code(uid,code_hash,dateline) VALUES({uid},'{}',1)",
            "ab".repeat(32)
        ),
    ] {
        f.execute(&sql).await;
    }
    let squatter_session = f
        .identity
        .login_with_metadata("squatter2", PASSWORD, &Default::default())
        .await;
    assert!(squatter_session.is_err(), "two-step verification is on");
    // GitHub sign-in now goes on to the second step, which the squatter passes with their authenticator.
    authorize(
        &f,
        TEST_SITE_URL,
        "github",
        "",
        &mut Cookies::default(),
        |_| github_code(6060, "squatter-octo", None),
    )
    .await
    .redirect("/login/verify");

    // The address's owner recovers the account with the link mailed to them; the reset page says what it will do.
    let new_password = recover(&f, uid, "second-owner@example.test", true).await;
    assert!(
        f.identity
            .session(&squatter.0["gonof_session"])
            .await
            .unwrap()
            .is_none()
    );
    let mut again = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut again, |_| {
        github_code(6060, "squatter-octo", None)
    })
    .await
    .redirect("/register/external");
    assert!(!again.0.contains_key("gonof_session"));
    let row = db::query("SELECT emailstatus, mobile FROM gonof_common_member WHERE uid=?")
        .bind(uid)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(row.try_get::<i64, _>("emailstatus").unwrap(), 1);
    assert!(
        row.try_get::<Option<String>, _>("mobile")
            .unwrap()
            .is_none()
    );
    for (table, rows) in [
        ("gonof_common_member_external", 0),
        ("gonof_common_member_two_factor", 0),
        ("gonof_common_member_recovery_code", 0),
    ] {
        assert_eq!(
            f.scalar(&format!("SELECT COUNT(*) FROM {table} WHERE uid={uid}"))
                .await,
            rows,
            "{table}"
        );
    }
    for (kind, data) in [
        ("identity.email.verify", "\"method\":\"password_reset\""),
        (
            "identity.external.unlink",
            "\"cause\":\"identity.password.reset\"",
        ),
        (
            "identity.mobile.unbind",
            "\"cause\":\"identity.password.reset\"",
        ),
        (
            "identity.two_factor.disable",
            "\"cause\":\"identity.password.reset\"",
        ),
    ] {
        assert_eq!(
            f.scalar(&format!(
                "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='{kind}' AND data LIKE '%{data}%'"
            ))
            .await,
            1,
            "{kind}"
        );
    }
    // The owner signs in with the new password alone, and can now link their own accounts.
    let grant = f.identity.login("squatter2", new_password).await.unwrap();
    let mut owner = Cookies::session(&grant.session_token);
    let account = get(&f, "/my/account", &mut owner).await;
    assert!(
        account.html.contains("data-external-link=\"github\""),
        "{}",
        account.html
    );
    assert!(!account.html.contains("data-verify-email-first"));
    f.services.cleanup().await.unwrap();
}

/// Recovering an account whose email is verified changes nothing but the password, sessions and tokens: its own links
/// keep working.
#[tokio::test]
async fn recovering_a_verified_account_keeps_its_links() {
    let f = Fixture::new().await;
    f.configure("github", GITHUB_ID, GITHUB_SECRET).await;
    let grant = f
        .password_member("verified-member", "verified-member@example.test")
        .await;
    let uid = grant.member.uid;
    let mut member = Cookies::session(&grant.session_token);
    authorize(&f, TEST_SITE_URL, "github", "link", &mut member, |_| {
        github_code(5050, "verified-octo", None)
    })
    .await
    .redirect("/my/account#external");
    post(
        &f,
        "/my/account/external/link",
        &mut member,
        &[("current_password", PASSWORD)],
    )
    .await
    .redirect("/my/account?external_linked=github#external");
    recover(&f, uid, "verified-member@example.test", false).await;
    assert!(
        f.identity
            .session(&grant.session_token)
            .await
            .unwrap()
            .is_none()
    );
    let mut again = Cookies::default();
    authorize(&f, TEST_SITE_URL, "github", "", &mut again, |_| {
        github_code(5050, "verified-octo", None)
    })
    .await
    .redirect("/");
    assert_eq!(
        f.identity
            .session(&again.0["gonof_session"])
            .await
            .unwrap()
            .unwrap()
            .member
            .uid,
        uid
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn other_domains_get_their_own_callbacks() {
    let f = Fixture::new().await;
    f.configure("google", GOOGLE_ID, GOOGLE_SECRET).await;
    f.configure("github", GITHUB_ID, GITHUB_SECRET).await;
    // The main domain follows the site-wide setting (google,github); demo2 (with a sub-path) only enables GitHub, and
    // its participation requirement only accepts GitHub.
    for sql in [
        format!("INSERT INTO gonof_common_site_domain(site_url,dateline) VALUES('{TEST_SITE_URL}',1)"),
        format!(
            "INSERT INTO gonof_common_site_domain(site_url,site_name,login_methods,participation,dateline) VALUES('{DEMO2}','Feedback Site','github','github',1)"
        ),
        "INSERT INTO gonof_common_site_domain_forum(domain_id,fid) SELECT d.domain_id,f.fid FROM gonof_common_site_domain d JOIN gonof_forum_forum f ON f.parent_fid IS NULL".to_owned(),
    ] {
        f.execute(&sql).await;
    }
    set_setting(&f.db, "settings.domains.enabled", "true")
        .await
        .unwrap();
    f.services.global.clear_local();
    let mut admin = Cookies::session(&f.admin.session_token);
    let page = get(&f, "/manage/login", &mut admin).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.html);
    for url in [
        format!("{TEST_SITE_URL}/auth/google/callback"),
        format!("{TEST_SITE_URL}/auth/github/callback"),
        format!("{DEMO2}/auth/github/callback"),
    ] {
        assert!(page.html.contains(&url), "{url}\n{}", page.html);
    }
    assert!(!page.html.contains(&format!("{DEMO2}/auth/google/callback")));

    // demo2's login page only has GitHub; Google cannot be started there.
    let login = request(
        &f,
        DEMO2,
        "GET",
        "/feedback/login",
        &mut Cookies::default(),
        &[],
    )
    .await;
    assert!(
        login.html.contains("data-auth-provider=\"github\""),
        "{}",
        login.html
    );
    assert!(!login.html.contains("data-auth-provider=\"google\""));
    let google = request(
        &f,
        DEMO2,
        "GET",
        "/feedback/auth/google",
        &mut Cookies::default(),
        &[],
    )
    .await;
    assert_eq!(google.status, StatusCode::NOT_FOUND);

    // Registering with GitHub on demo2: the callback URL carries the sub-path, and redirects carry the prefix too.
    let mut member = Cookies::default();
    let callback = authorize(&f, DEMO2, "github", "", &mut member, |_| {
        github_code(31337, "feedback-octo", Some("feedback@example.test"))
    })
    .await;
    callback.redirect("/feedback/register/external");
    let page = request(
        &f,
        DEMO2,
        "GET",
        "/feedback/register/external",
        &mut member,
        &[],
    )
    .await;
    assert!(page.html.contains("feedback@example.test"), "{}", page.html);
    f.services.cleanup().await.unwrap();
}
