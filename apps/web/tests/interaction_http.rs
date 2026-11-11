#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content,
    db::Database,
    identity,
    notifications::record::{Kind, NewNotice, Target},
    router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    db: Database,
    app: Router,
    sessions: Vec<String>,
    csrf_tokens: Vec<String>,
    first_pid: i64,
}

impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        let first_pid: i64 = gono_forum_web::db::query_scalar(
            "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        let identity = identity::Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = Vec::new();
        let mut csrf_tokens = Vec::new();
        for uid in [9001_i64, 9002, 9003] {
            gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,?,1)")
                .bind(uid).bind(format!("interaction-http-{uid}")).bind(format!("interaction-http-{uid}@example.test")).bind(if uid==9003 {0_i64}else{1_i64}).execute(db.executor()).await.unwrap();
            let grant = http_auth::login(&identity, &db, uid).await;
            csrf_tokens.push(grant.csrf_token);
            sessions.push(grant.session_token);
        }
        gono_forum_web::db::query("UPDATE gonof_forum_post SET authorid=9001 WHERE pid=?")
            .bind(first_pid)
            .execute(db.executor())
            .await
            .unwrap();
        gono_forum_web::db::query("UPDATE gonof_forum_thread SET authorid=9001 WHERE tid=103")
            .execute(db.executor())
            .await
            .unwrap();
        let app = router::build_with_global(
            config,
            &root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            _services: services,
            db,
            app,
            sessions,
            csrf_tokens,
            first_pid,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        user: Option<usize>,
        fields: &[(&str, String)],
        hx: bool,
        origin: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("HX-Request", if hx { "true" } else { "false" });
        if let Some(user) = user {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session={}", self.sessions[user]),
            );
        }
        if method == "POST" {
            builder = builder
                .header(header::ORIGIN, origin)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter())
            .finish();
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap();
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
        (status, headers, html)
    }

    fn state(&self, user: usize, desired: bool) -> Vec<(&'static str, String)> {
        vec![
            ("_csrf", self.csrf_tokens[user].clone()),
            ("request_key", identity::random_token()),
            ("desired", desired.to_string()),
            ("location", "/thread-103".into()),
        ]
    }
    fn read(&self, user: usize) -> Vec<(&'static str, String)> {
        vec![
            ("_csrf", self.csrf_tokens[user].clone()),
            ("request_key", identity::random_token()),
        ]
    }
    /// Write a notification through the regular entry point, which maintains the unread count. A new database
    /// allocates auto-increment IDs from 1 in order, so assert that we got exactly the one the test hard-codes.
    async fn notice(&self, id: i64, uid: i64, pid: i64) {
        let inserted = gono_forum_web::test_support::insert_notification(
            &self.db,
            &NewNotice {
                uid,
                authorid: 9002,
                kind: Kind::Reply,
                target: Target::Post(pid),
                event_key: &format!("reply:{id}"),
                reason: "",
                dateline: identity::now() - 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(inserted, id);
    }
}

const ORIGIN: &str = "http://127.0.0.1:58833";

#[tokio::test]
async fn collection_cancellation_clamps_pagination_and_keeps_topic_view() {
    let f = Fixture::new().await;
    gono_forum_web::db::query("INSERT INTO gonof_home_favorite(uid,tid,dateline) SELECT 9002,tid,1 FROM gonof_forum_thread ORDER BY tid LIMIT 21")
        .execute(f.db.executor()).await.unwrap();
    let page = f
        .call("GET", "/my/favorites?page=2", Some(1), &[], false, ORIGIN)
        .await;
    assert_eq!(page.0, StatusCode::OK);
    assert_eq!(page.2.matches("data-favorite-id=").count(), 1);
    let tid: i64 =
        gono_forum_web::db::query_scalar("SELECT MIN(tid) FROM gonof_home_favorite WHERE uid=9002")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    let mut fields = f.state(1, false);
    fields
        .iter_mut()
        .find(|(key, _)| *key == "location")
        .unwrap()
        .1 = "/my/favorites?page=2".into();
    let result = f
        .call(
            "POST",
            &format!("/api/thread-{tid}/star"),
            Some(1),
            &fields,
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["hx-redirect"], "/my/favorites");
    let path = "/thread-103-2?view=flat#floor-76";
    let mut fields = f.state(1, true);
    fields
        .iter_mut()
        .find(|(key, _)| *key == "location")
        .unwrap()
        .1 = path.into();
    let result = f
        .call(
            "POST",
            "/api/thread-103/star",
            Some(1),
            &fields,
            false,
            ORIGIN,
        )
        .await;
    assert_eq!(result.0, StatusCode::SEE_OTHER);
    assert_eq!(result.1["location"], path);
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn favorite_and_vote_html_retries_permissions_and_tombstones() {
    let f = Fixture::new().await;
    assert_eq!(
        f.call("GET", "/my/favorites", None, &[], false, ORIGIN)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    let response = f
        .call("GET", "/my/favorites?page=01", Some(0), &[], false, ORIGIN)
        .await;
    assert_eq!(response.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(response.1["location"], "/my/favorites");
    assert_eq!(
        f.call("GET", "/my/favorites?page=2", Some(0), &[], false, ORIGIN)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let anonymous = f.call("GET", "/thread-103", None, &[], false, ORIGIN).await;
    assert_eq!(anonymous.0, StatusCode::OK);
    assert!(anonymous.2.contains("data-favorite-count>0</span>"));
    let fields = f.state(1, true);
    for _ in 0..2 {
        assert_eq!(
            f.call(
                "POST",
                "/api/thread-103/star",
                Some(1),
                &fields,
                false,
                ORIGIN
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
    }
    let anonymous = f.call("GET", "/thread-103", None, &[], false, ORIGIN).await;
    assert!(anonymous.2.contains("data-favorite-count>1</span>"));
    let cancel = f
        .call(
            "POST",
            "/api/thread-103/star",
            Some(1),
            &f.state(1, false),
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(cancel.0, StatusCode::OK);
    assert!(cancel.2.contains("aria-label=\"Bookmark\""));
    assert!(cancel.2.contains("data-favorite-count>0</span>"));
    let anonymous = f.call("GET", "/thread-103", None, &[], false, ORIGIN).await;
    assert!(anonymous.2.contains("data-favorite-count>0</span>"));
    let restored = f
        .call(
            "POST",
            "/api/thread-103/star",
            Some(1),
            &f.state(1, true),
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(restored.0, StatusCode::OK);
    assert!(restored.2.contains("aria-label=\"Remove bookmark\""));
    assert!(restored.2.contains("data-favorite-count>1</span>"));
    let favorites = f
        .call("GET", "/my/favorites", Some(1), &[], false, ORIGIN)
        .await;
    assert_eq!(favorites.1["cache-control"], "private, no-store");
    assert!(favorites.2.contains("data-favorite-id=\"103\""));
    assert!(!favorites.2.contains("data-favorite-count"));
    assert!(
        !f.call("GET", "/my/favorites", Some(0), &[], false, ORIGIN)
            .await
            .2
            .contains("data-favorite-id")
    );
    let endpoint = format!("/api/comments/{}/vote", f.first_pid);
    assert_eq!(
        f.call("POST", &endpoint, Some(0), &f.state(0, true), false, ORIGIN)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let vote = f
        .call("POST", &endpoint, Some(1), &f.state(1, true), true, ORIGIN)
        .await;
    assert_eq!(vote.0, StatusCode::OK);
    assert!(vote.2.contains("aria-label=\"Unlike 1\""));
    assert!(vote.2.contains("data-reaction-count>1</span>"));
    assert!(!vote.2.contains("<!doctype"));
    assert_eq!(
        f.call("POST", &endpoint, Some(1), &f.state(1, false), true, "null")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call(
            "POST",
            &endpoint,
            Some(1),
            &f.state(1, false),
            true,
            "https://outside.example"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut bad = f.state(1, false);
    bad[0].1 = "invalid".into();
    assert_eq!(
        f.call("POST", &endpoint, Some(1), &bad, true, ORIGIN)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call("GET", "/my/favorites", Some(2), &[], false, ORIGIN)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.call(
            "POST",
            "/api/thread-103/star",
            Some(2),
            &f.state(2, true),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=1 WHERE pid=?")
        .bind(f.first_pid)
        .execute(f.db.executor())
        .await
        .unwrap();
    // Hiding the first post makes the thread unlisted and unreadable: after raw SQL, sync through the
    // regular copy code.
    gono_forum_web::test_support::resync_thread(&f.db, 103)
        .await
        .unwrap();
    let hidden = f
        .call("GET", "/my/favorites", Some(1), &[], false, ORIGIN)
        .await
        .2;
    assert!(hidden.contains("Content not available"));
    assert!(!hidden.contains("href=\"/thread-103\""));
    let cancel = f
        .call("POST", &endpoint, Some(1), &f.state(1, false), true, ORIGIN)
        .await;
    assert_eq!(cancel.0, StatusCode::OK);
    assert_eq!(cancel.1["hx-redirect"], "/");
    assert!(!cancel.2.contains("data-reaction-count"));
    assert_eq!(
        f.call(
            "POST",
            "/api/thread-103/star",
            Some(1),
            &f.state(1, false),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn notification_ownership_unread_boundary_and_permanent_open() {
    let f = Fixture::new().await;
    f.notice(1, 9001, 103076).await;
    f.notice(2, 9002, 103002).await;
    f.notice(3, 9003, 103002).await;
    let page = f
        .call("GET", "/my/notifications", Some(0), &[], false, ORIGIN)
        .await;
    assert_eq!(page.0, StatusCode::OK);
    assert!(page.2.contains("data-unread-count=\"1\""));
    assert!(!page.2.contains("data-notification-id=\"2\""));
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/2/read",
            Some(0),
            &f.read(0),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call("GET", "/my/notifications", Some(2), &[], false, ORIGIN)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/3/read",
            Some(2),
            &f.read(2),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let opened = f
        .call(
            "POST",
            "/my/notifications/1/open",
            Some(0),
            &f.read(0),
            false,
            ORIGIN,
        )
        .await;
    assert_eq!(opened.0, StatusCode::SEE_OTHER);
    assert_eq!(
        opened.1["location"],
        "/thread-103?sub_root=103002&sub_page=2#floor-76"
    );
    f.notice(4, 9001, 103002).await;
    f.notice(5, 9001, 103076).await;
    let mut fields = f.read(0);
    fields.push(("max_id", "4".into()));
    let read = f
        .call(
            "POST",
            "/my/notifications/read-all",
            Some(0),
            &fields,
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(read.0, StatusCode::OK);
    assert!(read.2.contains("data-unread-count=\"1\""));
    assert!(read.2.contains("hx-swap-oob=\"true\""));
    assert!(
        read.2
            .contains("hx-target=\"#notification-list\" hx-swap=\"outerHTML\"")
    );
    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=1 WHERE pid=103002")
        .execute(f.db.executor())
        .await
        .unwrap();
    // Hiding a top-level post extends to its nested replies (root_invisible): after raw SQL,
    // recompute by the regular rules.
    gono_forum_web::test_support::refresh_listing(&f.db)
        .await
        .unwrap();
    let hidden = f
        .call("GET", "/my/notifications", Some(0), &[], false, ORIGIN)
        .await
        .2;
    assert!(hidden.contains("Content not available"));
    assert!(!hidden.contains("/my/notifications/5/open"));
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/5/open",
            Some(0),
            &f.read(0),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let unread: i64 =
        gono_forum_web::db::query_scalar("SELECT read_at FROM gonof_home_notification WHERE id=5")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(unread, 0);
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/5/read",
            Some(0),
            &f.read(0),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    for id in 6..=110 {
        f.notice(id, 9001, 103076).await;
    }
    let capped = f
        .call("GET", "/my/notifications", Some(0), &[], false, ORIGIN)
        .await;
    assert_eq!(capped.0, StatusCode::OK);
    assert!(capped.2.contains("data-unread-count=\"105\""));
    assert!(
        capped
            .2
            .contains("aria-label=\"Notifications, 105 unread\"")
    );
    assert!(capped.2.contains("<span class=\"unread-dot\">99+</span>"));
    f._services.cleanup_ref().await.unwrap();
}

/// The member card in the home page sidebar: each of the six stat cells is clickable, the private message placeholder
/// has no entry, the moderator entry appears by role, and visitors only see sign-in and registration.
#[tokio::test]
async fn read_all_in_a_category_tab_leaves_other_categories_unread() {
    let f = Fixture::new().await;
    // The earlier review notification (system kind) has a smaller id and is not visible under the "Replies and mentions" tab.
    let moderation = gono_forum_web::test_support::insert_notification(
        &f.db,
        &NewNotice {
            uid: 9001,
            authorid: 9002,
            kind: Kind::Moderation,
            target: Target::Post(103002),
            event_key: "moderation:1",
            reason: "Review result note",
            dateline: identity::now() - 20,
        },
    )
    .await
    .unwrap();
    assert_eq!(moderation, 1);
    f.notice(2, 9001, 103076).await;
    let unread = || async {
        gono_forum_web::db::query_scalar::<i64>(
            "SELECT id FROM gonof_home_notification WHERE uid=9001 AND read_at=0 ORDER BY id",
        )
        .fetch_all(f.db.executor())
        .await
        .unwrap()
    };

    let replies = f
        .call(
            "GET",
            "/my/notifications?category=replies",
            Some(0),
            &[],
            false,
            ORIGIN,
        )
        .await;
    assert_eq!(replies.0, StatusCode::OK);
    assert!(replies.2.contains("name=\"max_id\" value=\"2\""));
    assert!(replies.2.contains("name=\"category\" value=\"replies\""));
    assert!(!replies.2.contains("data-notification-id=\"1\""));

    let mut fields = f.read(0);
    fields.push(("max_id", "2".into()));
    fields.push(("category", "replies".into()));
    let done = f
        .call(
            "POST",
            "/my/notifications/read-all",
            Some(0),
            &fields,
            false,
            ORIGIN,
        )
        .await;
    assert_eq!(done.0, StatusCode::SEE_OTHER);
    assert_eq!(done.1["location"], "/my/notifications?category=replies");
    assert_eq!(
        unread().await,
        [1],
        "unseen notifications in other categories must stay unread"
    );

    // HTMX partial refreshes also stay in the current category, returning only that category's notifications.
    let mut fields = f.read(0);
    fields.push(("max_id", "1".into()));
    fields.push(("category", "system".into()));
    let system = f
        .call(
            "POST",
            "/my/notifications/read-all",
            Some(0),
            &fields,
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(system.0, StatusCode::OK);
    assert!(system.2.contains("data-notification-kind=\"moderation\""));
    assert!(!system.2.contains("data-notification-kind=\"reply\""));
    assert!(unread().await.is_empty());
    // Only the list is swapped, so the category tabs and the right column stay; the card's unread dot is sent out of
    // band, now hidden.
    assert_eq!(system.1["hx-retarget"], "#notification-list");
    assert_eq!(system.1["hx-reswap"], "outerHTML");
    assert!(system.2.contains("id=\"notification-list\""));
    assert!(!system.2.contains("section-tabs"));
    assert!(
        system.2.contains(" id=\"my-card-unread\"")
            && system.2.contains(" hidden hx-swap-oob=\"true\""),
        "{}",
        system.2
    );
    let page = f
        .call("GET", "/my/notifications", Some(0), &[], false, ORIGIN)
        .await
        .2;
    assert!(page.contains("id=\"notification-list\"") && page.contains(" id=\"my-card-unread\""));
}

#[tokio::test]
async fn member_card_exposes_six_stats_and_role_scoped_entries() {
    let fixture = Fixture::new().await;
    let (status, _, html) = fixture.call("GET", "/", Some(0), &[], false, ORIGIN).await;
    assert_eq!(status, StatusCode::OK);
    // All six cells are links, none a placeholder: the private message cell is the real unread count and entry.
    assert_eq!(html.matches("class=\"tally__item\"").count(), 6);
    assert_eq!(html.matches("tally__item tally__item--pending").count(), 0);
    assert!(html.contains("href=\"/my/messages\""));
    assert_eq!(html.matches("class=\"person__shortcuts\"").count(), 1);
    for path in [
        "/my/favorites",
        "/my/credits",
        "/my/notifications",
        "/my/reports",
        "/my/profile",
        "/new",
    ] {
        assert!(html.contains(&format!("href=\"{path}\"")), "missing {path}");
    }
    // Threads and replies point to the member's own private lists, not the public set.
    assert!(html.contains("href=\"/my/threads\""));
    assert!(html.contains("href=\"/my/posts\""));
    assert!(!html.contains("/space/9001/threads"));
    // The avatar and username lead to the user center overview.
    assert!(html.contains("href=\"/my\" aria-label=\"My center\""));
    // Ordinary members do not see the content management entry, and there is no separate list of user links.
    assert!(!html.contains("href=\"/manage\""));
    assert!(!html.contains("class=\"user-links\""));
    // The page header account menu has seven items and does not lay out the whole user center.
    let menu = html
        .split_once("class=\"account-menu\"")
        .expect("the page header has an account menu")
        .1
        .split_once("</details>")
        .expect("the account menu is closed")
        .0;
    assert!(menu.contains(">My center</a>"));
    assert!(menu.contains(">Public profile</a>"));
    assert!(!menu.contains(">My bookmarks</a>"));
    assert!(!menu.contains(">My invites</a>"));
    // With the check-in rule off by default, the button falls back to the credits page link; when on, members who
    // have not checked in get a form, and after actually checking in it becomes disabled text.
    assert!(html.contains(">My Coins</a>"));
    assert!(!html.contains("action=\"/api/check-in\""));
    // daily_limit must not be smaller than amount, otherwise rule_reward skips the grant without writing a ledger
    // entry.
    gono_forum_web::db::query(
        "UPDATE gonof_common_credit_rule SET enabled=1,amount=5,daily_limit=5 WHERE rule_key='signin'",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();
    // Check-in rules live in the process-wide global cache: after changing the database directly, clear this
    // process's cache (saving in the admin area clears it itself).
    fixture._services.global.clear_local();
    let (_, _, ready) = fixture.call("GET", "/", Some(0), &[], false, ORIGIN).await;
    assert!(ready.contains("action=\"/api/check-in\""));
    assert!(ready.contains(">Check in +5</button>"));
    let (status, _, _) = fixture
        .call(
            "POST",
            "/api/check-in",
            Some(0),
            // Check-in is idempotent per member and Shanghai calendar day; the form only carries CSRF.
            &[("_csrf", fixture.csrf_tokens[0].clone())],
            false,
            ORIGIN,
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, done) = fixture.call("GET", "/", Some(0), &[], false, ORIGIN).await;
    assert!(done.contains(">Checked in today</span>"));
    assert!(!done.contains("action=\"/api/check-in\""));
    let (status, _, guest) = fixture.call("GET", "/", None, &[], false, ORIGIN).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!guest.contains("class=\"tally\""));
    assert!(!guest.contains("class=\"person__shortcuts\""));
    assert!(guest.contains("person__avatar--guest"));
}

/// A like without JS redirects 303 straight back to the floor instead of first loading all replies;
/// toggling favorites and likes, and marking and opening notifications, are rate limited per account, with 429 over
/// the limit.
#[tokio::test]
async fn interaction_toggles_and_reads_are_rate_limited_and_plain_votes_skip_the_reply_page() {
    use gono_forum_web::debug::trace::{self, RequestProfile};
    use std::sync::Arc;
    let f = Fixture::new().await;
    let endpoint = format!("/api/comments/{}/vote", f.first_pid);
    let profile = Arc::new(RequestProfile::new());
    let plain = trace::scope(
        profile.clone(),
        Box::pin(f.call("POST", &endpoint, Some(1), &f.state(1, true), false, ORIGIN)),
    )
    .await;
    assert_eq!(plain.0, StatusCode::SEE_OTHER);
    let replies = |profile: &RequestProfile| {
        profile
            .snapshot()
            .iter()
            .filter(|record| {
                record.sql.contains("ORDER BY p.root_position")
                    || record.sql.contains("ORDER BY p.position")
            })
            .count()
    };
    assert_eq!(
        replies(&profile),
        0,
        "a like without JS does not load the reply list"
    );
    let profile = Arc::new(RequestProfile::new());
    let htmx = trace::scope(
        profile.clone(),
        Box::pin(f.call("POST", &endpoint, Some(1), &f.state(1, false), true, ORIGIN)),
    )
    .await;
    assert_eq!(htmx.0, StatusCode::OK);
    assert!(
        replies(&profile) > 0,
        "an HTMX partial refresh loads it as usual"
    );

    // Already toggled twice above; 60 per account per minute.
    for n in 2..60 {
        let response = f
            .call(
                "POST",
                "/api/thread-103/star",
                Some(1),
                &f.state(1, n % 2 == 0),
                false,
                ORIGIN,
            )
            .await;
        assert_eq!(response.0, StatusCode::SEE_OTHER, "toggle {}", n + 1);
    }
    let limited = f
        .call("POST", &endpoint, Some(1), &f.state(1, true), false, ORIGIN)
        .await;
    assert_eq!(limited.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.2.contains("You are doing this too often"));
    // Other accounts are not affected.
    assert_eq!(
        f.call(
            "POST",
            "/api/thread-103/star",
            Some(0),
            &f.state(0, true),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );

    for _ in 0..60 {
        assert_eq!(
            f.call(
                "POST",
                "/my/notifications/read-all",
                Some(0),
                &f.read(0),
                false,
                ORIGIN
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );
    }
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/read-all",
            Some(0),
            &f.read(0),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        f.call(
            "POST",
            "/my/notifications/1/open",
            Some(0),
            &f.read(0),
            false,
            ORIGIN
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS,
        "opening a notification shares the quota with marking read"
    );
    f._services.cleanup_ref().await.unwrap();
}

/// Likes do not increment the thread's `cache_revision` or the site-wide content version, so they do not flush the
/// site-wide anonymous list cache (the version assertion checks this). Like counts are read fresh on every request
/// and are not in the floor and render caches, so anonymous visitors see the new count at once while the cache key
/// stays the same.
#[tokio::test]
async fn likes_reach_cached_anonymous_pages_without_bumping_cache_versions() {
    let f = Fixture::new().await;
    // Two anonymous visits: the first goes to the source and fills the floor and render caches, the second hits.
    for _ in 0..2 {
        let page = f.call("GET", "/thread-103", None, &[], false, ORIGIN).await;
        assert_eq!(page.0, StatusCode::OK);
        assert!(
            page.2.contains("data-reaction-count>0</span>"),
            "{}",
            page.2
        );
    }
    let before = cache_versions(&f.db).await;
    let endpoint = format!("/api/comments/{}/vote", f.first_pid);
    let vote = f
        .call("POST", &endpoint, Some(1), &f.state(1, true), true, ORIGIN)
        .await;
    assert_eq!(vote.0, StatusCode::OK);
    assert!(vote.2.contains("data-reaction-count>1</span>"));
    assert_eq!(
        cache_versions(&f.db).await,
        before,
        "a like bumps neither the thread revision nor the site content revision"
    );
    let page = f.call("GET", "/thread-103", None, &[], false, ORIGIN).await;
    assert_eq!(page.0, StatusCode::OK);
    assert!(
        page.2.contains("data-reaction-count>1</span>"),
        "{}",
        page.2
    );
    f._services.cleanup_ref().await.unwrap();
}

/// Thread 103's `cache_revision` and the site-wide content version, combined into one number for comparing as a
/// whole.
async fn cache_versions(db: &Database) -> i64 {
    gono_forum_web::db::query_scalar(
        "SELECT cache_revision * 1000000 + (SELECT revision FROM gonof_common_content_revision WHERE id=1) FROM gonof_forum_thread WHERE tid=103",
    )
    .fetch_one(db.executor())
    .await
    .unwrap()
}
