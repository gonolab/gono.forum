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

/// Two signed-in members (9201, 9202) and the app, built after the given site settings rows were written.
async fn social_site(
    settings: &[(&str, &str)],
) -> (
    gono_forum_web::test_support::TestFixture,
    Router,
    Vec<identity::SessionGrant>,
) {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    content::seed_forums(&fixture.db).await.unwrap();
    content::seed_fixture(&fixture.db).await.unwrap();
    for (key, value) in settings {
        db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
            .bind(*value)
            .bind(*key)
            .execute(fixture.db.executor())
            .await
            .unwrap();
    }
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
    (fixture, app, users)
}

fn state_fields(csrf: &str, location: &str) -> Vec<(&'static str, String)> {
    vec![
        ("_csrf", csrf.into()),
        ("request_key", identity::random_token()),
        ("desired", "true".into()),
        ("location", location.into()),
    ]
}

// Following a member counts toward the interaction limits the admin settings list it under, not also toward the
// limit of thread and forum follows, blocks and forum filters.
#[tokio::test]
async fn following_a_member_does_not_spend_the_social_allowance() {
    let (fixture, app, users) = social_site(&[("settings.limit.social_per_5min", "1")]).await;
    let me = &users[0];
    let followed = request(
        &app,
        "POST",
        "/api/thread-103/follow",
        Some(&me.session_token),
        &state_fields(&me.csrf_token, "/my/following/threads"),
        false,
    )
    .await;
    assert_eq!(followed.0, StatusCode::SEE_OTHER);
    let member = request(
        &app,
        "POST",
        "/api/users/9202/follow",
        Some(&me.session_token),
        &state_fields(&me.csrf_token, "/my/following/members"),
        false,
    )
    .await;
    assert_eq!(member.0, StatusCode::SEE_OTHER, "{}", member.2);
    let following: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_home_member_follow WHERE uid=9201")
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
    assert_eq!(following, 1);
    // The social allowance itself still holds for what it covers.
    let block = request(
        &app,
        "POST",
        "/api/users/9202/block",
        Some(&me.session_token),
        &state_fields(&me.csrf_token, "/my/blocked-users"),
        false,
    )
    .await;
    assert_eq!(block.0, StatusCode::TOO_MANY_REQUESTS);
    fixture.cleanup().await.unwrap();
}

fn assert_interaction_page(response: &(StatusCode, HeaderMap, String), what: &str) {
    assert_eq!(
        response.1.get("X-Interaction-Page").map(|v| v.as_bytes()),
        Some(&b"true"[..]),
        "{what}: the error page is not marked for the page to show"
    );
    assert_eq!(response.1["HX-Retarget"], "body", "{what}");
    assert_eq!(response.1["HX-Reswap"], "innerHTML", "{what}");
}

// A refused follow or forum filter sent with htmx answers with an error page the page swaps in, so the member sees
// why instead of a button that does nothing. (Only member follows are refused on a read-only site.)
#[tokio::test]
async fn refused_social_changes_show_their_reason() {
    let (fixture, app, users) = social_site(&[
        ("settings.site.read_only", "true"),
        ("settings.site.read_only_message", "Closed for the move"),
    ])
    .await;
    let me = &users[0];
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
    for (path, fields, status, reason) in [
        (
            "/api/users/9202/follow".to_owned(),
            state_fields(&me.csrf_token, "/space/9202"),
            StatusCode::FORBIDDEN,
            "Closed for the move",
        ),
        (
            format!("/api/forums/{fid}/filter"),
            vec![
                ("_csrf", me.csrf_token.clone()),
                ("request_key", identity::random_token()),
                ("mode", "everything".into()),
                ("location", "/".into()),
            ],
            StatusCode::BAD_REQUEST,
            "",
        ),
    ] {
        let response = request(&app, "POST", &path, Some(&me.session_token), &fields, true).await;
        assert_eq!(response.0, status, "{path}");
        assert!(response.2.contains(reason), "{path}");
        assert_interaction_page(&response, &path);
        // Without htmx it stays a plain error page.
        let native = request(&app, "POST", &path, Some(&me.session_token), &fields, false).await;
        assert_eq!(native.0, status, "{path}");
        assert!(native.1.get("X-Interaction-Page").is_none(), "{path}");
    }
    fixture.cleanup().await.unwrap();
}

// Rate limited social changes get the same treatment.
#[tokio::test]
async fn rate_limited_social_changes_show_their_reason() {
    let (fixture, app, users) = social_site(&[("settings.limit.social_per_5min", "1")]).await;
    let me = &users[0];
    let fields = state_fields(&me.csrf_token, "/my/blocked-users");
    let first = request(
        &app,
        "POST",
        "/api/users/9202/block",
        Some(&me.session_token),
        &fields,
        true,
    )
    .await;
    assert_eq!(first.0, StatusCode::OK);
    let limited = request(
        &app,
        "POST",
        "/api/thread-103/follow",
        Some(&me.session_token),
        &state_fields(&me.csrf_token, "/"),
        true,
    )
    .await;
    assert_eq!(limited.0, StatusCode::TOO_MANY_REQUESTS);
    assert_interaction_page(&limited, "rate limit");
    fixture.cleanup().await.unwrap();
}
