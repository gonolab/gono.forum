use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

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
    i18n::Lang,
    identity::{self, Identity, SessionGrant},
    router, urls,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const PASSWORD: &str = "Profile-contract-password-2026!";

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
    async fn new(configure: impl FnOnce(&mut Config)) -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let mut config: Config = services.config.clone();
        configure(&mut config);
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
            .generate_invite(admin.member.uid, "profile-member")
            .await
            .unwrap();
        let member = identity
            .register(
                "profilemember",
                "profile@example.test",
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

    async fn token(&self, event: &str) -> String {
        let row = gono_forum_web::db::query("SELECT payload FROM gonof_common_outbox WHERE event_type=? AND aggregate_id=? ORDER BY eventid DESC LIMIT 1").bind(event).bind(self.member.member.uid).fetch_one(self.db.executor()).await.unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(&db::text(&row, "payload").unwrap()).unwrap();
        payload["token"].as_str().unwrap().into()
    }

    async fn avatar_aid(&self) -> Option<i64> {
        gono_forum_web::db::query_scalar(
            "SELECT avatar_aid FROM gonof_common_member WHERE username='profilemember'",
        )
        .fetch_one(self.db.executor())
        .await
        .unwrap()
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
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.33:48321".parse::<SocketAddr>().unwrap(),
    ));
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
async fn profile_updates_preserve_plain_text_and_keep_the_uid_addressed_space_stable() {
    let fixture = Fixture::new(|_| {}).await;
    let mut anonymous = Cookies::default();
    let space = urls::space(fixture.member.member.uid);
    assert!(
        get(&fixture, &space, &mut anonymous)
            .await
            .html
            .contains("profilemember")
    );
    get(&fixture, "/my/profile", &mut anonymous)
        .await
        .redirect("/login?return_to=%2Fmy%2Fprofile");
    let mut cookies = Cookies::member(&fixture);
    let page = get(&fixture, "/my/profile", &mut cookies).await;
    page.status(StatusCode::OK);
    assert!(page.html.contains("Verify your email to upload an avatar"));
    let bio = "First line\r\n<script>alert(1)</script> **plain text**";
    // The full-width form of "NewProfile": usernames are NFKC-normalized and lowercased, so it becomes "newprofile".
    post(
        &fixture,
        "/my/profile",
        &mut cookies,
        &[
            (
                "username",
                "\u{ff2e}\u{ff45}\u{ff57}\u{ff30}\u{ff52}\u{ff4f}\u{ff46}\u{ff49}\u{ff4c}\u{ff45}",
            ),
            ("bio", bio),
        ],
    )
    .await
    .redirect("/my/profile?saved=true");
    assert!(
        fixture
            .identity
            .login("profilemember", PASSWORD)
            .await
            .is_err()
    );
    assert!(fixture.identity.login("newprofile", PASSWORD).await.is_ok());
    // Personal spaces are addressed by uid: renaming creates no new URL, and there is no old URL to redirect.
    let public = get(&fixture, &space, &mut anonymous).await;
    public.status(StatusCode::OK);
    assert!(public.html.contains("newprofile"));
    assert!(
        public
            .html
            .contains("&#60;script&#62;alert(1)&#60;/script&#62; **plain text**")
    );
    assert!(!public.html.contains("<script>alert(1)</script>"));
    assert!(!public.html.contains("profile@example.test"));
    assert!(!public.html.contains("profilemember"));
    let own = get(&fixture, "/my/profile", &mut cookies).await;
    assert!(own.html.contains("Next change possible"));
    assert!(own.html.contains("readonly"));
    let stale = post(
        &fixture,
        "/my/profile",
        &mut cookies,
        &[
            ("username", "profilemember"),
            ("bio", "Unsaved bio from a stale page"),
        ],
    )
    .await;
    stale.status(StatusCode::CONFLICT);
    assert!(stale.html.contains("Unsaved bio from a stale page"));
    assert!(stale.html.contains("value=\"newprofile\""));
    post(
        &fixture,
        "/my/profile",
        &mut cookies,
        &[
            ("username", "newprofile"),
            ("bio", "Changing only the bio ignores the username cooldown"),
        ],
    )
    .await
    .redirect("/my/profile?saved=true");
    let long = "x".repeat(501);
    post(
        &fixture,
        "/my/profile",
        &mut cookies,
        &[("username", "newprofile"), ("bio", &long)],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        fixture
            .identity
            .profile(&fixture.member.session_token)
            .await
            .unwrap()
            .bio,
        "Changing only the bio ignores the username cooldown"
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn email_change_requires_current_password_and_throttles_repeated_applications() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    let wrong = post(
        &fixture,
        "/my/profile/email",
        &mut cookies,
        &[
            ("email", "new@example.test"),
            ("current_password", "Wrong-profile-password-2026"),
        ],
    )
    .await;
    wrong.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.html.contains(Lang::EN.account_error_wrong_password()));
    assert!(!wrong.html.contains("Wrong-profile-password-2026"));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE purpose='email_change'")
            .await,
        0
    );
    post(
        &fixture,
        "/my/profile/email",
        &mut cookies,
        &[
            ("email", "new@example.test"),
            ("current_password", PASSWORD),
        ],
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
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn email_confirmation_is_read_only_until_post_then_switches_email_and_revokes_sessions() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    let other = fixture
        .identity
        .login("profilemember", PASSWORD)
        .await
        .unwrap();
    post(
        &fixture,
        "/my/profile/email",
        &mut cookies,
        &[
            ("email", "NewProfileEmail@Example.test"),
            ("current_password", PASSWORD),
        ],
    )
    .await
    .redirect("/my/profile?email_sent=true");
    let pending = get(&fixture, "/my/profile", &mut cookies).await;
    assert!(pending.html.contains("Current email: profile@example.test"));
    assert!(pending.html.contains("newprofileemail@example.test"));
    pending.no_password();
    let token = fixture.token("email_change").await;
    let mut anonymous = Cookies::default();
    for _ in 0..2 {
        let page = get(
            &fixture,
            &format!("/confirm-email-change?token={token}"),
            &mut anonymous,
        )
        .await;
        page.status(StatusCode::OK);
        assert_eq!(page.headers[header::REFERRER_POLICY], "strict-origin");
        assert!(page.html.contains("Confirm the new email"));
        assert_eq!(
            fixture
                .identity
                .session(&fixture.member.session_token)
                .await
                .unwrap()
                .unwrap()
                .member
                .email
                .as_deref(),
            Some("profile@example.test")
        );
    }
    let csrf = anonymous.csrf(&fixture.identity).await;
    request(
        &fixture,
        "POST",
        "/confirm-email-change",
        &mut anonymous,
        &[("_csrf", &csrf), ("token", &token)],
        &[("Origin", "null")],
    )
    .await
    .status(StatusCode::FORBIDDEN);
    assert!(
        fixture
            .identity
            .email_change_status(&token)
            .await
            .unwrap()
            .is_some()
    );
    post(
        &fixture,
        "/confirm-email-change",
        &mut anonymous,
        &[("token", &token)],
    )
    .await
    .redirect("/login?email_changed=true");
    assert!(!anonymous.0.contains_key("gonof_session"));
    assert!(!anonymous.0.contains_key("gonof_csrf"));
    for sid in [&fixture.member.session_token, &other.session_token] {
        assert!(fixture.identity.session(sid).await.unwrap().is_none());
    }
    assert!(
        fixture
            .identity
            .login("profile@example.test", PASSWORD)
            .await
            .is_err()
    );
    let logged = fixture
        .identity
        .login("newprofileemail@example.test", PASSWORD)
        .await
        .unwrap();
    assert!(logged.member.email_verified);
    get(
        &fixture,
        &format!("/confirm-email-change?token={token}"),
        &mut anonymous,
    )
    .await
    .status(StatusCode::BAD_REQUEST);
    post(
        &fixture,
        "/confirm-email-change",
        &mut anonymous,
        &[("token", &token)],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    let login = get(&fixture, "/login?email_changed=true", &mut anonymous).await;
    assert!(
        login
            .html
            .contains("Your email has changed and all devices are logged out")
    );
    let mut fresh = Cookies::session(&logged.session_token);
    assert!(
        !get(&fixture, "/my/profile", &mut fresh)
            .await
            .html
            .contains("data-pending-email")
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn every_profile_write_rejects_cross_site_requests_and_invalid_csrf() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    let csrf = cookies.csrf(&fixture.identity).await;
    for path in [
        "/my/profile",
        "/my/profile/email",
        "/confirm-email-change",
        "/my/profile/avatar/42",
        "/my/profile/avatar/remove",
    ] {
        let fields = [
            ("username", "profilemember"),
            ("bio", "unsafe attempt"),
            ("current_password", PASSWORD),
            ("email", "unsafe@example.test"),
        ];
        request(&fixture, "POST", path, &mut cookies, &fields, &[])
            .await
            .status(StatusCode::FORBIDDEN);
        let mut with_csrf = fields.to_vec();
        with_csrf.push(("_csrf", &csrf));
        for extra in [
            [("Origin", "https://attacker.example")],
            [("Sec-Fetch-Site", "cross-site")],
        ] {
            let response = request(&fixture, "POST", path, &mut cookies, &with_csrf, &extra).await;
            response.status(StatusCode::FORBIDDEN);
            response.no_password();
        }
    }
    assert_eq!(
        fixture
            .identity
            .profile(&fixture.member.session_token)
            .await
            .unwrap()
            .bio,
        ""
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE purpose='email_change'")
            .await,
        0
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

const BOUNDARY: &str = "profile-http-boundary";

/// One multipart field; when a filename is given it is written as an image file.
fn part(name: &str, filename: Option<&str>, value: &[u8]) -> Vec<u8> {
    let mut bytes = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"");
    if let Some(filename) = filename {
        bytes.push_str(&format!(
            "; filename=\"{filename}\"\r\nContent-Type: image/png"
        ));
    }
    bytes.push_str("\r\n\r\n");
    let mut bytes = bytes.into_bytes();
    bytes.extend(value);
    bytes.extend(b"\r\n");
    bytes
}

async fn multipart(
    fixture: &Fixture,
    cookies: &mut Cookies,
    csrf: &str,
    key: &str,
    filename: &str,
    bytes: &[u8],
) -> Response {
    let key = identity::token_hash(key);
    let parts = [
        part("_csrf", None, csrf.as_bytes()),
        part("request_key", None, key.as_bytes()),
        part("file", Some(filename), bytes),
    ];
    upload_avatar(fixture, cookies, &parts, &[]).await
}

/// Submits the avatar form in the given order; an Origin in `extra` replaces the default same-origin Origin.
async fn upload_avatar(
    fixture: &Fixture,
    cookies: &mut Cookies,
    parts: &[Vec<u8>],
    extra: &[(&str, &str)],
) -> Response {
    upload_to(fixture, "/my/profile/avatar", cookies, parts, extra).await
}

/// Submits multipart to any upload URL in the given order; `extra` follows the same rules as [`upload_avatar`].
async fn upload_to(
    fixture: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    parts: &[Vec<u8>],
    extra: &[(&str, &str)],
) -> Response {
    let mut body = parts.concat();
    body.extend(format!("--{BOUNDARY}--\r\n").as_bytes());
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
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
        .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
    if !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")) {
        builder = builder.header(header::ORIGIN, ORIGIN);
    }
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Body::from(body)).unwrap();
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

/// Configures an unreachable storage endpoint: registering an upload only records the attachment, and processing is
/// left to the media task, which does not run in tests.
async fn configure_storage(fixture: &Fixture) {
    use gono_forum_web::media::{Media, StorageInput};
    let config: Config = fixture._services.config.clone();
    let media = Media::new(fixture.db.clone(), config.media, fixture.root.clone());
    media
        .save_settings_actor(
            fixture.admin.member.actor(),
            StorageInput {
                revision: 0,
                endpoint: "http://127.0.0.1:9".into(),
                region: "us-east-1".into(),
                bucket: "profile-http".into(),
                prefix: "profile-http/".into(),
                path_style: true,
                access_key: "test-only-key".into(),
                secret_key: "test-only-secret".into(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn avatar_upload_checks_origin_and_token_before_spending_quota_or_staging() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    fixture
        .identity
        .verify_email(&fixture.token("email_verification").await)
        .await
        .unwrap();
    configure_storage(&fixture).await;
    let csrf = cookies.csrf(&fixture.identity).await;
    let staging = fixture
        .root
        .join(&fixture._services.config.media.local_directory)
        .join("staging");
    let key = identity::token_hash("avatar-order");
    let image = vec![42_u8; 4096];
    let token = part("_csrf", None, csrf.as_bytes());
    let request_key = part("request_key", None, key.as_bytes());
    let file = part("file", Some("avatar.png"), &image);
    // Avatars allow 20 per account per hour. The old code deducted the count and wrote to staging before verifying the
    // token, so the 21 rejected rounds of submissions below would use up the quota and the later normal upload could
    // only get 429.
    for _ in 0..21 {
        let forged = [
            // The token is correct but comes from another origin: this is what happens when other pages on the same site can
            // get the token in the page.
            (
                vec![token.clone(), request_key.clone(), file.clone()],
                vec![("Origin", "http://127.0.0.1:8888")],
            ),
            (
                vec![token.clone(), request_key.clone(), file.clone()],
                vec![("Sec-Fetch-Site", "cross-site")],
            ),
            (
                vec![
                    part("_csrf", None, b"wrong-csrf"),
                    request_key.clone(),
                    file.clone(),
                ],
                vec![],
            ),
            (vec![request_key.clone(), file.clone()], vec![]),
        ];
        for (parts, extra) in forged {
            let response = upload_avatar(&fixture, &mut cookies, &parts, &extra).await;
            response.status(StatusCode::FORBIDDEN);
            assert!(response.html.contains("The form has expired"));
        }
    }
    let staged = staging.exists();
    let accepted = upload_avatar(
        &fixture,
        &mut cookies,
        &[token.clone(), request_key.clone(), file.clone()],
        &[],
    )
    .await;
    accepted.status(StatusCode::SEE_OTHER);
    assert!(
        accepted.headers[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/my/profile/avatar/")
    );
    assert!(!staged, "a rejected upload must write nothing to staging");
    // The file comes before the token: the whole file would have to be received before knowing whether the token is
    // right, so it is always rejected.
    let reordered = upload_avatar(
        &fixture,
        &mut cookies,
        &[
            part(
                "request_key",
                None,
                identity::token_hash("reordered").as_bytes(),
            ),
            file.clone(),
            token.clone(),
        ],
        &[],
    )
    .await;
    reordered.status(StatusCode::FORBIDDEN);
    assert!(
        reordered
            .html
            .contains("form token was not sent before the image")
    );
    // A second token of the same name can neither override the already verified first one nor sneak through on its
    // back.
    upload_avatar(
        &fixture,
        &mut cookies,
        &[
            token.clone(),
            part("_csrf", None, b"wrong-csrf"),
            part(
                "request_key",
                None,
                identity::token_hash("duplicate").as_bytes(),
            ),
            file.clone(),
        ],
        &[],
    )
    .await
    .status(StatusCode::BAD_REQUEST);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        1
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn avatar_forms_enforce_upload_gate_require_ready_images_and_preserve_previous_avatar_on_failure()
 {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    let csrf = cookies.csrf(&fixture.identity).await;
    multipart(
        &fixture,
        &mut cookies,
        &csrf,
        "avatar-test",
        "avatar.png",
        b"pending image",
    )
    .await
    .status(StatusCode::FORBIDDEN);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        0
    );
    fixture
        .identity
        .verify_email(&fixture.token("email_verification").await)
        .await
        .unwrap();
    configure_storage(&fixture).await;
    let csrf = cookies.csrf(&fixture.identity).await;
    multipart(
        &fixture,
        &mut cookies,
        "wrong-csrf",
        "bad-csrf",
        "bad.png",
        b"pending image",
    )
    .await
    .status(StatusCode::FORBIDDEN);
    // Larger than the generic account form's 16KiB: the avatar route's own Multipart limit should apply.
    let response = multipart(
        &fixture,
        &mut cookies,
        &csrf,
        "avatar-test",
        "avatar.png",
        &vec![42; 20 * 1024],
    )
    .await;
    response.status(StatusCode::SEE_OTHER);
    let path = response.headers[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let aid: i64 = path.rsplit('/').next().unwrap().parse().unwrap();
    let pending = get(&fixture, &path, &mut cookies).await;
    assert!(pending.html.contains("waiting for the check"));
    assert!(!pending.html.contains("Use this avatar"));
    post(&fixture, &path, &mut cookies, &[])
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.avatar_aid().await, None);
    // Simulates the media service having finished its checks; real byte checks, S3 and public image reads are covered
    // by the media and browser suites.
    gono_forum_web::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='ready',scan_status='clean',mime_type='image/png',width=2,height=2 WHERE aid=?").bind(aid).execute(fixture.db.executor()).await.unwrap();
    let ready = get(&fixture, &path, &mut cookies).await;
    assert!(ready.html.contains("Use this avatar"));
    assert!(ready.html.contains(&format!("src=\"/media/{aid}\"")));
    assert_eq!(fixture.avatar_aid().await, None);
    post(&fixture, &path, &mut cookies, &[])
        .await
        .redirect("/my/profile?avatar_saved=true");
    let profile = get(&fixture, "/my/profile", &mut cookies).await;
    assert!(
        profile
            .html
            .contains(&format!("/avatar/{}/{aid}", fixture.member.member.uid))
    );
    // Avatars bypass the session layer, and revalidating with a still-valid ETag only returns 304; the cache header is
    // `private, max-age=300`, so the browser does not come back within 5 minutes.
    let avatar = format!("/avatar/{}/{aid}", fixture.member.member.uid);
    let avatar_tag = format!("\"avatar-{aid}-128\"");
    let mut fresh = Cookies::default();
    let confirmed = request(
        &fixture,
        "GET",
        &avatar,
        &mut fresh,
        &[],
        &[("if-none-match", &avatar_tag)],
    )
    .await;
    assert_eq!(confirmed.status, StatusCode::NOT_MODIFIED);
    assert_eq!(
        confirmed.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    assert_eq!(confirmed.headers[header::ETAG], avatar_tag.as_str());
    assert!(
        confirmed.headers.get(header::SET_COOKIE).is_none(),
        "avatars neither read the session nor set a CSRF cookie"
    );
    // Forum icons decide visibility per visitor and go through the session layer, but the cache header is kept just the
    // same.
    gono_forum_web::db::query("INSERT INTO gonof_forum_forum(name,slug,description,guidance,icon_aid) VALUES('Icon revalidation','icon-revalidate','','',?)")
        .bind(aid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let fid: i64 = gono_forum_web::db::query_scalar(
        "SELECT fid FROM gonof_forum_forum WHERE slug='icon-revalidate'",
    )
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    let icon_tag = format!("\"forum-icon-{aid}-128\"");
    let icon = request(
        &fixture,
        "GET",
        &format!("/forum-icon/{fid}/{aid}"),
        &mut fresh,
        &[],
        &[("if-none-match", &icon_tag)],
    )
    .await;
    assert_eq!(icon.status, StatusCode::NOT_MODIFIED);
    assert_eq!(
        icon.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    gono_forum_web::db::query("UPDATE gonof_forum_forum SET icon_aid=NULL WHERE fid=?")
        .bind(fid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let failed = multipart(
        &fixture,
        &mut cookies,
        &csrf,
        "avatar-bad",
        "invalid.exe",
        b"unsupported file",
    )
    .await;
    failed.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        failed
            .html
            .contains(&format!("/avatar/{}/{aid}", fixture.member.member.uid))
    );
    let mut attacker = Cookies::session(&fixture.admin.session_token);
    post(&fixture, &path, &mut attacker, &[])
        .await
        .status(StatusCode::NOT_FOUND);
    let mut anonymous = Cookies::default();
    get(
        &fixture,
        &format!("/avatar/{}/{aid}", fixture.admin.member.uid),
        &mut anonymous,
    )
    .await
    .status(StatusCode::NOT_FOUND);
    let public = get(
        &fixture,
        &urls::space(fixture.member.member.uid),
        &mut anonymous,
    )
    .await;
    assert!(
        public
            .html
            .contains(&format!("/avatar/{}/{aid}", fixture.member.member.uid))
    );
    assert!(!public.html.contains("profile@example.test"));
    post(&fixture, "/my/profile/avatar/remove", &mut cookies, &[])
        .await
        .redirect("/my/profile?avatar_removed=true");
    assert_eq!(fixture.avatar_aid().await, None);
    get(
        &fixture,
        &format!("/avatar/{}/{aid}", fixture.member.member.uid),
        &mut anonymous,
    )
    .await
    .status(StatusCode::NOT_FOUND);
    // After restoring the default avatar, the browser coming back with the old ETag to revalidate does not get 304.
    request(
        &fixture,
        "GET",
        &avatar,
        &mut anonymous,
        &[],
        &[("if-none-match", &avatar_tag)],
    )
    .await
    .status(StatusCode::NOT_FOUND);
    // Images by uid (the URL used by lists, newest members and floors): without an avatar, the transparent image is
    // returned; revalidating with the transparent image's tag gets 304, and weak tags are accepted too.
    let by_uid = format!("/avatar/{}", fixture.member.member.uid);
    // Use HEAD: the headers are the same as GET, with no body (the test helper decodes the body as UTF-8, which a PNG
    // cannot be).
    let blank = request(&fixture, "HEAD", &by_uid, &mut anonymous, &[], &[]).await;
    assert_eq!(blank.status, StatusCode::OK);
    assert_eq!(blank.headers[header::ETAG], "\"avatar-none\"");
    assert_eq!(blank.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        blank.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    let blank_again = request(
        &fixture,
        "GET",
        &by_uid,
        &mut anonymous,
        &[],
        &[("if-none-match", "W/\"avatar-none\"")],
    )
    .await;
    assert_eq!(blank_again.status, StatusCode::NOT_MODIFIED);
    // Use this image again: the same URL now points at this image's display version (the attachment here has no real
    // object, so only negotiation is used to verify where it points; fetching the image itself is covered by the
    // display version cases in `media::tests`).
    post(&fixture, &path, &mut cookies, &[])
        .await
        .redirect("/my/profile?avatar_saved=true");
    let current = request(
        &fixture,
        "GET",
        &by_uid,
        &mut anonymous,
        &[],
        &[("if-none-match", &avatar_tag)],
    )
    .await;
    assert_eq!(current.status, StatusCode::NOT_MODIFIED);
    assert_eq!(current.headers[header::ETAG], avatar_tag.as_str());
    // A nonexistent member also gets the transparent image.
    let missing = request(&fixture, "HEAD", "/avatar/999999", &mut anonymous, &[], &[]).await;
    assert_eq!(missing.status, StatusCode::OK);
    assert_eq!(missing.headers[header::ETAG], "\"avatar-none\"");
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn profile_update_throttle_rejects_further_writes_without_mutation() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    for _ in 0..30 {
        post(
            &fixture,
            "/my/profile",
            &mut cookies,
            &[("username", "profilemember"), ("bio", "Saved bio")],
        )
        .await
        .redirect("/my/profile?saved=true");
    }
    post(
        &fixture,
        "/my/profile",
        &mut cookies,
        &[("username", "profilemember"), ("bio", "Must not be saved")],
    )
    .await
    .status(StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        fixture
            .identity
            .profile(&fixture.member.session_token)
            .await
            .unwrap()
            .bio,
        "Saved bio"
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

/// One upload submission: multipart fields in order, plus extra request headers.
type UploadAttempt = (Vec<Vec<u8>>, Vec<(&'static str, &'static str)>);

/// Four forged submissions: correct token from another origin, browser declaring cross-site, wrong token, no token.
fn forged_uploads(token: &[u8], key: &[u8], file: &[u8]) -> Vec<UploadAttempt> {
    vec![
        (
            vec![token.to_vec(), key.to_vec(), file.to_vec()],
            vec![("Origin", "http://127.0.0.1:8888")],
        ),
        (
            vec![token.to_vec(), key.to_vec(), file.to_vec()],
            vec![("Sec-Fetch-Site", "cross-site")],
        ),
        (
            vec![
                part("_csrf", None, b"wrong-csrf"),
                key.to_vec(),
                file.to_vec(),
            ],
            vec![],
        ),
        (vec![key.to_vec(), file.to_vec()], vec![]),
    ]
}

/// Forum attachments (`/api/media`) follow the same order as avatars: origin and token are verified before deducting
/// the count and writing to staging; at most 30 per account per 5 minutes, and rejected forged submissions do not use
/// up the count.
#[tokio::test]
async fn attachment_upload_checks_origin_and_token_before_staging_and_throttles_per_member() {
    let fixture = Fixture::new(|_| {}).await;
    let mut cookies = Cookies::member(&fixture);
    fixture
        .identity
        .verify_email(&fixture.token("email_verification").await)
        .await
        .unwrap();
    configure_storage(&fixture).await;
    let csrf = cookies.csrf(&fixture.identity).await;
    let staging = fixture
        .root
        .join(&fixture._services.config.media.local_directory)
        .join("staging");
    let key = |name: &str| part("request_key", None, identity::token_hash(name).as_bytes());
    let token = part("_csrf", None, csrf.as_bytes());
    let file = part("file", Some("photo.png"), &[7_u8; 512]);
    let hx = ("HX-Request", "true");
    // The old code wrote the file into the staging directory and read the whole body before verifying the token, so the
    // first round would leave a staging directory behind.
    for _ in 0..31 {
        for (parts, mut extra) in forged_uploads(&token, &key("forged"), &file) {
            extra.push(hx);
            let response = upload_to(&fixture, "/api/media", &mut cookies, &parts, &extra).await;
            response.status(StatusCode::FORBIDDEN);
            assert!(
                response.html.contains("The form has expired"),
                "{}",
                response.html
            );
        }
    }
    assert!(
        !staging.exists(),
        "a rejected upload must write nothing to staging"
    );
    // The order used by the editor script: token, request key, return URL, file.
    for round in 0..30 {
        let parts = [
            token.clone(),
            key(&format!("accepted-{round}")),
            part("return_to", None, b"/new"),
            file.clone(),
        ];
        let response = upload_to(&fixture, "/api/media", &mut cookies, &parts, &[hx]).await;
        response.status(StatusCode::OK);
        assert!(
            response.html.contains("data-attachment-id"),
            "{}",
            response.html
        );
    }
    let throttled = upload_to(
        &fixture,
        "/api/media",
        &mut cookies,
        &[token.clone(), key("throttled"), file.clone()],
        &[hx],
    )
    .await;
    throttled.status(StatusCode::TOO_MANY_REQUESTS);
    assert!(throttled.html.contains("Too many uploads"));
    assert_eq!(
        std::fs::read_dir(&staging).unwrap().count(),
        0,
        "an upload over the limit writes nothing to staging"
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        30
    );
    // The file comes before the token: the whole file would have to be received before knowing whether the token is
    // right, so it is always rejected.
    let reordered = upload_to(
        &fixture,
        "/api/media",
        &mut cookies,
        &[key("reordered"), file.clone(), token.clone()],
        &[hx],
    )
    .await;
    reordered.status(StatusCode::FORBIDDEN);
    assert!(
        reordered
            .html
            .contains("form token was not sent before the file")
    );
    // A second token of the same name can neither override the already verified first one nor sneak through on its
    // back.
    upload_to(
        &fixture,
        "/api/media",
        &mut cookies,
        &[
            token.clone(),
            part("_csrf", None, b"wrong-csrf"),
            key("duplicate"),
            file.clone(),
        ],
        &[hx],
    )
    .await
    .status(StatusCode::BAD_REQUEST);
    // The editor form without JS: with a mismatched token it is rejected only when the file is read, and the refilled
    // editor keeps the author's draft.
    let draft = upload_to(
        &fixture,
        "/api/media",
        &mut cookies,
        &[
            part("_csrf", None, b"wrong-csrf"),
            key("draft"),
            part("title", None, b"Draft title to keep"),
            part("body", None, b"Draft body to keep"),
            file.clone(),
            part("upload", None, b"1"),
        ],
        &[],
    )
    .await;
    draft.status(StatusCode::FORBIDDEN);
    assert!(draft.html.contains("The form has expired"));
    assert!(draft.html.contains("Draft title to keep"));
    assert!(draft.html.contains("Draft body to keep"));
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}

/// Admin brand image uploads likewise verify origin and token first, then write to staging.
#[tokio::test]
async fn brand_upload_checks_origin_and_token_before_staging() {
    let fixture = Fixture::new(|_| {}).await;
    configure_storage(&fixture).await;
    let mut cookies = Cookies::session(&fixture.admin.session_token);
    let csrf = cookies.csrf(&fixture.identity).await;
    let staging = fixture
        .root
        .join(&fixture._services.config.media.local_directory)
        .join("staging");
    let key = |name: &str| part("request_key", None, identity::token_hash(name).as_bytes());
    let token = part("_csrf", None, csrf.as_bytes());
    let file = part("file", Some("logo.png"), &[9_u8; 512]);
    for (parts, extra) in forged_uploads(&token, &key("forged"), &file) {
        let response = upload_to(
            &fixture,
            "/manage/branding/upload",
            &mut cookies,
            &parts,
            &extra,
        )
        .await;
        response.status(StatusCode::FORBIDDEN);
        assert!(
            response.html.contains("The form has expired"),
            "{}",
            response.html
        );
    }
    assert!(
        !staging.exists(),
        "a rejected upload must write nothing to staging"
    );
    let reordered = upload_to(
        &fixture,
        "/manage/branding/upload",
        &mut cookies,
        &[key("reordered"), file.clone(), token.clone()],
        &[],
    )
    .await;
    reordered.status(StatusCode::FORBIDDEN);
    assert!(
        reordered
            .html
            .contains("form token was not sent before the image")
    );
    upload_to(
        &fixture,
        "/manage/branding/upload",
        &mut cookies,
        &[
            token.clone(),
            part("_csrf", None, b"wrong-csrf"),
            file.clone(),
        ],
        &[],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!staging.exists());
    upload_to(
        &fixture,
        "/manage/branding/upload",
        &mut cookies,
        &[token.clone(), key("accepted"), file.clone()],
        &[],
    )
    .await
    .redirect("/manage/branding");
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        1
    );
    fixture.db.pool.close().await;
    fixture._services.cleanup_ref().await.unwrap();
}
