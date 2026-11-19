//! Forum hiding, blocking and personal feed URLs.
//!
//! Every filter exit has at least one assertion; settings are all submitted through the web forms (the blocked
//! notification one uses the data layer), so removing any filter condition makes the matching "cannot see" assertion
//! fail.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::{
        self,
        forum_filter::{self, FilterMode, ForumFilters},
        social,
    },
    notifications::{self, preferences},
    organization, router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: i64 = 9900;
const VIEWER: i64 = 9901;
const WRITER: i64 = 9902;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .map_or("", |value| value.to_str().unwrap())
    }
}

struct Fixture {
    services: TestFixture,
    app: Router,
    identity: Identity,
    viewer: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        for (uid, adminid) in [(ADMIN, 1), (VIEWER, 0), (WRITER, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid).bind(format!("filter-{uid}")).bind(format!("filter-{uid}@example.test")).bind(adminid)
                .execute(db.executor()).await.unwrap();
        }
        let config = services.config.clone();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let viewer = http_auth::login(&identity, &db, VIEWER).await;
        let app = router::build(config, &services.root, db.clone());
        Self {
            services,
            app,
            identity,
            viewer,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn fid(&self, slug: &str) -> i64 {
        db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug=?")
            .bind(slug)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    /// The fixture importer stores author names normalized to lower case ("Linden" is the member "linden").
    async fn uid(&self, username: &str) -> i64 {
        db::query_scalar("SELECT uid FROM gonof_common_member WHERE username=?")
            .bind(username)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.46:48322".parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn get(&self, path: &str, session: Option<&SessionGrant>) -> Reply {
        let mut builder = Request::builder().uri(path);
        if let Some(session) = session {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", session.session_token),
            );
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, fields: &[(&str, String)]) -> Reply {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &self.viewer.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", self.viewer.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
        )
        .await
    }

    /// Sets a level through the forum page's native form: `hide`, `block` or `none`.
    async fn set_filter(&self, fid: i64, mode: &str, location: &str) -> Reply {
        let reply = self
            .post(
                &format!("/api/forums/{fid}/filter"),
                &[
                    ("request_key", identity::random_token()),
                    ("mode", mode.into()),
                    ("location", location.into()),
                ],
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        reply
    }

    /// Turns on or regenerates the feed URL: 303 back to /my/feed, with the plaintext in a one-time reveal cookie sent
    /// only to this page; opening the settings page with it shows it once and clears the cookie, and later visits no
    /// longer show it. Returns the token plaintext and that cookie.
    async fn issue_feed(&self, path: &str) -> (String, String) {
        let issued = self.post(path, &[]).await;
        assert_eq!(issued.status, StatusCode::SEE_OTHER, "{}", issued.body);
        assert_eq!(issued.header(header::LOCATION), "/my/feed");
        assert!(!issued.body.contains("/feeds/private/"));
        let line = issued
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .find(|value| value.starts_with("gonof_feed_reveal__port58833="))
            .expect("creating must set the one-time reveal cookie")
            .to_owned();
        assert!(
            line.contains("; Path=/my/feed;") && line.contains("HttpOnly"),
            "{line}"
        );
        let reveal = line.split(';').next().unwrap().to_owned();
        let shown = self.feed_with(&reveal).await;
        assert_eq!(shown.status, StatusCode::OK);
        assert!(shown.body.contains("shown only this once"));
        assert!(
            shown
                .headers
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|value| value
                    .to_str()
                    .unwrap()
                    .starts_with("gonof_feed_reveal__port58833=;")),
            "the reveal page clears the cookie once read"
        );
        let token = token_in(&shown.body);
        assert!(!reveal.contains(&token), "the cookie holds ciphertext");
        // A refresh (the browser has deleted the cookie) no longer shows it, and does not regenerate it either.
        let again = self.viewer_get("/my/feed").await.body;
        assert!(!again.contains(&token) && again.contains(&token[..8]));
        (token, reveal)
    }

    /// Opens /my/feed with the session and the one-time reveal cookie.
    async fn feed_with(&self, reveal: &str) -> Reply {
        self.send(
            Request::builder()
                .uri("/my/feed")
                .header(
                    header::COOKIE,
                    format!(
                        "gonof_session__port58833={}; {reveal}",
                        self.viewer.session_token
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn viewer_get(&self, path: &str) -> Reply {
        let reply = self.get(path, Some(&self.viewer)).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
        reply
    }
}

/// Threads that appear in thread lists and the sidebar.
fn lists(body: &str, tid: i64) -> bool {
    body.contains(&format!("data-thread-id=\"{tid}\""))
        || body.contains(&format!("/thread-{tid}\""))
}

#[tokio::test]
async fn hidden_forums_leave_aggregates_but_stay_reachable() {
    let f = Fixture::new().await;
    let ai = f.fid("ai").await;
    // The ai forum: 104, 107, 113, 118, 122.
    assert!(lists(&f.viewer_get("/").await.body, 107));
    let reply = f.set_filter(ai, "hide", "/ai").await;
    assert_eq!(reply.header(header::LOCATION), "/ai");
    assert_eq!(
        db::query_scalar::<String>(
            "SELECT filter_mode FROM gonof_home_forum_filter WHERE uid=? AND fid=?"
        )
        .bind(VIEWER)
        .bind(ai)
        .fetch_one(f.db().executor())
        .await
        .unwrap(),
        "hide"
    );

    // The four home page views and the two sidebar cards.
    for path in ["/", "/?sort=created"] {
        let body = f.viewer_get(path).await.body;
        for tid in [104, 107, 113, 118, 122] {
            assert!(
                !lists(&body, tid),
                "{path} still lists thread {tid} of the hidden forum"
            );
        }
        assert!(lists(&body, 106), "{path} lists other forums as usual");
    }
    // The anonymous view and the shared cache are unaffected.
    assert!(lists(&f.get("/", None).await.body, 107));
    // Search.
    let search = format!(
        "/search?q={}",
        url::form_urlencoded::byte_serialize("Small models".as_bytes()).collect::<String>()
    );
    assert!(!lists(&f.viewer_get(&search).await.body, 113));
    assert!(lists(&f.get(&search, None).await.body, 113));
    // Entering the forum directly and opening threads work as usual.
    let forum = f.viewer_get("/ai").await.body;
    assert!(forum.contains("data-thread-id=\"107\""));
    assert!(forum.contains("data-forum-filter-note"));
    assert!(
        forum.contains("data-forum-filter=\"none\""),
        "after hiding there is a way to undo it"
    );
    let topic = f.viewer_get("/thread-107").await.body;
    assert!(!topic.contains("data-forum-blocked"));
    // Personal spaces do not filter hidden forums.
    let author = f.uid("linden").await;
    assert!(
        f.viewer_get(&format!("/space/{author}/threads"))
            .await
            .body
            .contains("thread-107")
    );
    // The management list.
    let list = f.viewer_get("/my/forum-filters").await.body;
    assert!(list.contains(&format!("data-forum-filter-row=\"{ai}\"")));
    assert!(list.contains("<span>Hidden</span>"));

    // After unhiding, the home page is restored.
    f.set_filter(ai, "none", "").await;
    assert!(lists(&f.viewer_get("/").await.body, 107));
    assert!(
        !f.viewer_get("/my/forum-filters")
            .await
            .body
            .contains("data-forum-filter-row")
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn blocked_parent_forum_disappears_everywhere_with_its_children() {
    let f = Fixture::new().await;
    let db = f.db();
    let hosting = f.fid("hosting").await;
    // hosting: 105 (by Sprout), 110, 114, 119, 123; 110 is moved into a newly created child forum.
    let child = organization::save_forum(
        db,
        actor(ADMIN),
        &organization::ForumInput {
            name: "Hosting subforum".into(),
            slug: "hosting-child".into(),
            parent_fid: hosting,
            visibility: "public".into(),
            status: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let version: i64 = db::query_scalar("SELECT version FROM gonof_forum_thread WHERE tid=110")
        .fetch_one(db.executor())
        .await
        .unwrap();
    organization::move_thread(
        db,
        actor(ADMIN),
        110,
        child,
        version,
        &identity::random_token(),
        "Test carrying children along",
    )
    .await
    .unwrap();
    // Before blocking, favorite and follow 105, and favorite 106 as a control.
    let interaction = interaction::Interaction::new(db.clone());
    for tid in [105, 106] {
        interaction
            .set_favorite(actor(VIEWER), tid, true, &identity::random_token())
            .await
            .unwrap();
    }
    social::set_follow_thread(db, actor(VIEWER), 105, true, &identity::random_token())
        .await
        .unwrap();

    f.set_filter(hosting, "block", "/hosting").await;

    // Home page and search: threads of neither the parent forum nor the child forum appear.
    let home = f.viewer_get("/").await.body;
    for tid in [105, 110, 114, 119, 123] {
        assert!(
            !lists(&home, tid),
            "the home page still lists thread {tid} of the blocked forum"
        );
    }
    let search = format!(
        "/search?q={}",
        url::form_urlencoded::byte_serialize("home server".as_bytes()).collect::<String>()
    );
    assert!(!lists(&f.viewer_get(&search).await.body, 105));
    // Directly visiting the forum, the child forum and threads: only the notice and the unblock entry.
    let forum = f.viewer_get("/hosting").await.body;
    assert!(forum.contains("data-forum-blocked"));
    assert!(!forum.contains("data-thread-id=\"105\""));
    let child_page = f.viewer_get("/hosting-child").await.body;
    assert!(
        child_page.contains("data-forum-blocked")
            && child_page.contains("You blocked the parent forum")
    );
    assert!(child_page.contains(&format!("/api/forums/{hosting}/filter")));
    for tid in [105, 110] {
        let topic = f.viewer_get(&format!("/thread-{tid}")).await.body;
        assert!(topic.contains("data-forum-blocked"), "thread {tid}");
    }
    assert!(
        !f.viewer_get("/thread-105")
            .await
            .body
            .contains("Notes from building a home server from scratch")
    );
    // Anonymous as usual.
    assert!(
        f.get("/hosting", None)
            .await
            .body
            .contains("data-thread-id=\"105\"")
    );
    // The posting forum picker.
    // The category drop-down also has numeric `<option value="…">` entries, so forums are recognized by name.
    let editor = f.viewer_get("/new").await.body;
    assert!(!editor.contains(&format!("<option value=\"{hosting}\">Hosting</option>")));
    assert!(!editor.contains(&format!(
        "<option value=\"{child}\">Hosting subforum</option>"
    )));
    assert!(editor.contains(&format!(
        "<option value=\"{}\">Daily</option>",
        f.fid("daily").await
    )));
    assert!(
        f.viewer_get("/new/hosting")
            .await
            .body
            .contains("data-forum-blocked")
    );
    // Personal space: Sprout's 105 does not appear, 111 in the daily forum appears as usual.
    let author = f.uid("sprout").await;
    let space = f.viewer_get(&format!("/space/{author}/threads")).await.body;
    assert!(!space.contains("thread-105") && space.contains("thread-111"));
    // Favorited and followed threads: placeholders, still removable.
    let favorites = f.viewer_get("/my/favorites").await.body;
    assert!(!favorites.contains("Notes from building a home server from scratch"));
    assert!(favorites.contains("What are you reading lately?"));
    let following = f.viewer_get("/my/following/threads").await.body;
    assert!(following.contains("Content not available"));
    assert!(!following.contains("Notes from building a home server from scratch"));
    // The management list notes the child forums carried along.
    let list = f.viewer_get("/my/forum-filters").await.body;
    assert!(
        list.contains("<span>Blocked</span>")
            && list.contains("Includes subforums: Hosting subforum")
    );

    // Unblock the parent forum from the child forum's notice page and return to the original page.
    let reply = f.set_filter(hosting, "none", "/hosting-child").await;
    assert_eq!(reply.header(header::LOCATION), "/hosting-child");
    assert!(
        f.viewer_get("/hosting")
            .await
            .body
            .contains("data-thread-id=\"105\"")
    );
    assert!(lists(&f.viewer_get("/").await.body, 105));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn filter_writes_are_idempotent_and_blocked_forums_skip_notifications() {
    let f = Fixture::new().await;
    let db = f.db();
    let hosting = f.fid("hosting").await;
    let daily = f.fid("daily").await;
    // Idempotency: replaying the same request key changes nothing; the same key with different content is a conflict;
    // forums that cannot be read cannot be set.
    let key = identity::random_token();
    forum_filter::set(
        db,
        &f.services.cache,
        actor(VIEWER),
        daily,
        Some(FilterMode::Hide),
        &key,
    )
    .await
    .unwrap();
    forum_filter::set(
        db,
        &f.services.cache,
        actor(VIEWER),
        daily,
        None,
        &identity::random_token(),
    )
    .await
    .unwrap();
    forum_filter::set(
        db,
        &f.services.cache,
        actor(VIEWER),
        daily,
        Some(FilterMode::Hide),
        &key,
    )
    .await
    .unwrap();
    assert!(
        ForumFilters::load(db, VIEWER, []).await.unwrap().is_empty(),
        "replaying an old request must not bring back a removed hide"
    );
    assert!(
        forum_filter::set(
            db,
            &f.services.cache,
            actor(VIEWER),
            daily,
            Some(FilterMode::Block),
            &key
        )
        .await
        .is_err()
    );
    assert!(
        forum_filter::set(
            db,
            &f.services.cache,
            actor(VIEWER),
            999_999,
            Some(FilterMode::Hide),
            &identity::random_token()
        )
        .await
        .is_err()
    );

    // Follow 105 (hosting), 106 (daily) and the hosting forum; mail is also turned on for the followed-threads kind.
    for tid in [105, 106] {
        social::set_follow_thread(db, actor(VIEWER), tid, true, &identity::random_token())
            .await
            .unwrap();
    }
    social::set_follow_forum(db, actor(VIEWER), hosting, true, &identity::random_token())
        .await
        .unwrap();
    let mut settings = preferences::get(db, actor(VIEWER)).await.unwrap();
    settings.email[3] = true;
    preferences::save(db, actor(VIEWER), &settings)
        .await
        .unwrap();
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    // Before blocking: on-site notifications as usual, mail queued.
    let before = reply(&writer, 105, "Reply before blocking").await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    assert_eq!(notices(db, before).await, 1);

    forum_filter::set(
        db,
        &f.services.cache,
        actor(VIEWER),
        hosting,
        Some(FilterMode::Block),
        &identity::random_token(),
    )
    .await
    .unwrap();
    // Mail already queued is re-checked when sending and skipped.
    gono_forum_web::test_support::deliver_mail(
        db,
        &f.services.root.join("filter-mail"),
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    let reason: String = db::query_scalar("SELECT last_error FROM gonof_common_outbox WHERE event_type='notification.email' AND aggregate_id=? AND status='failed' ORDER BY eventid DESC LIMIT 1")
        .bind(VIEWER)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(reason, "the recipient blocked the forum");

    // After blocking: replies and new threads in the blocked forum notify nobody; other forums as usual.
    let blocked = reply(&writer, 105, "Reply after blocking").await;
    let open = reply(&writer, 106, "Reply in another forum").await;
    let thread = writer
        .publish(
            WRITER,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid: hosting,
                title: "New thread in a blocked forum".into(),
                body: "Followed the forum but blocked it".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    assert_eq!(
        notices(db, blocked).await,
        0,
        "replies in a blocked forum notify nobody"
    );
    assert_eq!(
        notices(db, thread.pid).await,
        0,
        "blocking wins over following the forum"
    );
    assert_eq!(notices(db, open).await, 1);
    let queued: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.email' AND aggregate_id=? AND event_key=?")
        .bind(VIEWER)
        .bind(format!("notification.email:{blocked}:{VIEWER}"))
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(queued, 0, "mail for a blocked forum is not queued");
    // Notifications received before blocking become placeholders in the list.
    let filters = ForumFilters::load(db, VIEWER, []).await.unwrap();
    let page = notifications::list_in_category(db, actor(VIEWER), 1, 20, "", &filters)
        .await
        .unwrap();
    assert!(
        page.items
            .iter()
            .any(|n| n.pid == Some(open) && n.available)
    );
    assert!(
        !page.items.iter().any(|n| n.pid == Some(before)),
        "the notification from before blocking no longer shows the floor"
    );
    assert!(
        page.items
            .iter()
            .any(|n| !n.available && n.subject.is_empty()),
        "the notification from before blocking becomes a placeholder"
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn personal_feed_tokens_store_digests_rotate_disable_and_filter() {
    let f = Fixture::new().await;
    let db = f.db();
    let ai = f.fid("ai").await;
    let hosting = f.fid("hosting").await;
    // Hide ai, block hosting, block member Camellia (106, 112, 118, 124); 124 is changed to a members-only thread.
    f.set_filter(ai, "hide", "").await;
    f.set_filter(hosting, "block", "").await;
    let camellia = f.uid("camellia").await;
    social::set_block(db, actor(VIEWER), camellia, true, &identity::random_token())
        .await
        .unwrap();
    db::query("UPDATE gonof_forum_thread SET visibility='members' WHERE tid=120")
        .execute(db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();

    // Turn on: the plaintext appears only in this response; the database holds only the digest and the first 8
    // characters.
    let page = f.viewer_get("/my/feed").await.body;
    assert!(page.contains("Feed addresses are off."));
    let (token, enable_reveal) = f.issue_feed("/my/feed/enable").await;
    let row = db::query(
        "SELECT token_hash,token_hint,include_members FROM gonof_home_feed_token WHERE uid=?",
    )
    .bind(VIEWER)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(
        db::text(&row, "token_hash").unwrap(),
        identity::token_hash(&token)
    );
    assert_eq!(db::text(&row, "token_hint").unwrap(), token[..8]);
    assert_eq!(row.try_get::<i64, _>("include_members").unwrap(), 0);
    let stored: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_feed_token WHERE token_hash=? OR token_hint=?",
    )
    .bind(&token)
    .bind(&token)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(stored, 0, "the database does not store the plaintext");
    let later = f.viewer_get("/my/feed").await.body;
    assert!(!later.contains(&token) && later.contains(&token[..8]));
    let logged: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_log WHERE uid=? AND type='feed.token.create' AND data NOT LIKE ?")
        .bind(VIEWER)
        .bind(format!("%{}%", &token[..16]))
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(logged, 1);

    // Site-wide feed: forums that are hidden or blocked and blocked members are removed, members-only threads are not
    // included; the response does not let proxies keep a copy.
    let site = f
        .get(&format!("/feeds/private/{token}/feed.xml"), None)
        .await;
    assert_eq!(site.status, StatusCode::OK);
    assert!(
        site.header(header::CONTENT_TYPE)
            .starts_with("application/rss+xml")
    );
    assert_eq!(site.header(header::CACHE_CONTROL), "private, no-cache");
    assert_eq!(site.header(header::REFERRER_POLICY), "no-referrer");
    assert_eq!(
        site.header(header::HeaderName::from_static("x-robots-tag")),
        "noindex, nofollow"
    );
    assert!(
        site.header(header::SET_COOKIE).is_empty(),
        "feeds do not touch the session"
    );
    for tid in [107, 105, 106, 120] {
        assert!(
            !site.body.contains(&format!("/thread-{tid}<")),
            "the site-wide feed must not have {tid}"
        );
    }
    assert!(site.body.contains("/thread-111<"));
    // Entries carry their forum, as in the public feeds.
    assert!(
        !site.body.contains("<category></category>"),
        "{}",
        site.body
    );
    let atom = f
        .get(&format!("/feeds/private/{token}/atom.xml"), None)
        .await;
    assert!(
        atom.header(header::CONTENT_TYPE)
            .starts_with("application/atom+xml")
    );
    assert!(atom.body.contains("/thread-111<") || atom.body.contains("/thread-111\""));
    // Single-forum feed: hidden ones as usual, blocked ones 404.
    let ai_feed = f
        .get(&format!("/feeds/private/{token}/ai/feed.xml"), None)
        .await;
    assert_eq!(ai_feed.status, StatusCode::OK);
    assert!(ai_feed.body.contains("/thread-107<"));
    assert_eq!(
        f.get(&format!("/feeds/private/{token}/hosting/feed.xml"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Turn on "Include members-only threads".
    let saved = f
        .post("/my/feed/members", &[("include_members", "true".into())])
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    assert!(
        f.get(&format!("/feeds/private/{token}/feed.xml"), None)
            .await
            .body
            .contains("/thread-120<")
    );

    // Regenerate: the old URL is 404 at once and the switch goes back to off.
    let (fresh, _) = f.issue_feed("/my/feed/rotate").await;
    assert_ne!(fresh, token);
    // Even if the reveal cookie from the generation is still around, the replaced old URL is no longer shown.
    let stale = f.feed_with(&enable_reveal).await.body;
    assert!(!stale.contains(&token) && !stale.contains("data-feed-address"));
    assert_eq!(
        f.get(&format!("/feeds/private/{token}/feed.xml"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let fresh_site = f
        .get(&format!("/feeds/private/{fresh}/feed.xml"), None)
        .await;
    assert_eq!(fresh_site.status, StatusCode::OK);
    assert!(!fresh_site.body.contains("/thread-120<"));
    // 404 while the owner is frozen, restored after unfreezing.
    db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=?")
        .bind(VIEWER)
        .execute(db.executor())
        .await
        .unwrap();
    assert_eq!(
        f.get(&format!("/feeds/private/{fresh}/feed.xml"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    db::query("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=?")
        .bind(VIEWER)
        .execute(db.executor())
        .await
        .unwrap();
    assert_eq!(
        f.get(&format!("/feeds/private/{fresh}/feed.xml"), None)
            .await
            .status,
        StatusCode::OK
    );
    // Badly formatted tokens do not query the database.
    assert_eq!(
        f.get("/feeds/private/not-a-token/feed.xml", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Disable: the row is deleted and the URL is 404.
    let disabled = f.post("/my/feed/disable", &[]).await;
    assert_eq!(disabled.status, StatusCode::SEE_OTHER);
    assert_eq!(
        f.get(&format!("/feeds/private/{fresh}/feed.xml"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let rows: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_home_feed_token WHERE uid=?")
        .bind(VIEWER)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // Logging out of all devices invalidates it as well.
    let (again, _) = f.issue_feed("/my/feed/enable").await;
    f.identity
        .logout_all(&f.viewer.session_token)
        .await
        .unwrap();
    assert_eq!(
        f.get(&format!("/feeds/private/{again}/feed.xml"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    f.services.cleanup().await.unwrap();
}

/// A settings page that fails after the feed URL was issued (here its token query) keeps the one-time reveal cookie,
/// so a refresh can still show the URL instead of leaving the member to issue another.
#[tokio::test]
async fn a_failed_feed_page_keeps_the_reveal_cookie() {
    let f = Fixture::new().await;
    let issued = f.post("/my/feed/enable", &[]).await;
    assert_eq!(issued.status, StatusCode::SEE_OTHER, "{}", issued.body);
    let reveal = issued
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with("gonof_feed_reveal__port58833="))
        .expect("issuing sets the one-time reveal cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    db::raw_sql("ALTER TABLE gonof_home_feed_token RENAME TO gonof_home_feed_token_away")
        .execute(f.db().executor())
        .await
        .unwrap();
    let failed = f.feed_with(&reveal).await;
    db::raw_sql("ALTER TABLE gonof_home_feed_token_away RENAME TO gonof_home_feed_token")
        .execute(f.db().executor())
        .await
        .unwrap();
    assert!(failed.status.is_server_error(), "{}", failed.status);
    assert!(
        !failed
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value
                .to_str()
                .unwrap()
                .starts_with("gonof_feed_reveal__port58833=")),
        "a failed page keeps the cookie"
    );
    let shown = f.feed_with(&reveal).await;
    assert_eq!(shown.status, StatusCode::OK);
    assert!(
        shown.body.contains("shown only this once"),
        "{}",
        shown.body
    );
    let token = token_in(&shown.body);
    assert_eq!(
        f.get(&format!("/feeds/private/{token}/feed.xml"), None)
            .await
            .status,
        StatusCode::OK
    );
    f.services.cleanup().await.unwrap();
}

async fn reply(writer: &ContentWriter, tid: i64, body: &str) -> i64 {
    writer
        .reply(
            WRITER,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: body.into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .pid
}

async fn notices(db: &Database, pid: i64) -> i64 {
    db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE pid=? AND uid=?")
        .bind(pid)
        .bind(VIEWER)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

/// The token in the first personal feed URL on the page.
fn token_in(body: &str) -> String {
    let start = body
        .find("/feeds/private/")
        .expect("the page should show the feed address")
        + "/feeds/private/".len();
    body[start..start + 64].to_owned()
}
