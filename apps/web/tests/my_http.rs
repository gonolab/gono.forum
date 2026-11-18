//! HTTP contract of the personal center pages: login gate, second-level tabs, the right-hand member card, and an
//! overview that only adds numbers the card lacks.
#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{config::Config, content, db::Database, identity, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The in-page second-level tab section. First-level entries on the card also carry aria-current, so counting must
/// avoid them.
fn tab_bar(html: &str) -> &str {
    html.split_once("class=\"section-tabs\"")
        .expect("a personal center page must have second-level tabs")
        .1
        .split_once("</nav>")
        .expect("the tab navigation must be closed")
        .0
}

/// The fragment after the start of the personal center card. The card comes after the main content, so class names
/// from the main content do not mix in.
fn my_card<'a>(html: &'a str, path: &str) -> &'a str {
    html.split_once("class=\"panel panel--aside panel--keep person-card\"")
        .unwrap_or_else(|| panic!("{path} has no personal center card"))
        .1
}

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    db: Database,
    app: Router,
    session: String,
    csrf: String,
    origin: String,
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
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,bio,regdate) VALUES(9301,'my-http','synthetic-hash','my-http@example.test',1,'Bio with <b> angle brackets',1700000000)")
            .execute(db.executor())
            .await
            .unwrap();
        let identity = identity::Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let grant = http_auth::login(&identity, &db, 9301).await;
        let origin = gono_forum_web::test_support::TEST_SITE_URL.to_owned();
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
            session: grant.session_token,
            csrf: grant.csrf_token,
            origin,
        }
    }

    async fn post(&self, path: &str, fields: &[(&str, String)]) -> (StatusCode, String, String) {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("_csrf", &self.csrf)
            .extend_pairs(fields.iter().map(|(name, value)| (*name, value)))
            .finish();
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(
                        header::COOKIE,
                        format!("gonof_session__port58833={}", self.session),
                    )
                    .header(header::ORIGIN, &self.origin)
                    .header(
                        header::HOST,
                        gono_forum_web::test_support::host_of(&self.origin),
                    )
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let status = response.status();
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
        (status, location, body)
    }

    async fn get(&self, path: &str, signed_in: bool) -> (StatusCode, String, String) {
        let mut builder = Request::builder().uri(path);
        if signed_in {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.session),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
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
        (status, location, body)
    }
}

#[tokio::test]
async fn message_sending_limits_apply_across_compose_and_reply_routes() {
    let fixture = Fixture::new().await;
    for uid in 9401_i64..=9404 {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,1,1)")
            .bind(uid).bind(format!("limit-{uid}")).bind(format!("limit-{uid}@example.test"))
            .execute(fixture.db.executor()).await.unwrap();
    }
    let fields = |n: u64, uid: i64| {
        vec![
            ("username", format!("limit-{uid}")),
            ("body", format!("Rate limit test {n}")),
            ("request_key", format!("{n:064x}")),
        ]
    };
    let (status, location, _) = fixture.post("/my/messages/new", &fields(0, 9401)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let reply_path = format!("{location}/replies");
    for n in 1..10 {
        let path = if n % 2 == 0 {
            "/my/messages/new"
        } else {
            &reply_path
        };
        assert_eq!(
            fixture.post(path, &fields(n, 9401)).await.0,
            StatusCode::SEE_OTHER
        );
    }
    gono_forum_web::messages::send(
        &fixture.db,
        identity::Actor {
            uid: 9401,
            credentials_version: 1,
            api_key: None,
        },
        9301,
        "New message not opened yet",
        &"f".repeat(64),
    )
    .await
    .unwrap();
    // The same recipient cannot bypass the limit by alternating between the two entries, new conversation and reply.
    // Both entries return to their own form: the body is filled back as-is and not lost to rate limiting.
    let (status, _, html) = fixture.post(&reply_path, &fields(10, 9401)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(html.contains("You are sending private messages too often"));
    assert!(
        html.contains("Rate limit test 10</textarea>"),
        "the reply form did not keep the body"
    );
    // A rate-limited reply still renders the whole conversation, so the page carries this new message; since it is
    // shown, unread is cleared accordingly, just as when a reply is sent back for failing validation, leaving no red
    // dot pointing at already read content.
    assert!(
        html.contains("New message not opened yet"),
        "the rate-limited page must still show the conversation"
    );
    assert_eq!(
        gono_forum_web::messages::unread_conversations(&fixture.db, 9301)
            .await
            .unwrap(),
        0
    );
    let (status, _, html) = fixture.post("/my/messages/new", &fields(10, 9401)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(html.contains("You are sending private messages too often"));
    assert!(
        html.contains("Rate limit test 10</textarea>"),
        "the new message form did not keep the body"
    );
    assert!(
        html.contains("value=\"limit-9401\""),
        "the new message form did not keep the recipient"
    );
    // Changing recipients still counts toward the same account's rate-limit window (the two refused attempts before
    // also use up account quota).
    for n in 12..30 {
        let uid = if n < 22 { 9402 } else { 9403 };
        assert_eq!(
            fixture.post("/my/messages/new", &fields(n, uid)).await.0,
            StatusCode::SEE_OTHER
        );
    }
    assert_eq!(
        fixture.post("/my/messages/new", &fields(30, 9404)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let count: i64 = gono_forum_web::db::query_scalar("SELECT COUNT(*) FROM gonof_home_message")
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
    assert_eq!(count, 29);
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn sending_in_a_long_conversation_returns_to_the_latest_messages() {
    let fixture = Fixture::new().await;
    gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(9401,'long-conversation','synthetic-hash','long@example.test',1,1)")
        .execute(fixture.db.executor()).await.unwrap();
    let me = identity::Actor {
        uid: 9301,
        credentials_version: 1,
        api_key: None,
    };
    let mut thread = 0;
    for n in 0..20 {
        thread = gono_forum_web::messages::send(
            &fixture.db,
            me,
            9401,
            &format!("Message on the earliest page {n}"),
            &format!("{n:064x}"),
        )
        .await
        .unwrap();
    }
    let (status, location, _) = fixture
        .post(
            &format!("/my/messages/{thread}/replies"),
            &[
                ("body", "Latest message just sent".into()),
                ("request_key", "f".repeat(64)),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, _, latest) = fixture.get(&location, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(latest.contains("Latest message just sent"));
    assert!(!latest.contains("Message on the earliest page"));
    assert!(latest.contains(&format!("href=\"/my/messages/{thread}?page=1\"")));
    let (status, _, history) = fixture
        .get(&format!("/my/messages/{thread}?page=1"), true)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(history.contains("Message on the earliest page"));
    assert!(!history.contains("Latest message just sent"));
    assert!(history.contains(&format!("href=\"/my/messages/{thread}?page=2\"")));
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn overview_requires_login_and_shows_tabs_card_and_extra_counts() {
    let fixture = Fixture::new().await;

    // Anonymous access redirects to login with the return address.
    let (status, location, _) = fixture.get("/my", false).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/login?return_to=%2Fmy");

    gono_forum_web::db::query(
        "INSERT INTO gonof_home_thread_follow(uid,tid,dateline) VALUES(9301,103,100)",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();

    let (status, _, html) = fixture.get("/my", true).await;
    assert_eq!(status, StatusCode::OK);

    // Six tabs for the home entry; only the overview is current, and it has no icon.
    assert_eq!(html.matches("class=\"section-tabs__item\"").count(), 6);
    for title in [
        "Overview",
        "Coins",
        "Growth history",
        "Prizes",
        "Invites",
        "Public profile",
    ] {
        assert!(
            tab_bar(&html).contains(&format!(">{title}</a>")),
            "missing tab {title}"
        );
    }
    assert_eq!(
        html.matches("class=\"section-tabs__item\" href=\"/my\"")
            .count(),
        1
    );
    assert!(html.contains("href=\"/my\" aria-current=\"page\""));

    // On the right is the personal center's own card, separate from the home page's: four first-level entries, the
    // overview is the current page, and there are no numbers.
    assert!(!html.contains("class=\"panel panel--aside person-card\""));
    let card = html
        .split_once("class=\"panel panel--aside panel--keep person-card\"")
        .expect("the page must have the personal center card")
        .1;
    assert_eq!(card.matches("class=\"person-card__link\"").count(), 4);
    assert!(card.contains("href=\"/my\" aria-current=\"page\""));
    // The notifications entry also holds its unread dot, hidden while nothing is unread.
    for title in ["Home", "Content", "Notifications", "Messages"] {
        assert!(
            card.contains(&format!(">{title}</a>")) || card.contains(&format!(">{title}<span")),
            "the card lacks the entry {title}"
        );
    }
    // No more stat cells after the card: all six numbers have moved into this page's main content.
    assert_eq!(card.matches("class=\"tally__item\"").count(), 0);

    // The seven stat cells of the main content; followed threads count the one just inserted.
    assert_eq!(html.matches("class=\"tally__item\"").count(), 7);
    for label in [
        "Threads",
        "Replies",
        "Bookmarks",
        "Coins",
        "Frozen Coins",
        "Followed threads",
        "Followed forums",
    ] {
        assert!(
            html.contains(&format!(">{label}</dt>")),
            "the main content lacks {label}"
        );
    }
    assert!(html.contains("href=\"/my/following/threads\">1</a>"));
    assert!(html.contains("href=\"/my/following/forums\">0</a>"));

    // Private pages do not enter shared caches and are not indexed; the bio is escaped as plain text.
    assert!(html.contains("noindex"));
    // Askama escapes with numeric entities, so the bio renders as plain text and no tags leak out.
    assert!(html.contains("Bio with &#60;b&#62; angle brackets"));
    assert!(!html.contains("Bio with <b>"));
    // The fixture's regdate is 1700000000: 2023-11-14 22:13 in UTC, and the 15th 06:13 in the display time zone
    // (UTC+8). The registration date used to miss the time-zone conversion and rendered as the 14th in UTC, one time
    // zone off from other times on the site; this 8-hour window across midnight exposes exactly that.
    assert!(
        html.contains("2023-11-15"),
        "the registration date must be shown in the display time zone"
    );
    assert!(!html.contains("2023-11-14"));
}

#[tokio::test]
async fn settings_pages_share_the_personal_center_layout() {
    let fixture = Fixture::new().await;
    let pages = [
        ("/my/profile", "Profile"),
        ("/my/account", "Account security"),
        ("/my/api", "API"),
        ("/my/preferences", "Notification preferences"),
        ("/my/reading", "Reading preferences"),
        ("/my/forum-filters", "Forum filters"),
        ("/my/feed", "Feed address"),
    ];
    for (path, title) in pages {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        // Nine tabs in the settings section (personal style among them), with exactly one current item, this page. The settings icon on the card also carries aria-current, so only count within the tab section.
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            9,
            "{path}"
        );
        let tabs = tab_bar(&html);
        assert_eq!(tabs.matches("aria-current=\"page\"").count(), 1, "{path}");
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path}: its tab is not marked current"
        );
        for tab in [
            "Profile",
            "Account security",
            "API",
            "Notification preferences",
            "Reading preferences",
            "Blocked members",
            "Forum filters",
            "Feed address",
        ] {
            assert!(
                tabs.contains(&format!(">{tab}</a>")),
                "{path} lacks the tab {tab}"
            );
        }
        // The page name is only for screen readers; the page no longer repeats a visible title.
        assert!(
            html.contains(&format!("<h1 class=\"sr-only\">{title}</h1>")),
            "{path}"
        );
        assert_eq!(
            html.matches("<h1").count(),
            1,
            "{path} has more than one h1"
        );
        // On the right is the personal center card. Settings collapse into an icon, none of the four text links is
        // current, and the position is marked only by that icon; otherwise these pages could not be located on the
        // card.
        let card = my_card(&html, path);
        assert_eq!(
            card.matches("class=\"person-card__link\"").count(),
            4,
            "{path}"
        );
        assert_eq!(card.matches("aria-current").count(), 1, "{path}");
        assert!(
            card.contains("aria-label=\"Settings\" aria-current="),
            "{path}: the settings icon does not mark the current place"
        );
    }
    // The block list is also in the settings section, on the same tab bar as the pages above.
    let (status, _, html) = fixture.get("/my/blocked-users", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches("class=\"section-tabs__item\"").count(), 9);
    assert!(html.contains("href=\"/my/blocked-users\" aria-current=\"page\""));
}

#[tokio::test]
async fn content_pages_share_one_tab_bar_and_own_lists_show_hidden_items() {
    let fixture = Fixture::new().await;
    // The ten pages of the content section share one tab bar; threads and replies are private lists from the member's
    // own view. The two attachment pages are covered in member_center_http; followed members, followers and the follow
    // feed in interactions_plus_http.
    for path in [
        "/my/threads",
        "/my/posts",
        "/my/favorites",
        "/my/following/threads",
        "/my/following/forums",
        "/my/following/members",
        "/my/followers",
        "/my/following/feed",
        "/my/attachments",
        "/my/attachments/trash",
    ] {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            10,
            "{path}"
        );
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path}: its tab is not marked current"
        );
        for tab in [
            "Threads",
            "Replies",
            "Bookmarks",
            "Followed threads",
            "Followed forums",
            "Followed members",
            "Followers",
            "Following feed",
            "Attachments",
            "Attachment bin",
        ] {
            assert!(
                tab_bar(&html).contains(&format!(">{tab}</a>")),
                "{path} lacks the tab {tab}"
            );
        }
    }

    // Make one thread from the fixture the member's own and hide it: the public collection cannot see it, while the
    // member's own list shows it with its state.
    gono_forum_web::db::query("UPDATE gonof_forum_thread SET authorid=9301 WHERE tid=103")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    gono_forum_web::db::query(
        "UPDATE gonof_forum_post SET authorid=9301,invisible=1 WHERE tid=103 AND first=1",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let (_, _, html) = fixture.get("/my/threads", true).await;
    assert!(
        html.contains("<span class=\"tag\">Hidden</span>"),
        "the member's own list must show the hidden state"
    );
    assert!(
        html.contains("1 item, page 1."),
        "hidden threads count toward the member's own total"
    );
    let (_, _, public) = fixture.get("/space/9301/threads", false).await;
    assert!(!public.contains(">Hidden</span>"));

    // Anonymous access redirects to login and keeps the return address.
    let (status, location, _) = fixture.get("/my/posts", false).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/login?return_to=%2Fmy%2Fposts");
}

#[tokio::test]
async fn own_reply_status_follows_hidden_parent_and_hidden_thread() {
    let fixture = Fixture::new().await;
    let execute = |sql: &'static str| {
        let db = fixture.db.clone();
        async move {
            gono_forum_web::db::query(sql)
                .execute(db.executor())
                .await
                .unwrap();
        }
    };
    let child: i64 = gono_forum_web::db::query_scalar(
        "SELECT pid FROM gonof_forum_post WHERE tid=103 AND root_pid=103002 AND invisible=0 AND deleted_at=0 ORDER BY pid LIMIT 1",
    )
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_post SET authorid=9301 WHERE pid=?")
        .bind(child)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let (_, _, html) = fixture.get("/my/posts", true).await;
    assert!(html.contains("1 item, page 1."));
    // The excerpt in the member's own reply list is the whole post text, so it also uses the wrapping excerpt block
    // rather than the non-wrapping row-time.
    assert!(html.contains("<p class=\"row-excerpt\">"));
    assert!(!html.contains("<p class=\"row-time\">"));
    assert!(
        !html.contains("<span class=\"tag\">Hidden</span>"),
        "no state is shown while the parent floor and the thread are public"
    );

    // Parent floor hidden by governance: this floor disappears from the public collection along with it, and the
    // member's own list must mark it "Hidden".
    execute("UPDATE gonof_forum_post SET invisible=-1 WHERE pid=103002").await;
    let (_, _, html) = fixture.get("/my/posts", true).await;
    assert!(
        html.contains("<span class=\"tag\">Hidden</span>"),
        "the own reply is marked Hidden while its parent floor is hidden"
    );

    // Thread first post hidden by governance: the whole thread is invisible to others, also marked "Hidden".
    execute("UPDATE gonof_forum_post SET invisible=0 WHERE pid=103002").await;
    execute("UPDATE gonof_forum_post SET invisible=-1 WHERE tid=103 AND first=1").await;
    let (_, _, html) = fixture.get("/my/posts", true).await;
    assert!(
        html.contains("<span class=\"tag\">Hidden</span>"),
        "the own reply is marked Hidden while its thread is hidden"
    );
}

#[tokio::test]
async fn home_section_tabs_cover_credits_invites_and_public_profile() {
    let fixture = Fixture::new().await;
    for path in [
        "/my",
        "/my/credits",
        "/my/credits/growth",
        "/my/prizes",
        "/my/invites",
    ] {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            6,
            "{path}"
        );
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path}: its tab is not marked current"
        );
        for tab in [
            "Overview",
            "Coins",
            "Growth history",
            "Prizes",
            "Invites",
            "Public profile",
        ] {
            assert!(
                tab_bar(&html).contains(&format!(">{tab}</a>")),
                "{path} lacks the tab {tab}"
            );
        }
    }
    // The public profile has no separate page; the tab goes straight to the public personal space.
    let (status, location, _) = fixture.get("/my/public", true).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/space/9301");
    let (status, location, _) = fixture.get("/my/public", false).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/login?return_to=%2Fmy%2Fpublic");
}

#[tokio::test]
async fn notification_category_tabs_filter_in_place_and_keep_paging() {
    let fixture = Fixture::new().await;
    gono_forum_web::db::query(
        "INSERT INTO gonof_common_member(uid,username,regdate) VALUES(9302,'notice-actor',1)",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let pid: i64 = gono_forum_web::db::query_scalar(
        "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1",
    )
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    // Sanction notifications have an extra sanction_id constraint, so a review notification stands in for the system
    // category here.
    use gono_forum_web::notifications::record::{Kind, NewNotice, Target};
    for (kind, key, reason) in [
        (Kind::Reply, "k-reply", ""),
        (Kind::Mention, "k-mention", ""),
        (Kind::FollowThread, "k-follow", ""),
        (Kind::Moderation, "k-moderation", "Contract test"),
    ] {
        gono_forum_web::test_support::insert_notification(
            &fixture.db,
            &NewNotice {
                uid: 9301,
                authorid: 9302,
                kind,
                target: Target::Post(pid),
                event_key: key,
                reason,
                dateline: 100,
            },
        )
        .await
        .unwrap();
    }

    // All: four tabs, the first is current, and all four notifications are present.
    let (status, _, html) = fixture.get("/my/notifications", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches("class=\"section-tabs__item\"").count(), 4);
    assert!(html.contains("href=\"/my/notifications\" aria-current=\"page\""));
    for tab in ["All", "Replies and mentions", "Following", "System"] {
        assert!(
            tab_bar(&html).contains(&format!(">{tab}</a>")),
            "missing tab {tab}"
        );
    }

    // Each category keeps only its own notifications, and the current item switches with it; the address changes,
    // not the page.
    for (category, keep, drop) in [
        ("replies", "mentioned you", "Moderation result"),
        (
            "following",
            "replied to a thread you follow",
            "Moderation result",
        ),
        ("system", "Moderation result", "mentioned you"),
    ] {
        let path = format!("/my/notifications?category={category}");
        let (status, _, html) = fixture.get(&path, true).await;
        assert_eq!(status, StatusCode::OK, "{category}");
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{category}: its tab is not marked current"
        );
        assert!(html.contains(keep), "{category} lacks its notification");
        assert!(
            !html.contains(drop),
            "{category} shows a notification of another category"
        );
    }

    // A misspelled category is treated as all and normalized to the address without that parameter.
    let (status, location, _) = fixture
        .get("/my/notifications?category=nonsense", true)
        .await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(location, "/my/notifications");
}

#[tokio::test]
async fn affairs_pages_share_one_tab_bar_and_admin_views_keep_their_own_layout() {
    let fixture = Fixture::new().await;
    for (path, title) in [
        ("/my/reports", "Reports"),
        ("/my/restorations", "Restoration requests"),
        ("/my/sanctions", "Account record"),
        ("/my/data", "Data requests"),
    ] {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            4,
            "{path}"
        );
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path}: its tab is not marked current"
        );
        for tab in [
            "Reports",
            "Restoration requests",
            "Account record",
            "Data requests",
        ] {
            assert!(
                tab_bar(&html).contains(&format!(">{tab}</a>")),
                "{path} lacks the tab {tab}"
            );
        }
        assert!(
            html.contains(&format!("<h1 class=\"sr-only\">{title}</h1>")),
            "{path}"
        );
        assert_eq!(
            html.matches("<h1").count(),
            1,
            "{path} has more than one h1"
        );
        let card = my_card(&html, path);
        assert_eq!(
            card.matches("class=\"person-card__link\"").count(),
            4,
            "{path}"
        );
        assert_eq!(card.matches("aria-current").count(), 1, "{path}");
        assert!(
            card.contains("aria-label=\"My cases\" aria-current="),
            "{path}: the cases icon does not mark the current place"
        );
    }

    // The admin report review and account sanctions share the same main content but use the admin shell: the tabs are
    // those of the owning module (.section-tabs__item uses the same class names as the personal center), without the personal
    // center's member card.
    gono_forum_web::db::query("UPDATE gonof_common_member SET adminid=1 WHERE uid=9301")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    for (path, tabs) in [
        // Review management: report review, restoration requests, appeal handling
        ("/manage/reports", 3),
        // User management: users, registration sources, invites, account sanctions, data requests, verified members,
        // honor groups, verification notes
        ("/manage/sanctions", 8),
    ] {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            tabs,
            "{path}"
        );
        assert!(
            html.contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path}: its tab does not mark the current place"
        );
        // The admin shell does not carry a hidden top-level heading like the personal center, so the main content's
        // page title has to be the h1.
        assert_eq!(
            html.matches("<h1").count(),
            1,
            "{path} must have exactly one h1"
        );
        // The admin shell has its own left-hand module navigation; the personal center's member card must not appear.
        assert!(
            html.contains("class=\"admin-menu__item\""),
            "{path} lacks the admin module navigation"
        );
        assert!(
            !html.contains("class=\"panel panel--aside panel--keep person-card\""),
            "{path}"
        );
    }
}

#[tokio::test]
async fn member_appeals_live_under_sanctions_while_ticket_holders_keep_their_own_entry() {
    let fixture = Fixture::new().await;
    // The triggers require the operator and the sanctioned member to differ, warning to have no expiry, and the
    // request key to be 64 hex characters.
    gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,adminid,regdate) VALUES(9303,'appeal-operator','synthetic-hash',1,1)")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    // The appeal form only appears within 30 days of taking effect.
    gono_forum_web::db::query(&format!(
        "INSERT INTO gonof_common_member_sanction(uid,kind,reason,operator_uid,starts_at,expires_at,request_key) VALUES(9301,'warning','Contract test',9303,{},0,'{}')",
        identity::now() - 100,
        "a".repeat(64)
    ))
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let id: i64 = gono_forum_web::db::query_scalar(
        "SELECT id FROM gonof_common_member_sanction WHERE uid=9301",
    )
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();

    // Signed-in member: the appeal link in the account records points to an address inside the personal center.
    let (status, _, html) = fixture.get("/my/sanctions", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!("href=\"/my/sanctions/{id}/appeal\"")));
    assert!(!html.contains(&format!("href=\"/appeals/sanctions/{id}\"")));

    // That address can be opened directly and stays in the personal center layout.
    let (status, _, html) = fixture
        .get(&format!("/my/sanctions/{id}/appeal"), true)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Sanction appeal"));
    // Form submission and the back link stay in the personal center instead of jumping to the ticket path.
    let member_action = format!("action=\"/my/sanctions/{id}/appeal\"");
    assert!(html.contains(&member_action));
    assert!(!html.contains(&format!("action=\"/appeals/sanctions/{id}\"")));
    assert!(html.contains("href=\"/my/sanctions\""));
    // A signed-in member opening the /appeals address also lands back in the personal center on submission.
    let (status, _, html) = fixture.get(&format!("/appeals/sanctions/{id}"), true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&member_action));
    let (status, location, _) = fixture
        .post(
            &format!("/my/sanctions/{id}/appeal"),
            &[
                ("request_key", identity::random_token()),
                ("reason", "Please review this warning".into()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("/my/sanctions/{id}/appeal"));
    let (_, _, html) = fixture
        .get(&format!("/my/sanctions/{id}/appeal"), true)
        .await;
    assert!(html.contains("Please review this warning"));

    // Not signed in, the /appeals path is used: frozen accounts enter with an email ticket, bypassing the personal
    // center.
    let (status, location, _) = fixture
        .get(&format!("/my/sanctions/{id}/appeal"), false)
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/appeals");
}

#[tokio::test]
async fn private_messages_pair_once_mark_read_and_respect_blocks() {
    let fixture = Fixture::new().await;
    for uid in [9401_i64, 9402] {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,1,1)")
            .bind(uid)
            .bind(format!("dm-{uid}"))
            .bind(format!("dm-{uid}@example.test"))
            .execute(fixture.db.executor())
            .await
            .unwrap();
    }
    let me = gono_forum_web::identity::Actor {
        uid: 9301,
        credentials_version: 1,
        api_key: None,
    };
    let key = |n: u8| std::iter::repeat_n(char::from(b'a' + n), 64).collect::<String>();

    // The first send creates a conversation with one state row per side; the recipient's unread goes up by one, and
    // the sender's is zero.
    let thread = gono_forum_web::messages::send(&fixture.db, me, 9401, "First hello", &key(0))
        .await
        .unwrap();
    let rows: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_message_member WHERE thread_id=?",
    )
    .bind(thread)
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(
        gono_forum_web::messages::unread_conversations(&fixture.db, 9401)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        gono_forum_web::messages::unread_conversations(&fixture.db, 9301)
            .await
            .unwrap(),
        0
    );

    // Further messages between the same pair only append to the original conversation without creating a new one.
    let again = gono_forum_web::messages::send(&fixture.db, me, 9401, "One more question", &key(1))
        .await
        .unwrap();
    assert_eq!(again, thread);
    let threads: i64 =
        gono_forum_web::db::query_scalar("SELECT COUNT(*) FROM gonof_home_message_thread")
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
    assert_eq!(threads, 1);

    // Replaying the same key no longer adds to unread and writes no extra message.
    gono_forum_web::messages::send(&fixture.db, me, 9401, "One more question", &key(1))
        .await
        .unwrap();
    let messages: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_message WHERE thread_id=?",
    )
    .bind(thread)
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    assert_eq!(messages, 2);

    // Opening the conversation clears the member's own unread; the other side is unaffected.
    let view = gono_forum_web::messages::conversation(&fixture.db, 9401, thread, Some(1), 20)
        .await
        .unwrap();
    assert_eq!(view.messages.len(), 2);
    assert!(!view.messages[0].mine);
    assert_eq!(
        gono_forum_web::messages::unread_conversations(&fixture.db, 9401)
            .await
            .unwrap(),
        0
    );

    // A conversation that is not the member's own is treated as nonexistent.
    assert!(
        gono_forum_web::messages::conversation(&fixture.db, 9402, thread, Some(1), 20)
            .await
            .is_err()
    );

    // Refused after the other side blocks, and the message does not reveal the block; you cannot message yourself.
    gono_forum_web::db::query(
        "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES(9402,9301,1)",
    )
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let blocked = gono_forum_web::messages::send(&fixture.db, me, 9402, "Are you there", &key(2))
        .await
        .unwrap_err()
        .to_string();
    assert!(blocked.contains("does not accept"), "{blocked}");
    assert!(!blocked.to_lowercase().contains("block"), "{blocked}");
    assert!(
        gono_forum_web::messages::send(&fixture.db, me, 9301, "Talking to myself", &key(3))
            .await
            .is_err()
    );

    // Pages: the conversation list, conversation detail and new conversation are all under the private-message
    // first-level entry.
    for path in ["/my/messages", "/my/messages/new"] {
        let (status, _, html) = fixture.get(path, true).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.matches("class=\"section-tabs__item\"").count(),
            1,
            "{path}"
        );
        assert!(tab_bar(&html).contains(">Messages</a>"), "{path}");
    }
    let (status, _, html) = fixture.get(&format!("/my/messages/{thread}"), true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Conversation with dm-9401"));
    assert!(html.contains("First hello"));

    // The private-message cell on the card is now wired to the real unread count.
    gono_forum_web::messages::send(
        &fixture.db,
        gono_forum_web::identity::Actor {
            uid: 9401,
            credentials_version: 1,
            api_key: None,
        },
        9301,
        "Got it",
        &key(4),
    )
    .await
    .unwrap();
    let (_, _, html) = fixture.get("/my", true).await;
    // The card no longer shows a count; unread only shows a small dot.
    assert!(html.contains(">Messages<span class=\"person-card__dot\""));
    // The conversation list's excerpt is the entire last message (up to 2000 characters): it goes into a wrapping
    // excerpt block limited to two lines, and must not use the non-wrapping time style row-time, or long messages
    // would push the whole page into horizontal scrolling.
    let (_, _, html) = fixture.get("/my/messages", true).await;
    assert!(html.contains("<p class=\"row-excerpt\">Got it</p>"));

    // The read-only gate still returns the configured notice, and must not become a 500 because an error carrying
    // ownership is passed along.
    gono_forum_web::db::query("UPDATE gonof_common_member SET adminid=1 WHERE uid=9301")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    gono_forum_web::runtime::update(
        &fixture.db,
        &fixture._services.global,
        me,
        "site.settings.save",
        |s| {
            s.site.read_only = true;
            s.site.read_only_message = "Down for maintenance, please send messages later".into();
        },
    )
    .await
    .unwrap();
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("_csrf", &fixture.csrf)
        .append_pair("username", "dm-9401")
        .append_pair("body", "Not sent while read-only")
        .append_pair("request_key", &key(5))
        .finish();
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/my/messages/new")
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", fixture.session),
                )
                .header(header::ORIGIN, &fixture.origin)
                .header(
                    header::HOST,
                    gono_forum_web::test_support::host_of(&fixture.origin),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
    assert!(html.contains("Down for maintenance, please send messages later"));
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn unregistered_my_paths_stay_out_of_the_public_member_route() {
    let fixture = Fixture::new().await;
    // The personal center owns the /my prefix itself: unregistered sub-paths must land on 404 and must not render some
    // other page.
    for path in ["/my/", "/my/no-such-page", "/my/following/no-such-page"] {
        for signed_in in [false, true] {
            let (status, _, _) = fixture.get(path, signed_in).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{path} signed_in={signed_in}"
            );
        }
    }
}
