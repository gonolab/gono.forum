#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{content, db, identity, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    fields: &[(&str, String)],
    htmx: bool,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, format!("gonof_session__port58833={cookie}"));
    }
    if method == "POST" {
        req = req
            .header(header::ORIGIN, "http://127.0.0.1:58833")
            .header(
                header::HOST,
                gono_forum_web::test_support::host_of("http://127.0.0.1:58833"),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    if htmx {
        req = req.header("HX-Request", "true");
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter())
        .finish();
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
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
#[tokio::test]
async fn private_social_pages_and_native_or_htmx_writes_enforce_ownership_and_csrf() {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    content::seed_forums(&fixture.db).await.unwrap();
    content::seed_fixture(&fixture.db).await.unwrap();
    let identity = identity::Identity::new(
        fixture.db.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let mut users = Vec::new();
    for uid in 9201_i64..=9202 {
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,1,1)").bind(uid).bind(format!("social-http-{uid}")).bind(format!("social-http-{uid}@example.test")).execute(fixture.db.executor()).await.unwrap();
        users.push(http_auth::login(&identity, &fixture.db, uid).await);
    }
    let app = router::build(fixture.config.clone(), &fixture.root, fixture.db.clone());
    for path in [
        "/my/following/threads",
        "/my/following/forums",
        "/my/following/members",
        "/my/followers",
        "/my/following/feed",
        "/my/blocked-users",
        "/my/preferences",
    ] {
        let anonymous = request(&app, "GET", path, None, &[], false).await;
        assert_eq!(anonymous.0, StatusCode::SEE_OTHER);
        let page = request(&app, "GET", path, Some(&users[0].session_token), &[], false).await;
        assert_eq!(page.0, StatusCode::OK, "{path}: {}", page.2);
        assert_eq!(page.1[header::CACHE_CONTROL], "private, no-store");
    }
    let fields = |csrf: &str, desired: bool, location: &str| {
        vec![
            ("_csrf", csrf.into()),
            ("request_key", identity::random_token()),
            ("desired", desired.to_string()),
            ("location", location.into()),
        ]
    };
    let rejected = request(
        &app,
        "POST",
        "/api/thread-103/follow",
        Some(&users[0].session_token),
        &fields("wrong", true, "/my/following/threads"),
        false,
    )
    .await;
    assert_eq!(rejected.0, StatusCode::FORBIDDEN);
    let saved = request(
        &app,
        "POST",
        "/api/thread-103/follow",
        Some(&users[0].session_token),
        &fields(&users[0].csrf_token, true, "/my/following/threads"),
        false,
    )
    .await;
    assert_eq!(saved.0, StatusCode::SEE_OTHER);
    assert_eq!(saved.1[header::LOCATION], "/my/following/threads");
    let ours = request(
        &app,
        "GET",
        "/my/following/threads",
        Some(&users[0].session_token),
        &[],
        false,
    )
    .await;
    assert!(ours.2.contains("1 entry, page 1."));
    let theirs = request(
        &app,
        "GET",
        "/my/following/threads",
        Some(&users[1].session_token),
        &[],
        false,
    )
    .await;
    assert!(theirs.2.contains("0 entries, page 1."));
    let alias = request(
        &app,
        "GET",
        "/my/following/threads?page=1",
        Some(&users[0].session_token),
        &[],
        false,
    )
    .await;
    assert_eq!(alias.0, StatusCode::MOVED_PERMANENTLY);
    let invalid = request(
        &app,
        "GET",
        "/my/following/threads?page=0",
        Some(&users[0].session_token),
        &[],
        false,
    )
    .await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    let block = request(
        &app,
        "POST",
        "/api/users/9202/block",
        Some(&users[0].session_token),
        &fields(&users[0].csrf_token, true, "/my/blocked-users"),
        true,
    )
    .await;
    assert_eq!(block.0, StatusCode::OK);
    assert_eq!(block.1["HX-Redirect"], "/my/blocked-users");
    let blocks = request(
        &app,
        "GET",
        "/my/blocked-users",
        Some(&users[0].session_token),
        &[],
        false,
    )
    .await;
    assert!(blocks.2.contains("social-http-9202"));
    assert!(blocks.2.contains("data-social-target=\"9202\">Unblock"));
    let their_blocks = request(
        &app,
        "GET",
        "/my/blocked-users",
        Some(&users[1].session_token),
        &[],
        false,
    )
    .await;
    assert!(their_blocks.2.contains("0 entries, page 1."));
    let self_block = request(
        &app,
        "POST",
        "/api/users/9201/block",
        Some(&users[0].session_token),
        &fields(&users[0].csrf_token, true, "/my/blocked-users"),
        false,
    )
    .await;
    assert_eq!(self_block.0, StatusCode::UNPROCESSABLE_ENTITY);
    let preferences = vec![
        ("_csrf", users[0].csrf_token.clone()),
        ("version", "0".into()),
        ("site_reply", "true".into()),
        ("email_mention", "true".into()),
    ];
    let updated = request(
        &app,
        "POST",
        "/my/preferences",
        Some(&users[0].session_token),
        &preferences,
        false,
    )
    .await;
    assert_eq!(updated.0, StatusCode::SEE_OTHER);
    assert_eq!(updated.1[header::LOCATION], "/my/preferences?saved=true");
    let stale = request(
        &app,
        "POST",
        "/my/preferences",
        Some(&users[0].session_token),
        &preferences,
        false,
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    let removed = request(
        &app,
        "POST",
        "/api/thread-103/follow",
        Some(&users[0].session_token),
        &fields(&users[0].csrf_token, false, "/my/following/threads?page=2"),
        true,
    )
    .await;
    assert_eq!(removed.0, StatusCode::OK);
    assert_eq!(removed.1["HX-Redirect"], "/my/following/threads");
    for alias in [
        "/settings/blocks",
        "/api/threads/103/follow",
        "/api/members/9202/block",
    ] {
        let response = request(
            &app,
            "GET",
            alias,
            Some(&users[0].session_token),
            &[],
            false,
        )
        .await;
        assert_eq!(response.0, StatusCode::NOT_FOUND);
    }
    fixture.cleanup().await.unwrap();
}
