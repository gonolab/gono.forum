use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content,
    db::{self, Database},
    identity::{self, Identity, IdentityError, SessionGrant},
    moderation, router,
    test_support::TestFixture,
    urls,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const OWNER: &str = "privacyowner";
const PASSWORD: &str = "Privacy-contract-password-2026!";
const BIO: &str = "SPACE_PRIVATE_BIO_MARKER";
const SUBJECT: &str = "SPACE_PRIVATE_TOPIC_MARKER";
const BODY: &str = "SPACE_PRIVATE_BODY_MARKER";

struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
    app: Router,
    admin: SessionGrant,
    owner: SessionGrant,
    reader: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        let config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        let mut discussion = content::fixtures()
            .into_iter()
            .find(|thread| thread.id == 103)
            .unwrap();
        discussion.author = OWNER.into();
        discussion.title = SUBJECT.into();
        discussion.body = vec![BODY.into()];
        for post in &mut discussion.posts {
            post.author = OWNER.into();
            post.body = format!("{BODY}_{}", post.pid);
        }
        let mut threads = vec![discussion.clone()];
        for tid in 201..=221 {
            let mut thread = discussion.clone();
            thread.id = tid;
            thread.title = format!("{SUBJECT}_{tid}");
            thread.posts.clear();
            threads.push(thread);
        }
        content::import_fixture(&db, &threads).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let reader = register(&identity, &admin, "privacyreader").await;
        // Import is restricted to empty content databases; enable login for its synthetic author.
        let password: String =
            db::query_scalar("SELECT password FROM gonof_common_member WHERE uid=?")
                .bind(reader.member.uid)
                .fetch_one(db.executor())
                .await
                .unwrap();
        db::query("UPDATE gonof_common_member SET password=?,email='privacyowner@example.test',bio=? WHERE username=?")
            .bind(password)
            .bind(BIO)
            .bind(OWNER)
            .execute(db.executor())
            .await
            .unwrap();
        let owner = identity.login(OWNER, PASSWORD).await.unwrap();
        let app = router::build(config, &services.root, db.clone());
        Self {
            services,
            db,
            identity,
            app,
            admin,
            owner,
            reader,
        }
    }

    async fn visibility(&self, uid: i64) -> String {
        db::query_scalar("SELECT space_visibility FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }

    async fn set(&self, value: &str) {
        self.identity
            .update_space_visibility(&self.owner.session_token, value)
            .await
            .unwrap();
    }
}

async fn register(identity: &Identity, admin: &SessionGrant, name: &str) -> SessionGrant {
    let invite = identity
        .generate_invite(admin.member.uid, name)
        .await
        .unwrap();
    identity
        .register(
            name,
            &format!("{name}@example.test"),
            PASSWORD,
            &invite.code,
        )
        .await
        .unwrap()
}

struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

impl Response {
    fn status(&self, expected: StatusCode) {
        assert_eq!(self.status, expected, "{}", self.html);
        assert_eq!(self.headers[header::CACHE_CONTROL], "private, no-store");
        if self.html.starts_with("<!doctype html>") {
            assert!(
                self.headers[header::VARY]
                    .to_str()
                    .unwrap()
                    .contains("HX-Request")
            );
        }
    }

    fn redirect(&self, status: StatusCode, target: &str) {
        self.status(status);
        assert_eq!(self.headers[header::LOCATION], target);
    }

    fn concealed(&self) {
        for secret in [
            OWNER,
            BIO,
            SUBJECT,
            BODY,
            "data-member-total",
            "data-member-pid",
        ] {
            assert!(
                !self.html.contains(secret),
                "private response contains {secret}"
            );
        }
    }
}

async fn request(
    fixture: &Fixture,
    method: &str,
    path: &str,
    sid: Option<&str>,
    fields: &[(&str, &str)],
    htmx: bool,
    origin: Option<&str>,
) -> Response {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(sid) = sid {
        builder = builder.header(header::COOKIE, format!("gonof_session__port58833={sid}"));
    }
    if htmx {
        builder = builder.header("HX-Request", "true");
    }
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    if let Some(origin) = origin {
        builder = builder
            .header(header::ORIGIN, origin)
            .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut req = builder.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.38:48321".parse::<SocketAddr>().unwrap(),
    ));
    let response = fixture.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
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

async fn get(fixture: &Fixture, path: &str, sid: Option<&str>, htmx: bool) -> Response {
    request(fixture, "GET", path, sid, &[], htmx, None).await
}

async fn save(fixture: &Fixture, sid: &str, value: &str, extra: &[(&str, &str)]) -> Response {
    let session = fixture.identity.session(sid).await.unwrap().unwrap();
    let mut fields = vec![
        ("_csrf", session.csrf_token.as_str()),
        ("space_visibility", value),
    ];
    fields.extend_from_slice(extra);
    request(
        fixture,
        "POST",
        "/my/profile/privacy",
        Some(sid),
        &fields,
        false,
        Some(ORIGIN),
    )
    .await
}

#[tokio::test]
async fn privacy_defaults_and_database_constraints_accept_only_canonical_values() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.visibility(fixture.owner.member.uid).await, "public");
    assert_eq!(
        fixture.visibility(fixture.reader.member.uid).await,
        "public"
    );
    for (index, value) in ["public", "members", "private"].into_iter().enumerate() {
        db::query("INSERT INTO gonof_common_member(uid,username,space_visibility) VALUES(?,?,?)")
            .bind(20_000_i64 + index as i64)
            .bind(format!("privacy-db-{index}"))
            .bind(value)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        fixture.set(value).await;
        assert_eq!(fixture.visibility(fixture.owner.member.uid).await, value);
    }
    // NULL is still rejected by NOT NULL.
    let null: Option<&str> = None;
    assert!(
        db::query("INSERT INTO gonof_common_member(uid,username,space_visibility) VALUES(20999,'privacy-invalid',?)")
            .bind(null)
            .execute(fixture.db.executor())
            .await
            .is_err(),
        "INSERT accepted NULL"
    );
    assert!(
        db::query("UPDATE gonof_common_member SET space_visibility=? WHERE uid=?")
            .bind(null)
            .bind(fixture.owner.member.uid)
            .execute(fixture.db.executor())
            .await
            .is_err(),
        "UPDATE accepted NULL"
    );
    // Other non-canonical values are not rejected by the database: code only writes via `SpaceVisibility`, comparing
    // values exactly; existing data is found byte by byte by the identity check script (trailing spaces and case
    // differences both count).
    for value in ["", "friends", "PUBLIC", "public "] {
        assert!(
            gono_forum_web::identity::SpaceVisibility::parse(value).is_err(),
            "{value:?}"
        );
        for statement in [
            format!(
                "INSERT INTO gonof_common_member(uid,username,space_visibility) VALUES(20999,'privacy-invalid','{value}')"
            ),
            format!(
                "UPDATE gonof_common_member SET space_visibility='{value}' WHERE uid={}",
                fixture.owner.member.uid
            ),
        ] {
            gono_forum_web::test_support::assert_identity_rejected_or_flagged(
                &fixture.db,
                &statement,
                "members with an invalid space visibility or account state",
            )
            .await;
        }
        assert_eq!(
            fixture.visibility(fixture.owner.member.uid).await,
            "private"
        );
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn privacy_gate_covers_every_collection_page_alias_and_viewer_without_admin_bypass() {
    let fixture = Fixture::new().await;
    assert!(!fixture.reader.member.email_verified);
    let uid = fixture.owner.member.uid;
    let space = urls::space(uid);
    let paths = [
        (space.clone(), space.clone()),
        (format!("{space}/threads"), format!("{space}/threads")),
        (format!("{space}/posts"), format!("{space}/posts")),
        (format!("{space}/threads-2"), format!("{space}/threads-2")),
        (format!("{space}/posts-2"), format!("{space}/posts-2")),
        (format!("/space/0{uid}"), space.clone()),
        (format!("{space}/threads-01"), format!("{space}/threads")),
        (
            format!("/space/0{uid}/posts-002"),
            format!("{space}/posts-2"),
        ),
        (
            format!("{space}/threads-999"),
            format!("{space}/threads-999"),
        ),
    ];
    for visibility in ["public", "members", "private"] {
        fixture.set(visibility).await;
        assert_eq!(
            content::public_member(
                &fixture.db,
                OWNER,
                gono_forum_web::test_support::SEEDED_PAGING
            )
            .await
            .unwrap()
            .is_some(),
            visibility == "public"
        );
        for (role, sid) in [
            ("guest", None),
            ("reader", Some(fixture.reader.session_token.as_str())),
            ("owner", Some(fixture.owner.session_token.as_str())),
            ("admin", Some(fixture.admin.session_token.as_str())),
        ] {
            for (path, canonical) in &paths {
                let (path, canonical) = (path.as_str(), canonical.as_str());
                for htmx in [false, true] {
                    let response = get(&fixture, path, sid, htmx).await;
                    if visibility == "private" && role != "owner" {
                        response.status(StatusCode::FORBIDDEN);
                        response.concealed();
                    } else if visibility == "members" && role == "guest" {
                        let query = url::form_urlencoded::Serializer::new(String::new())
                            .append_pair("return_to", canonical)
                            .finish();
                        response.redirect(StatusCode::SEE_OTHER, &format!("/login?{query}"));
                        response.concealed();
                    } else if path.ends_with("-999") {
                        response.status(StatusCode::NOT_FOUND);
                    } else if path != canonical {
                        response.redirect(StatusCode::MOVED_PERMANENTLY, canonical);
                    } else {
                        response.status(StatusCode::OK);
                        assert!(response.html.contains(BIO));
                        assert!(response.html.contains("data-member-pid"));
                    }
                }
            }
        }
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn privacy_changes_leave_public_threads_and_author_identity_visible() {
    let fixture = Fixture::new().await;
    for visibility in ["members", "private"] {
        fixture.set(visibility).await;
        for htmx in [false, true] {
            let response = get(&fixture, "/thread-103", None, htmx).await;
            response.status(StatusCode::OK);
            assert!(response.html.contains(SUBJECT));
            assert!(response.html.contains(BODY));
            assert!(response.html.contains(&format!(
                "href=\"{}\"",
                urls::space(fixture.owner.member.uid)
            )));
            assert!(response.html.contains(OWNER));
        }
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn no_javascript_privacy_save_persists_only_the_authenticated_members_setting() {
    let fixture = Fixture::new().await;
    let sid = fixture.owner.session_token.as_str();
    let page = get(&fixture, "/my/profile", Some(sid), false).await;
    page.status(StatusCode::OK);
    assert!(page.html.contains("data-space-privacy"));
    assert!(page.html.contains("/my/profile/privacy"));
    assert!(page.html.contains("Who can see your profile page"));
    for visibility in ["members", "private", "public"] {
        save(&fixture, sid, visibility, &[])
            .await
            .redirect(StatusCode::SEE_OTHER, "/my/profile?privacy_saved=true");
        assert_eq!(
            fixture.visibility(fixture.owner.member.uid).await,
            visibility
        );
        let refreshed = fixture.identity.profile(sid).await.unwrap();
        assert_eq!(refreshed.space_visibility.value(), visibility);
        assert_eq!(refreshed.bio, BIO);
        assert_eq!(refreshed.member.username, OWNER);
        let page = get(&fixture, "/my/profile?privacy_saved=true", Some(sid), false).await;
        page.status(StatusCode::OK);
        assert!(
            page.html
                .contains(&format!("value=\"{visibility}\" selected"))
        );
        assert!(!page.html.contains(PASSWORD));
    }
    let victim = fixture.owner.member.uid.to_string();
    save(
        &fixture,
        &fixture.reader.session_token,
        "private",
        &[("uid", &victim), ("username", OWNER)],
    )
    .await
    .redirect(StatusCode::SEE_OTHER, "/my/profile?privacy_saved=true");
    assert_eq!(fixture.visibility(fixture.owner.member.uid).await, "public");
    assert_eq!(
        fixture.visibility(fixture.reader.member.uid).await,
        "private"
    );
    for visibility in ["", "friends", "PUBLIC", "public "] {
        save(&fixture, sid, visibility, &[])
            .await
            .status(StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(fixture.visibility(fixture.owner.member.uid).await, "public");
    }
    assert!(matches!(
        fixture
            .identity
            .update_space_visibility(sid, "friends")
            .await,
        Err(IdentityError::Validation(_))
    ));
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn privacy_save_rejects_missing_csrf_cross_site_origin_and_anonymous_writes() {
    let fixture = Fixture::new().await;
    let sid = fixture.owner.session_token.as_str();
    let csrf = fixture
        .identity
        .session(sid)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    for (token, origin) in [
        ("", Some(ORIGIN)),
        ("invalid-token", Some(ORIGIN)),
        (csrf.as_str(), Some("https://elsewhere.example")),
        (csrf.as_str(), Some("null")),
    ] {
        request(
            &fixture,
            "POST",
            "/my/profile/privacy",
            Some(sid),
            &[("_csrf", token), ("space_visibility", "private")],
            false,
            origin,
        )
        .await
        .status(StatusCode::FORBIDDEN);
        assert_eq!(fixture.visibility(fixture.owner.member.uid).await, "public");
    }
    let anonymous = request(
        &fixture,
        "POST",
        "/my/profile/privacy",
        None,
        &[("_csrf", &csrf), ("space_visibility", "private")],
        false,
        Some(ORIGIN),
    )
    .await;
    anonymous.redirect(StatusCode::SEE_OTHER, "/login?return_to=%2Fmy%2Fprofile");
    assert_eq!(fixture.visibility(fixture.owner.member.uid).await, "public");
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn stale_credentials_and_frozen_accounts_cannot_bypass_or_update_space_privacy() {
    let fixture = Fixture::new().await;
    fixture.set("members").await;
    let collection = format!("{}/posts-2", urls::space(fixture.owner.member.uid));
    let old_sid = fixture.owner.session_token.as_str();
    let old_csrf = fixture
        .identity
        .session(old_sid)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    db::query(
        "UPDATE gonof_common_member SET credentials_version=credentials_version+1 WHERE uid=?",
    )
    .bind(fixture.owner.member.uid)
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let fresh = fixture.identity.login(OWNER, PASSWORD).await.unwrap();
    for (sid, csrf, frozen) in [
        (old_sid, old_csrf.as_str(), false),
        (
            fresh.session_token.as_str(),
            fresh.csrf_token.as_str(),
            true,
        ),
    ] {
        if frozen {
            db::query("UPDATE gonof_common_member SET freeze=1 WHERE uid=?")
                .bind(fixture.owner.member.uid)
                .execute(fixture.db.executor())
                .await
                .unwrap();
        }
        for htmx in [false, true] {
            let response = get(&fixture, &collection, Some(sid), htmx).await;
            response.redirect(
                StatusCode::SEE_OTHER,
                &format!(
                    "/login?return_to=%2Fspace%2F{}%2Fposts-2",
                    fixture.owner.member.uid
                ),
            );
            response.concealed();
            get(&fixture, "/my/profile", Some(sid), htmx)
                .await
                .redirect(StatusCode::SEE_OTHER, "/login?return_to=%2Fmy%2Fprofile");
        }
        assert!(matches!(
            fixture
                .identity
                .update_space_visibility(sid, "public")
                .await,
            Err(IdentityError::Unauthorized)
        ));
        request(
            &fixture,
            "POST",
            "/my/profile/privacy",
            Some(sid),
            &[("_csrf", csrf), ("space_visibility", "public")],
            false,
            Some(ORIGIN),
        )
        .await
        .redirect(StatusCode::SEE_OTHER, "/login?return_to=%2Fmy%2Fprofile");
        assert_eq!(
            fixture.visibility(fixture.owner.member.uid).await,
            "members"
        );
    }
    db::query("UPDATE gonof_common_member SET space_visibility='private' WHERE uid=?")
        .bind(fixture.owner.member.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    for sid in [old_sid, fresh.session_token.as_str()] {
        let response = get(&fixture, &collection, Some(sid), false).await;
        response.status(StatusCode::FORBIDDEN);
        response.concealed();
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn private_profile_bio_is_not_returned_by_public_reporting_paths() {
    let fixture = Fixture::new().await;
    db::query("UPDATE gonof_common_member SET emailstatus=1 WHERE uid=?")
        .bind(fixture.reader.member.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let target = moderation::ReportTarget {
        target_type: "profile".into(),
        target_id: fixture.owner.member.uid,
    };
    let mut reports = Vec::new();
    for grant in [&fixture.reader, &fixture.admin] {
        let id = moderation::submit_report(
            &fixture.db,
            grant.member.actor(),
            &target,
            "Review this synthetic account profile",
            &identity::random_token(),
            identity::now(),
            10,
        )
        .await
        .unwrap();
        reports.push((grant, id));
    }
    for visibility in ["public", "members", "private"] {
        fixture.set(visibility).await;
        let preview =
            moderation::report_target(&fixture.db, &target, false, reports[0].0.member.uid)
                .await
                .unwrap();
        assert_eq!(preview.title, OWNER);
        assert_eq!(preview.body, if visibility == "private" { "" } else { BIO });
        assert_eq!(preview.href, urls::space(fixture.owner.member.uid));
        assert_eq!(
            moderation::report_target(&fixture.db, &target, true, reports[0].0.member.uid)
                .await
                .unwrap()
                .body,
            BIO
        );
        // The report entry URL has the same shape as the report_href that `router.rs` generates for threads and replies.
        let report_href = format!(
            "/my/reports/new?target_type={}&target_id={}",
            target.target_type, target.target_id
        );
        for htmx in [false, true] {
            let anonymous = get(&fixture, &report_href, None, htmx).await;
            anonymous.redirect(StatusCode::SEE_OTHER, "/login");
            anonymous.concealed();
            for (grant, id) in &reports {
                for path in [report_href.clone(), format!("/my/reports/{id}")] {
                    let response = get(&fixture, &path, Some(&grant.session_token), htmx).await;
                    response.status(StatusCode::OK);
                    assert!(response.html.contains(OWNER));
                    assert!(!response.html.contains(BIO));
                }
                let management = get(
                    &fixture,
                    &format!("/manage/reports/{id}"),
                    Some(&fixture.admin.session_token),
                    htmx,
                )
                .await;
                management.status(StatusCode::OK);
                assert!(management.html.contains(BIO));
            }
            let denied = get(
                &fixture,
                &format!("/manage/reports/{}", reports[0].1),
                Some(&fixture.reader.session_token),
                htmx,
            )
            .await;
            denied.status(StatusCode::FORBIDDEN);
            assert!(!denied.html.contains(BIO));
        }
    }
    fixture.services.cleanup().await.unwrap();
}
