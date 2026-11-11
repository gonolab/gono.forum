//! Member search, newest members on the home page, the private message entry on profiles and the check-in
//! statistics on the credits page.
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities, content, db,
    db::Database,
    i18n::Lang,
    identity::{self, Actor, SessionGrant},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(
    app: &Router,
    path: &str,
    session: Option<&SessionGrant>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(session) = session {
        req = req.header(
            header::COOKIE,
            format!("gonof_session={}", session.session_token),
        );
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

/// Inserts a synthetic member. When `state` is `deleted`, the e-mail and password are cleared as after account
/// deletion.
async fn member(
    db: &Database,
    uid: i64,
    username: &str,
    visibility: &str,
    freeze: i64,
    state: &str,
    bio: &str,
) {
    let deleted = state == "deleted";
    db::query(
        "INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate,space_visibility,freeze,account_state,bio)
         VALUES(?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(uid)
    .bind(username)
    .bind((!deleted).then_some("synthetic-hash"))
    .bind((!deleted).then(|| format!("member-{uid}@example.test")))
    .bind(i64::from(!deleted))
    .bind(1_767_225_600 + uid)
    .bind(visibility)
    .bind(freeze)
    .bind(state)
    .bind(if deleted { "" } else { bio })
    .execute(db.executor())
    .await
    .unwrap();
}

async fn block(db: &Database, uid: i64, blocked: i64) {
    db::query("INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES(?,?,1)")
        .bind(uid)
        .bind(blocked)
        .execute(db.executor())
        .await
        .unwrap();
}

struct Setup {
    fixture: TestFixture,
    identity: identity::Identity,
    app: Router,
}

async fn setup() -> Setup {
    let fixture = TestFixture::migrated().await.unwrap();
    content::seed_forums(&fixture.db).await.unwrap();
    let identity = identity::Identity::new(
        fixture.db.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let app = router::build_with_global(
        fixture.config.clone(),
        &fixture.root,
        fixture.db.clone(),
        fixture.cache.clone(),
        fixture.global.clone(),
    );
    Setup {
        fixture,
        identity,
        app,
    }
}

/// Member uids that appear in the result list, in page order.
fn listed(body: &str) -> Vec<i64> {
    body.split("data-member-uid=\"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].parse().unwrap())
        .collect()
}

#[tokio::test]
async fn member_search_matches_normalized_prefixes_escapes_wildcards_and_respects_space_privacy() {
    let s = setup().await;
    let db = &s.fixture.db;
    member(db, 9301, "dir-alpha", "public", 0, "active", "Public bio").await;
    member(
        db,
        9302,
        "dir-beta",
        "members",
        0,
        "active",
        "Bio for logged-in members",
    )
    .await;
    member(
        db,
        9303,
        "dir-gamma",
        "private",
        0,
        "active",
        "Secret bio for the owner only",
    )
    .await;
    member(db, 9304, "dir-delta", "private", 0, "deleted", "").await;
    member(db, 9305, "dir-frozen", "public", 1, "active", "").await;
    member(db, 9306, "dir-leaving", "public", 0, "deleting", "").await;
    member(db, 9307, "dir_lit", "public", 0, "active", "").await;
    member(db, 9308, "dir%pct", "public", 0, "active", "").await;
    member(db, 9309, "dir-blocked", "public", 0, "active", "").await;
    member(db, 9310, "dir-viewer", "public", 0, "active", "").await;
    member(db, 9311, "xdir-alpha", "public", 0, "active", "").await;
    block(db, 9310, 9309).await;
    let viewer = http_auth::login(&s.identity, db, 9310).await;

    // Anonymous: only "Everyone"; frozen members and those in deletion (deleting) appear as usual, like the
    // personal space gate; deleted ones do not appear.
    let (status, headers, body) = get(&s.app, "/search/users?q=dir-", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(listed(&body), [9301, 9309, 9305, 9306, 9310]);
    assert!(body.contains("Public bio"));
    assert!(body.contains("href=\"/space/9301\"") && body.contains("UID 9301"));
    assert!(
        body.contains("<span>Joined "),
        "shows the registration time"
    );
    // Logged-in member: "Logged-in members" is added, excluding people the member blocked; "Only me" still does not
    // appear,
    // and its bio is not exposed anywhere.
    let (_, _, body) = get(&s.app, "/search/users?q=dir-", Some(&viewer)).await;
    assert_eq!(listed(&body), [9301, 9302, 9305, 9306, 9310]);
    assert!(body.contains("Bio for logged-in members"));
    for page in [
        get(&s.app, "/search/users?q=dir", None).await.2,
        get(&s.app, "/search/users?q=dir", Some(&viewer)).await.2,
    ] {
        assert!(!page.contains("dir-gamma") && !page.contains("Secret bio for the owner only"));
        assert!(!page.contains("dir-delta"));
    }

    // Prefix match, not substring match.
    let (_, _, body) = get(&s.app, "/search/users?q=alpha", None).await;
    assert!(listed(&body).is_empty(), "a substring match is not a hit");
    assert!(body.contains("username starts with \u{201c}alpha\u{201d}"));
    // Normalized the same way as in the database: trim surrounding whitespace, NFKC, lowercase.
    // "  DIR-AL " with full-width letters (U+FF24 and so on) between half-width spaces and hyphen.
    let full_width = url::form_urlencoded::byte_serialize(
        "  \u{ff24}\u{ff29}\u{ff32}-\u{ff21}\u{ff2c} ".as_bytes(),
    )
    .collect::<String>();
    let (_, _, body) = get(&s.app, &format!("/search/users?q={full_width}"), None).await;
    assert_eq!(listed(&body), [9301]);
    // LIKE wildcards match literally: without escaping, `dir_` would match every `dir-…` and `dir%` every `dir…`.
    let (_, _, body) = get(&s.app, "/search/users?q=dir_", None).await;
    assert_eq!(listed(&body), [9307]);
    let (_, _, body) = get(&s.app, "/search/users?q=dir%25", None).await;
    assert_eq!(listed(&body), [9308]);
    let (_, _, body) = get(&s.app, "/search/users?q=dir!", None).await;
    assert!(listed(&body).is_empty());

    // An empty search term does not query the database and shows an input hint; the two search pages carry scope tabs
    // to each other.
    let (status, _, body) = get(&s.app, "/search/users", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Type the start of a username to find members"));
    assert!(body.contains("aria-label=\"Search in\""));
    let (_, _, threads) = get(&s.app, "/search?q=dir", None).await;
    assert!(threads.contains("href=\"/search/users?q=dir\""));
    assert!(threads.contains("aria-label=\"Search in\""));
    s.fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_search_pages_twenty_at_a_time_and_rejects_out_of_range_pages() {
    let s = setup().await;
    let db = &s.fixture.db;
    for index in 1..=25 {
        member(
            db,
            9400 + index,
            &format!("page-{index:02}"),
            "public",
            0,
            "active",
            "",
        )
        .await;
    }
    let (_, _, first) = get(&s.app, "/search/users?q=page-", None).await;
    let uids = listed(&first);
    assert_eq!(uids.len(), 20);
    assert_eq!(uids[0], 9401, "sorted by username");
    assert!(first.contains("href=\"/search/users?q=page-&#38;page=2\""));
    assert!(!first.contains("rel=\"prev\""));
    let (status, _, second) = get(&s.app, "/search/users?q=page-&page=2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed(&second), (9421..=9425).collect::<Vec<_>>());
    assert!(second.contains("rel=\"prev\"") && !second.contains("rel=\"next\""));
    assert_eq!(
        get(&s.app, "/search/users?q=page-&page=3", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&s.app, "/search/users?q=page-&page=0", None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&s.app, "/search/users?q=page-&page=51", None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&s.app, "/search/users?q=page-&page=x", None).await.0,
        StatusCode::BAD_REQUEST
    );
    s.fixture.cleanup().await.unwrap();
}

/// Uids that appear in the home page statistics card "Newest members".
fn recent(body: &str) -> Vec<i64> {
    let start = body
        .find("class=\"newcomers\"")
        .expect("the newest members card");
    let section = &body[start..start + body[start..].find("</div>").unwrap()];
    section
        .split("href=\"/space/")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].parse().unwrap())
        .collect()
}

#[tokio::test]
async fn warmed_recent_members_follow_privacy_renames_and_freezes_across_processes() {
    use gono_forum_web::{cache::GlobalCache, moderation};

    let s = setup().await;
    let db = &s.fixture.db;
    let uid = 9980;
    let replacement = 9979;
    let admin = admin_actor(db).await;
    member(db, replacement, "recent-next", "public", 0, "active", "").await;
    member(db, uid, "recent-owner", "public", 0, "active", "").await;
    let owner = http_auth::login(&s.identity, db, uid).await;
    gono_forum_web::test_support::set_setting(db, "settings.display.recent_members", "1")
        .await
        .unwrap();
    let other_global = GlobalCache::new(s.fixture.cache.clone());
    let other = router::build_with_global(
        s.fixture.config.clone(),
        &s.fixture.root,
        db.clone(),
        s.fixture.cache.clone(),
        other_global.clone(),
    );
    for app in [&s.app, &other] {
        assert_eq!(recent(&get(app, "/", None).await.2), [uid]);
    }
    s.identity
        .update_space_visibility(&owner.session_token, "private")
        .await
        .unwrap();
    assert_eq!(
        get(&s.app, &format!("/space/{uid}"), None).await.0,
        StatusCode::FORBIDDEN
    );
    // The second process already has a stale L1; after clearing it, it still reads the shared Redis once. Both sides
    // must fill up to the displayed count.
    assert_eq!(recent(&get(&other, "/", None).await.2), [replacement]);
    other_global.clear_local();
    assert_eq!(recent(&get(&other, "/", None).await.2), [replacement]);
    assert_eq!(recent(&get(&s.app, "/", None).await.2), [replacement]);

    s.identity
        .update_space_visibility(&owner.session_token, "public")
        .await
        .unwrap();
    assert_eq!(recent(&get(&other, "/", None).await.2), [uid]);
    s.identity
        .update_profile(&owner.session_token, "recent-renamed", "")
        .await
        .unwrap();
    let html = get(&other, "/", None).await.2;
    let (_, sidebar) = html.split_once("class=\"newcomers\"").unwrap();
    let (entries, _) = sidebar.split_once("</div>").unwrap();
    assert!(
        entries.contains("recent-renamed") && !entries.contains("recent-owner"),
        "{entries}"
    );
    let now = identity::now();
    let sanction = moderation::issue_sanction(
        db,
        admin,
        &moderation::SanctionRequest {
            uid,
            expected_version: owner.member.credentials_version,
            kind: "freeze".into(),
            hours: 1,
            reason: "Testing the newest members cache".into(),
            request_key: identity::random_token(),
        },
        now,
    )
    .await
    .unwrap();
    assert_eq!(recent(&get(&other, "/", None).await.2), [replacement]);
    moderation::revoke_sanction(db, admin, sanction, "Lifting the test freeze", now)
        .await
        .unwrap();
    assert_eq!(recent(&get(&s.app, "/", None).await.2), [uid]);
    s.fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn recent_members_follow_the_directory_scope_and_the_shared_cache_holds_only_public_members()
{
    let s = setup().await;
    let db = &s.fixture.db;
    member(db, 9501, "recent-public", "public", 0, "active", "").await;
    member(db, 9502, "recent-members", "members", 0, "active", "").await;
    member(db, 9503, "recent-private", "private", 0, "active", "").await;
    member(db, 9504, "recent-frozen", "public", 1, "active", "").await;
    member(db, 9505, "recent-leaving", "public", 0, "deleting", "").await;
    member(db, 9506, "recent-deleted", "private", 0, "deleted", "").await;
    member(db, 9507, "recent-blocked", "public", 0, "active", "").await;
    member(db, 9508, "recent-viewer", "public", 0, "active", "").await;
    block(db, 9508, 9507).await;
    let viewer = http_auth::login(&s.identity, db, 9508).await;
    // Query from the logged-in view first: if it were wrongly written to the shared cache, the following anonymous
    // view would see logged-in-only members.
    let (_, _, member_home) = get(&s.app, "/", Some(&viewer)).await;
    assert_eq!(recent(&member_home), [9508, 9502, 9501]);
    let (_, _, anonymous_home) = get(&s.app, "/", None).await;
    assert_eq!(recent(&anonymous_home), [9508, 9507, 9501]);
    // Conversely, once the anonymous result is cached, the logged-in view still queries fresh with its own scope and
    // blocks.
    let (_, _, member_home) = get(&s.app, "/", Some(&viewer)).await;
    assert_eq!(recent(&member_home), [9508, 9502, 9501]);
    // At most 8, by registration order (uid descending).
    for uid in 9511..=9520 {
        member(
            db,
            uid,
            &format!("recent-more-{uid}"),
            "public",
            0,
            "active",
            "",
        )
        .await;
    }
    let (_, _, member_home) = get(&s.app, "/", Some(&viewer)).await;
    assert_eq!(
        recent(&member_home),
        (9513..=9520).rev().collect::<Vec<_>>()
    );
    s.fixture.cleanup().await.unwrap();
}

async fn admin_actor(db: &Database) -> Actor {
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate,adminid) VALUES(9600,'dm-admin','synthetic-hash','dm-admin@example.test',1,1,1)")
        .execute(db.executor())
        .await
        .unwrap();
    Actor {
        uid: 9600,
        credentials_version: 1,
        api_key: None,
    }
}

/// The private message entry on a profile: `Some("")` is clickable, `Some(reason)` is disabled, `None` is not
/// shown.
fn entry(body: &str) -> Option<String> {
    if body.contains("data-message-entry>Messages</a>") {
        return Some(String::new());
    }
    let start = body.find("data-message-entry=\"disabled\"")?;
    let rest = &body[start..];
    // The reason is shown in parentheses: `<span class="message-entry-reason"> (reason)</span>`.
    let marker = "class=\"message-entry-reason\"> (";
    let rest = &rest[rest.find(marker).unwrap() + marker.len()..];
    let reason = &rest[..rest.find(")</span>").unwrap()];
    Some(reason.to_owned())
}

#[tokio::test]
async fn profile_message_entry_shows_only_when_sending_is_allowed() {
    let s = setup().await;
    let db = &s.fixture.db;
    let admin = admin_actor(db).await;
    member(db, 9601, "dm-sender", "public", 0, "active", "").await;
    member(db, 9602, "dm-target", "public", 0, "active", "").await;
    member(db, 9603, "dm-frozen", "public", 1, "active", "").await;
    member(db, 9604, "dm-leaving", "public", 0, "deleting", "").await;
    member(db, 9605, "dm-blocker", "public", 0, "active", "").await;
    member(db, 9606, "dm-blocked", "public", 0, "active", "").await;
    member(db, 9607, "dm-unverified", "public", 0, "active", "").await;
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=9607")
        .execute(db.executor())
        .await
        .unwrap();
    block(db, 9605, 9601).await;
    block(db, 9601, 9606).await;
    let sender = http_auth::login(&s.identity, db, 9601).await;

    // Not shown to anonymous visitors; not shown on the member's own profile.
    assert_eq!(entry(&get(&s.app, "/space/9602", None).await.2), None);
    assert_eq!(
        entry(&get(&s.app, "/space/9601", Some(&sender)).await.2),
        None
    );
    // Can send: links to the new private message page with the recipient prefilled. A frozen recipient can still
    // receive; sending still works when I have blocked the recipient.
    let body = get(&s.app, "/space/9602", Some(&sender)).await.2;
    assert_eq!(entry(&body), Some(String::new()));
    assert!(body.contains("href=\"/my/messages/new?to=9602\""));
    assert_eq!(
        entry(&get(&s.app, "/space/9603", Some(&sender)).await.2),
        Some(String::new())
    );
    assert_eq!(
        entry(&get(&s.app, "/space/9606", Some(&sender)).await.2),
        Some(String::new())
    );
    // The recipient is in deletion: say so; the recipient blocked me: still the
    // generic "This member does not accept your private messages".
    assert_eq!(
        entry(&get(&s.app, "/space/9604", Some(&sender)).await.2).as_deref(),
        Some("This member is closing their account and cannot receive private messages")
    );
    assert_eq!(
        entry(&get(&s.app, "/space/9605", Some(&sender)).await.2).as_deref(),
        Some("This member does not accept your private messages")
    );
    // A refused send uses the same sentence as the entry; looking up by username still finds members in the deletion
    // cooling-off period and leaves the reason to the send transaction, while deleted members are still not found.
    member(db, 9608, "dm-gone", "public", 0, "deleted", "").await;
    let actor = Actor {
        uid: 9601,
        credentials_version: 1,
        api_key: None,
    };
    for (uid, reason) in [
        (
            9604,
            "This member is closing their account and cannot receive private messages",
        ),
        (9605, "This member does not accept your private messages"),
        (
            9608,
            "This member's account has been deleted; you can no longer send them private messages",
        ),
    ] {
        match gono_forum_web::messages::send(db, actor, uid, "Hello", &identity::random_token())
            .await
        {
            Err(content::writes::ContentWriteError::Forbidden(message)) => {
                assert_eq!(message, reason, "uid {uid}")
            }
            other => panic!("uid {uid} must be refused: {:?}", other.map(|_| ())),
        }
    }
    assert_eq!(
        gono_forum_web::messages::find_member(db, "dm-leaving")
            .await
            .unwrap(),
        9604
    );
    assert!(matches!(
        gono_forum_web::messages::find_member(db, "dm-gone").await,
        Err(content::writes::ContentWriteError::Validation(
            "Member not found"
        ))
    ));
    // The member's own e-mail is not verified.
    let unverified = http_auth::login(&s.identity, db, 9607).await;
    assert_eq!(
        entry(&get(&s.app, "/space/9602", Some(&unverified)).await.2).as_deref(),
        Some("Verify your email before sending private messages")
    );
    // The site is read-only.
    gono_forum_web::runtime::update(db, &s.fixture.global, admin, "site.settings.save", |s| {
        s.site.read_only = true;
    })
    .await
    .unwrap();
    assert_eq!(
        entry(&get(&s.app, "/space/9602", Some(&sender)).await.2).as_deref(),
        Some("Private messages are unavailable while the site is read-only")
    );
    gono_forum_web::runtime::update(db, &s.fixture.global, admin, "site.settings.save", |s| {
        s.site.read_only = false;
    })
    .await
    .unwrap();
    // The new private message page prefills the recipient's username from ?to=; recipients who cannot receive,
    // oneself and malformed parameters are not prefilled.
    let prefilled = |body: &str| {
        let start = body.find("name=\"username\"").unwrap();
        let rest = &body[start..];
        let value = &rest[rest.find("value=\"").unwrap() + 7..];
        value[..value.find('"').unwrap()].to_owned()
    };
    let (status, _, body) = get(&s.app, "/my/messages/new?to=9602", Some(&sender)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(prefilled(&body), "dm-target");
    for query in ["?to=9604", "?to=9601", "?to=0", "?to=abc", "?to=999999", ""] {
        let body = get(&s.app, &format!("/my/messages/new{query}"), Some(&sender))
            .await
            .2;
        assert_eq!(prefilled(&body), "", "{query}");
    }
    assert_eq!(
        get(&s.app, "/my/messages/new?to=9602", None).await.0,
        StatusCode::SEE_OTHER
    );
    // Existing conversation: once the other side enters the deletion cooling-off period, the conversation list marks
    // "Being deleted" and the conversation page says the account is being deleted and has no reply box; after the
    // deletion completes it changes back to "Deleted user" and the original sentence.
    let thread = gono_forum_web::messages::send(
        db,
        actor,
        9602,
        "A message before the deletion",
        &identity::random_token(),
    )
    .await
    .unwrap();
    for (state, badge, notice) in [
        (
            "deleting",
            "Being deleted",
            "This member is closing their account and cannot receive private messages.",
        ),
        // The page escapes the apostrophe as `&#39;`.
        (
            "deleted",
            "Deleted user",
            "This member&#39;s account has been deleted; you can no longer send them private messages.",
        ),
    ] {
        db::query("UPDATE gonof_common_member SET account_state=? WHERE uid=9602")
            .bind(state)
            .execute(db.executor())
            .await
            .unwrap();
        let list = get(&s.app, "/my/messages", Some(&sender)).await.2;
        assert!(
            list.contains(&format!("<span class=\"tag tag--state\">{badge}</span>")),
            "{state}: {list}"
        );
        let page = get(&s.app, &format!("/my/messages/{thread}"), Some(&sender))
            .await
            .2;
        assert!(
            page.contains(&format!("data-counterpart-state=\"{state}\">{notice}</p>")),
            "{state}: {page}"
        );
        assert!(!page.contains("/replies\""), "{state}");
    }
    s.fixture.cleanup().await.unwrap();
}

/// The value of a `<meta property|name="…" content="…">` in the page; None if absent.
fn meta_content<'a>(html: &'a str, attr: &str) -> Option<&'a str> {
    let marker = format!("{attr} content=\"");
    let start = html.find(&marker)? + marker.len();
    Some(&html[start..start + html[start..].find('"')?])
}

/// The content of `<script type="application/ld+json">`; None if absent.
fn json_ld(html: &str) -> Option<serde_json::Value> {
    let start = html.find("<script type=\"application/ld+json\">")?;
    let rest = &html[start + "<script type=\"application/ld+json\">".len()..];
    let end = rest
        .find("</script>")
        .expect("the structured data is not closed");
    Some(serde_json::from_str(&rest[..end]).expect("the structured data must be valid JSON"))
}

/// Profiles are shown by account state; with indexing on,
/// profiles and collection pages whose visibility is "Everyone" are indexable for anonymous visitors and output
/// `ProfilePage`; profiles and collection pages of deleted accounts are 410 placeholder pages.
#[tokio::test]
async fn profile_pages_show_the_account_state_and_index_public_spaces() {
    let s = setup().await;
    let db = &s.fixture.db;
    let admin = admin_actor(db).await;
    member(
        db,
        9701,
        "seo-normal",
        "public",
        0,
        "active",
        "Collects coins</script>after-the-tag\nSecond line",
    )
    .await;
    member(
        db,
        9702,
        "seo-frozen",
        "public",
        1_900_000_000,
        "active",
        "",
    )
    .await;
    member(db, 9703, "seo-leaving", "public", 0, "deleting", "").await;
    member(db, 9704, "seo-gone-name", "public", 0, "deleted", "").await;
    member(db, 9705, "seo-members", "members", 0, "active", "").await;
    member(db, 9706, "seo-viewer", "public", 0, "active", "").await;
    db::query(
        "INSERT INTO gonof_forum_attachment(aid,uid,request_key,request_hash) VALUES(97010,9701,?,?)",
    )
    .bind(identity::random_token())
    .bind(identity::random_token())
    .execute(db.executor())
    .await
    .unwrap();
    db::query("UPDATE gonof_common_member SET avatar_aid=97010 WHERE uid=9701")
        .execute(db.executor())
        .await
        .unwrap();
    let viewer = http_auth::login(&s.identity, db, 9706).await;
    let robots = |body: &str| meta_content(body, "name=\"robots\"").unwrap().to_owned();
    let canonical = |body: &str| {
        let marker = "<link rel=\"canonical\" href=\"";
        let start = body.find(marker).unwrap() + marker.len();
        body[start..start + body[start..].find('"').unwrap()].to_owned()
    };

    // Indexing off (seeded): public profiles show as usual, but noindex, and no structured data is output.
    let (status, _, body) = get(&s.app, "/space/9701", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(robots(&body), "noindex, nofollow");
    assert!(json_ld(&body).is_none());
    assert_eq!(meta_content(&body, "property=\"og:type\""), Some("website"));
    assert!(!body.contains("profile:username"));

    gono_forum_web::runtime::update(db, &s.fixture.global, admin, "site.settings.save", |s| {
        s.seo.indexing = true;
    })
    .await
    .unwrap();
    // Indexing on: public profiles and collection pages (including pagination) of normal, frozen and deletion
    // cooling-off accounts are all indexable for anonymous visitors.
    for uid in [9701, 9702, 9703] {
        for path in [
            format!("/space/{uid}"),
            format!("/space/{uid}/threads"),
            format!("/space/{uid}/posts"),
        ] {
            let (status, _, body) = get(&s.app, &path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(robots(&body), "index, follow", "{path}");
            assert_eq!(
                meta_content(&body, "property=\"og:type\""),
                Some("profile"),
                "{path}"
            );
            let data = json_ld(&body).unwrap_or_else(|| panic!("{path} lacks ProfilePage"));
            assert_eq!(data["@type"], "ProfilePage", "{path}");
            // Structured data on collection pages also points to the profile URL.
            assert!(
                data["url"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("/space/{uid}")),
                "{path}"
            );
            // State labels do not go into the structured data.
            let text = data.to_string();
            assert!(
                !text.contains("Frozen") && !text.contains("Being deleted"),
                "{path}"
            );
        }
    }
    // ProfilePage for a normal account: username, UID, absolute profile URL, avatar, plain-text bio, thread and reply
    // counts, registration time.
    let (_, _, body) = get(&s.app, "/space/9701", None).await;
    assert_eq!(
        meta_content(&body, "property=\"profile:username\""),
        Some("seo-normal")
    );
    assert!(
        !body.contains("</script>after-the-tag"),
        "a </script> in the bio must not end the data block early"
    );
    let data = json_ld(&body).unwrap();
    let home = canonical(&body);
    assert!(home.starts_with("http") && home.ends_with("/space/9701"));
    assert_eq!(data["url"], home.as_str());
    assert_eq!(data["dateCreated"], "2026-01-01T02:41:41Z");
    let person = &data["mainEntity"];
    assert_eq!(person["@type"], "Person");
    assert_eq!(person["name"], "seo-normal");
    assert_eq!(person["identifier"], "9701");
    assert_eq!(person["url"], home.as_str());
    assert_eq!(
        person["image"],
        format!("{}/avatar/9701/97010", home.trim_end_matches("/space/9701"))
    );
    assert_eq!(
        person["description"],
        "Collects coins</script>after-the-tag\nSecond line"
    );
    let counters = person["agentInteractionStatistic"].as_array().unwrap();
    assert_eq!(counters.len(), 2);
    assert_eq!(
        counters[0]["interactionType"],
        "https://schema.org/WriteAction"
    );
    assert_eq!(counters[0]["userInteractionCount"], 0);
    assert_eq!(
        counters[1]["interactionType"],
        "https://schema.org/CommentAction"
    );
    assert_eq!(counters[1]["userInteractionCount"], 0);
    // Without an avatar and with an empty bio, those two items are left out.
    let frozen = get(&s.app, "/space/9702", None).await.2;
    let person = &json_ld(&frozen).unwrap()["mainEntity"];
    assert!(person.get("image").is_none() && person.get("description").is_none());

    // Profile section labels: frozen shows only "Frozen", without reason or term; the cooling-off period shows
    // "Being deleted"; normal accounts have neither.
    assert!(
        frozen
            .contains("<span class=\"tag tag--state\" data-member-state=\"frozen\">Frozen</span>")
    );
    assert!(!frozen.contains("data-member-state=\"deleting\""));
    assert!(!frozen.contains("2030-"), "the freeze term is not shown");
    let leaving = get(&s.app, "/space/9703", None).await.2;
    assert!(leaving.contains(
        "<span class=\"tag tag--state\" data-member-state=\"deleting\">Being deleted</span>"
    ));
    assert!(!leaving.contains("data-member-state=\"frozen\""));
    assert!(!body.contains("data-member-state="));
    // After unfreezing the label disappears automatically.
    db::query("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=9702")
        .execute(db.executor())
        .await
        .unwrap();
    assert!(
        !get(&s.app, "/space/9702", None)
            .await
            .2
            .contains("data-member-state=")
    );

    // A logged-in member viewing a public profile: not indexable, and no ProfilePage or og:type=profile output.
    let (_, _, body) = get(&s.app, "/space/9701", Some(&viewer)).await;
    assert_eq!(robots(&body), "noindex, nofollow");
    assert!(json_ld(&body).is_none());
    assert_eq!(meta_content(&body, "property=\"og:type\""), Some("website"));
    // A "Logged-in members" profile still redirects anonymous visitors to login, and what members see is not indexable.
    assert_eq!(
        get(&s.app, "/space/9705", None).await.0,
        StatusCode::SEE_OTHER
    );
    let (status, _, body) = get(&s.app, "/space/9705", Some(&viewer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(robots(&body), "noindex, nofollow");
    assert!(json_ld(&body).is_none());

    // Deleted: profiles and collection pages (any page number) are all 410 placeholder pages, the same for anonymous
    // visitors and members; they only say "This account has been deleted", without the original username, avatar or profile data,
    // noindex, no canonical and no structured data. A URL that is itself invalid is still 404.
    for path in [
        "/space/9704",
        "/space/9704/threads",
        "/space/9704/posts",
        "/space/9704/posts-3",
        "/space/09704",
    ] {
        for session in [None, Some(&viewer)] {
            let (status, _, body) = get(&s.app, path, session).await;
            assert_eq!(status, StatusCode::GONE, "{path}");
            assert!(
                body.contains("<h1 class=\"panel__title\">This account has been deleted</h1>"),
                "{path}"
            );
            assert!(body.contains("<title>Deleted user - "), "{path}");
            assert!(!body.contains("seo-gone-name"), "{path}");
            assert!(!body.contains("/avatar/9704"), "{path}");
            assert!(!body.contains("data-profile-head"), "{path}");
            assert_eq!(robots(&body), "noindex, nofollow", "{path}");
            assert!(!body.contains("rel=\"canonical\""), "{path}");
            assert!(json_ld(&body).is_none(), "{path}");
            assert_eq!(meta_content(&body, "property=\"og:type\""), Some("website"));
        }
    }
    assert_eq!(
        get(&s.app, "/space/9704/unknown", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&s.app, "/space/999999", None).await.0,
        StatusCode::NOT_FOUND
    );
    s.fixture.cleanup().await.unwrap();
}

fn stat(body: &str, name: &str) -> String {
    let start = body.find(&format!("data-signin-{name}>")).unwrap() + name.len() + 13;
    body[start..start + body[start..].find('<').unwrap()].to_owned()
}

#[tokio::test]
async fn credits_page_shows_signin_streaks_month_count_and_calendar_by_shanghai_day() {
    let s = setup().await;
    let db = &s.fixture.db;
    let admin = admin_actor(db).await;
    member(db, 9701, "signin-member", "public", 0, "active", "").await;
    member(db, 9702, "signin-other", "public", 0, "active", "").await;
    let viewer = http_auth::login(&s.identity, db, 9701).await;
    let actor = viewer.member.actor();
    let other = Actor {
        uid: 9702,
        credentials_version: 1,
        api_key: None,
    };
    // With rewards off the check-in itself is refused, no record is kept, and the statistics are all 0.
    assert!(
        activities::check_in(db, actor, identity::now())
            .await
            .is_err()
    );
    let body = get(&s.app, "/my/credits", Some(&viewer)).await.2;
    assert_eq!(
        [
            stat(&body, "current"),
            stat(&body, "longest"),
            stat(&body, "month"),
            stat(&body, "total")
        ],
        ["0 days", "0 days", "0 check-ins", "0 check-ins"]
    );
    activities::update_rule(
        db,
        &s.fixture.global,
        admin,
        "signin",
        true,
        1,
        1,
        1,
        identity::now(),
    )
    .await
    .unwrap();
    // Midnight today in Shanghai; 23:59:59 yesterday and midnight today belong to two different days.
    let now = identity::now();
    let today = (now + 8 * 3600).div_euclid(86400) * 86400 - 8 * 3600;
    let times = [
        today,
        today - 1,
        today - 86400 - 1,
        today - 5 * 86400,
        today - 6 * 86400,
        today - 7 * 86400,
        today - 8 * 86400,
    ];
    for time in times {
        assert!(activities::check_in(db, actor, time).await.unwrap());
    }
    // Checking in again on the same Shanghai day is not counted twice; other members' check-ins are not counted.
    assert!(!activities::check_in(db, actor, today + 60).await.unwrap());
    assert!(
        activities::check_in(db, other, today - 3 * 86400)
            .await
            .unwrap()
    );
    let body = get(&s.app, "/my/credits", Some(&viewer)).await.2;
    assert_eq!(
        stat(&body, "current"),
        "3 days",
        "today, yesterday and the day before are in a row; the day before that is missed"
    );
    assert_eq!(stat(&body, "longest"), "4 days");
    assert_eq!(stat(&body, "total"), "7 check-ins");
    let day = |time: i64| (time + 8 * 3600).div_euclid(86400);
    let month_of = |time: i64| {
        let date = chrono::DateTime::from_timestamp(time + 8 * 3600, 0)
            .unwrap()
            .date_naive();
        (
            chrono::Datelike::year(&date),
            chrono::Datelike::month(&date),
        )
    };
    let this_month: Vec<_> = times
        .iter()
        .filter(|time| month_of(**time) == month_of(today))
        .collect();
    // "1 check-in" or "N check-ins", depending on the day of the month.
    assert_eq!(
        stat(&body, "month"),
        Lang::EN.activity_signin_times(this_month.len())
    );
    // Month calendar: every day checked in this month is marked, and today carries aria-current.
    let calendar_days = body.matches("data-signin-day=").count();
    assert_eq!(calendar_days, this_month.len());
    let today_number = chrono::Datelike::day(
        &chrono::DateTime::from_timestamp(today + 8 * 3600, 0)
            .unwrap()
            .date_naive(),
    );
    assert!(body.contains(&format!(
        "aria-current=\"date\" data-signin-day=\"{today_number}\""
    )));
    assert_eq!(day(today) - day(today - 1), 1);
    let (year, month) = month_of(today);
    assert!(body.contains(&format!("<caption>{year}-{month}</caption>")));
    s.fixture.cleanup().await.unwrap();
}
