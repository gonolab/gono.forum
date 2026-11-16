//! Personal styles, the Simplified Chinese interface, site time zone, avatar shape and bio links. Members have no
//! display time zone of their own, so the time zone cases assert that the whole site displays and settles by the site
//! time zone.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::collections::BTreeMap;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities,
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity},
    router,
    test_support::{self, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
/// 2026-10-30 15:30 UTC: 23:30 the same day in UTC+8, 00:30 the next day in Tokyo.
const LATE_EVENING: i64 = 1_793_374_200;

struct Fixture {
    services: TestFixture,
    app: Router,
    identity: Identity,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        content::seed_forums(&services.db).await.unwrap();
        let identity = Identity::new(
            services.db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let app = router::build_with_global(
            services.config.clone(),
            &services.root,
            services.db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            identity,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    /// Creates a synthetic member and signs in, returning a jar with the session cookie and CSRF token.
    async fn member(&self, uid: i64, username: &str, bio: &str) -> Jar {
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate,bio) VALUES(?,?,'synthetic-hash',?,1,1,?)")
            .bind(uid)
            .bind(username)
            .bind(format!("member-{uid}@example.test"))
            .bind(bio)
            .execute(self.db().executor())
            .await
            .unwrap();
        let grant = http_auth::login(&self.identity, self.db(), uid).await;
        let mut jar = Jar::default();
        jar.cookies
            .insert("gonof_session".into(), grant.session_token.clone());
        jar.csrf = grant.csrf_token;
        jar
    }

    /// Anonymous visitor: GET once first to obtain the anonymous CSRF cookie.
    async fn anonymous(&self) -> Jar {
        let mut jar = Jar::default();
        send(&self.app, "GET", "/", &mut jar, &[], &[]).await;
        jar.csrf = jar.cookies.get("gonof_csrf").cloned().unwrap();
        jar
    }

    async fn scalar(&self, sql: &str) -> Option<String> {
        db::query_scalar::<Option<String>>(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn number(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn thread(&self, uid: i64, title: &str, body: &str, at: i64) -> i64 {
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(self.db().executor())
            .await
            .unwrap();
        ContentWriter::new(self.db().clone(), WriteConfig::default())
            .publish_at(
                uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap()
            .tid as i64
    }
}

#[derive(Clone, Default)]
struct Jar {
    cookies: BTreeMap<String, String>,
    csrf: String,
}

impl Jar {
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
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.into(), value.into());
            }
        }
    }

    fn header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    jar: &mut Jar,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, jar.header());
    let mut form = String::new();
    if method == "POST" {
        builder = builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, test_support::host_of(ORIGIN));
        form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("_csrf", &jar.csrf)
            .extend_pairs(fields.iter().copied())
            .finish();
    }
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(form)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    jar.update(&headers);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

async fn get(app: &Router, path: &str, jar: &mut Jar) -> Reply {
    send(app, "GET", path, jar, &[], &[]).await
}

const LINK: &str = "data-custom-style>";

// ---------------------------------------------------------------------------
// Personal styles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn custom_css_is_validated_saved_served_only_to_its_owner_and_reset() {
    let f = Fixture::new().await;
    let mut owner = f.member(9801, "style-owner", "").await;
    let mut other = f.member(9802, "style-other", "").await;
    let mut anonymous = f.anonymous().await;

    let login = get(&f.app, "/my/style", &mut anonymous).await;
    assert_eq!(login.status, StatusCode::SEE_OTHER);
    let page = get(&f.app, "/my/style", &mut owner).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("data-style-form") && page.body.contains("Up to 16 KiB."));

    // Anything invalid rejects the whole block, reports the line number, and leaves the database unchanged.
    for (css, reason) in [
        (
            ".a { color: red; }\nbody { background: url(https://evil.example/x.png); }",
            "Line 2: url() is not allowed",
        ),
        (
            "@import url(https://evil.example/x.css);",
            "Line 1: @import is not allowed",
        ),
        (
            "@font-face { font-family: x; }",
            "@font-face is not allowed",
        ),
        (
            ".a { width: expression(alert(1)); }",
            "expression() is not allowed",
        ),
        (
            "body::after { position: fixed; inset: 0; z-index: 60; }",
            "position: fixed is not allowed",
        ),
        (
            "h1::before { content: \"Please enter your password again\"; }",
            "content can only be",
        ),
        (
            ".a { background: u\\72l(x); }",
            "Backslash escapes are not allowed",
        ),
    ] {
        let reply = send(
            &f.app,
            "POST",
            "/my/style",
            &mut owner,
            &[("version", "0"), ("css", css)],
            &[],
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{css}");
        assert!(reply.body.contains(reason), "{css}: {}", reply.body);
        assert!(
            reply.body.contains("data-style-form"),
            "the rejected text is filled back in"
        );
    }
    // A style that is one long run of opening brackets is refused at the nesting limit. The validator recurses
    // once per level on the request's worker thread, so recursing into all of them would overflow the stack and
    // abort the server; the requests after this one show that it is still running.
    let room = gono_forum_web::custom_css::MAX_BYTES - 32;
    for css in [
        format!(".a {{ width: {}", "(".repeat(room)),
        format!(".a{} {{ color: red }}", ":not(".repeat(room / 5)),
        format!("@media {}", "(".repeat(room)),
    ] {
        let reply = send(
            &f.app,
            "POST",
            "/my/style",
            &mut owner,
            &[("version", "0"), ("css", &css)],
            &[],
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            reply
                .body
                .contains("Line 1: Brackets and functions can be nested at most 16 levels deep"),
            "{}",
            reply.body
        );
    }
    assert_eq!(
        f.number("SELECT COUNT(*) FROM gonof_home_reading_preference WHERE uid=9801")
            .await,
        0
    );

    let css = ".row-title { color: rebeccapurple; }\n:root { --r-sm: 0; }";
    let saved = send(
        &f.app,
        "POST",
        "/my/style",
        &mut owner,
        &[("version", "0"), ("css", &css.replace('\n', "\r\n"))],
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.headers[header::LOCATION], "/my/style?saved=true");
    assert_eq!(
        f.scalar("SELECT custom_css FROM gonof_home_reading_preference WHERE uid=9801")
            .await
            .as_deref(),
        Some(css),
        "the submitted text is stored, with line endings normalized to LF"
    );
    let log = f
        .scalar("SELECT data FROM gonof_common_log WHERE uid=9801 AND type='identity.custom_css'")
        .await
        .unwrap();
    assert!(
        log.contains("\"version\":1") && !log.contains("rebeccapurple"),
        "{log}"
    );
    let stale = send(
        &f.app,
        "POST",
        "/my/style",
        &mut owner,
        &[("version", "0"), ("css", ".a{color:red}")],
        &[],
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);

    // The member's own regular pages carry the stylesheet; the edit page, account security and error pages do not.
    let href = "href=\"/my/style.css?u=9801&#38;v=1\" data-custom-style>";
    for path in ["/", "/my/reading", "/space/9801", "/daily"] {
        let reply = get(&f.app, path, &mut owner).await;
        assert!(reply.body.contains(href), "{path}");
    }
    for path in ["/my/style", "/my/account", "/thread-999999", "/my/data"] {
        let reply = get(&f.app, path, &mut owner).await;
        assert!(
            !reply.body.contains(LINK),
            "{path} does not apply the personal style"
        );
    }
    // Others and visitors viewing their profile or the home page get none.
    for jar in [&mut other, &mut anonymous] {
        for path in ["/space/9801", "/"] {
            let reply = get(&f.app, path, jar).await;
            assert_eq!(reply.status, StatusCode::OK);
            assert!(!reply.body.contains(LINK), "{path}");
        }
    }

    // The stylesheet is only for the member; it is cached long only when the version matches.
    let sheet = get(&f.app, "/my/style.css?u=9801&v=1", &mut owner).await;
    assert_eq!(sheet.status, StatusCode::OK);
    assert_eq!(
        sheet.headers[header::CONTENT_TYPE],
        "text/css; charset=utf-8"
    );
    assert_eq!(
        sheet.headers[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(sheet.body, css);
    let old = get(&f.app, "/my/style.css?u=9801&v=0", &mut owner).await;
    assert_eq!(old.headers[header::CACHE_CONTROL], "private, no-store");
    for jar in [&mut other, &mut anonymous] {
        let reply = get(&f.app, "/my/style.css?u=9801&v=1", jar).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert!(!reply.body.contains("rebeccapurple"));
    }

    // Reset: the source is cleared, the version still increments, and the stylesheet becomes empty.
    let reset = send(
        &f.app,
        "POST",
        "/my/style/reset",
        &mut owner,
        &[("version", "1")],
        &[],
    )
    .await;
    assert_eq!(reset.status, StatusCode::SEE_OTHER);
    assert_eq!(reset.headers[header::LOCATION], "/my/style?reset=true");
    assert_eq!(
        f.scalar("SELECT custom_css FROM gonof_home_reading_preference WHERE uid=9801")
            .await,
        None
    );
    assert_eq!(
        f.number("SELECT custom_css_version FROM gonof_home_reading_preference WHERE uid=9801")
            .await,
        2
    );
    let empty = get(&f.app, "/my/style.css?u=9801&v=2", &mut owner).await;
    assert_eq!((empty.status, empty.body.as_str()), (StatusCode::OK, ""));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn pages_ignore_old_language_cookies_and_preserve_the_authors_text() {
    let f = Fixture::new().await;
    let mut member = f.member(9811, "hair-author", "").await;
    // Mixed Simplified and Traditional Chinese: the title "\u{5934}\u{53d1}\u{8207}\u{982d}\u{9aee}" and the body
    // "\u{53d1}\u{5e03}\u{8207}\u{767c}\u{4f48}" ("hair" and "publish", each written both ways) must come back exactly
    // as written, whatever language cookie the request carries.
    let title = "\u{5934}\u{53d1}\u{8207}\u{982d}\u{9aee}";
    let body = "\u{53d1}\u{5e03}\u{8207}\u{767c}\u{4f48}";
    let tid = f.thread(9811, title, body, identity::now() - 60).await;
    let mut guest = f.anonymous().await;
    for jar in [&mut guest, &mut member] {
        jar.cookies.insert("gonof_lang".into(), "zh-Hant".into());
        let page = get(&f.app, &format!("/thread-{tid}"), jar).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("<html lang=\"en\""));
        assert!(page.body.contains(title));
        assert!(page.body.contains(body));
        assert!(!page.body.contains("data-language-toggle"));
        let list = send(&f.app, "GET", "/", jar, &[], &[("HX-Request", "true")]).await;
        assert_eq!(list.status, StatusCode::OK);
        assert!(list.body.contains(title));
        let removed = send(
            &f.app,
            "POST",
            "/language",
            jar,
            &[("language", "zh-Hant")],
            &[],
        )
        .await;
        assert_eq!(removed.status, StatusCode::METHOD_NOT_ALLOWED);
    }
    let page = get(&f.app, "/my/reading", &mut member).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("name=\"language\""));
    f.services.cleanup_ref().await.unwrap();
}

// ---------------------------------------------------------------------------
// Member display time zone and avatar shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn member_time_zone_is_gone_and_everyone_sees_site_time() {
    let f = Fixture::new().await;
    let mut author = f.member(9821, "tz-author", "").await;
    let mut viewer = f.member(9822, "tz-viewer", "").await;
    let mut anonymous = f.anonymous().await;
    let tid = f
        .thread(9821, "Time zone thread", "First post", LATE_EVENING - 3600)
        .await;
    let reply = ContentWriter::new(f.db().clone(), WriteConfig::default())
        .reply_at(
            9821,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A late-night reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            LATE_EVENING,
        )
        .await
        .unwrap();

    // Members have no personal time zone (the reading preferences have no timezone column): an extra timezone in the
    // form means nothing, and the page has no such option; everyone sees times in the site time zone.
    let saved = send(
        &f.app,
        "POST",
        "/my/reading",
        &mut viewer,
        &[
            ("version", "0"),
            ("list_sort", "latest"),
            ("thread_view", "nested"),
            ("timezone", "Asia/Tokyo"),
            ("avatar_shape", "circle"),
        ],
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let page = get(&f.app, "/my/reading", &mut viewer).await;
    assert!(!page.body.contains("name=\"timezone\""));
    assert!(!page.body.contains("Asia/Tokyo"));
    assert!(
        page.body
            .contains("Times on pages always follow the site time zone.")
    );

    // Floor times are "month-day hour:minute": first post 14:30 UTC and reply 15:30 UTC, i.e. 22:30 and 23:30 Beijing
    // time. Each is a <time class="row-time"> whose datetime is the instant in UTC, so the shown text follows it.
    let path = format!("/thread-{tid}");
    let tokyo = get(&f.app, &path, &mut viewer).await;
    for jar in [&mut anonymous, &mut author] {
        let shanghai = get(&f.app, &path, jar).await;
        assert!(
            shanghai.body.contains("T14:30:00Z\">10-30 22:30</time>"),
            "site time zone: UTC+8"
        );
        assert!(shanghai.body.contains("T15:30:00Z\">10-30 23:30</time>"));
        assert!(!shanghai.body.contains("10-31 00:30"));
    }
    assert!(
        tokyo.body.contains("T15:30:00Z\">10-30 23:30</time>"),
        "the member who chose Tokyo also sees site time"
    );
    assert!(!tokyo.body.contains("10-31 00:30"));
    // Storage is unchanged: still UTC seconds.
    assert_eq!(
        f.number(&format!(
            "SELECT dateline FROM gonof_forum_post WHERE pid={}",
            reply.pid
        ))
        .await,
        LATE_EVENING
    );

    // A round avatar only changes what the member sees.
    assert!(tokyo.body.contains("data-avatar-shape=\"circle\">"));
    let shanghai = get(&f.app, &path, &mut author).await;
    assert!(!shanghai.body.contains("data-avatar-shape"));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn signin_day_boundary_follows_the_site_timezone() {
    let f = Fixture::new().await;
    let mut member = f.member(9831, "tz-signin", "").await;
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate,adminid) VALUES(9830,'tz-admin','synthetic-hash','tz-admin@example.test',1,1,1)")
        .execute(f.db().executor())
        .await
        .unwrap();
    let admin = Actor {
        uid: 9830,
        credentials_version: 1,
        api_key: None,
    };
    activities::update_rule(
        f.db(),
        &f.services.global,
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
    let saved = send(
        &f.app,
        "POST",
        "/my/reading",
        &mut member,
        &[
            ("version", "0"),
            ("list_sort", "latest"),
            ("thread_view", "nested"),
        ],
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    let actor = Actor {
        uid: 9831,
        credentials_version: 1,
        api_key: None,
    };
    // 15:30 and 16:30 UTC are the same day in UTC but cross midnight in UTC+8 (the seeded site time zone): check-in
    // counts twice by site day.
    assert!(
        activities::check_in(f.db(), actor, LATE_EVENING)
            .await
            .unwrap()
    );
    assert!(
        activities::check_in(f.db(), actor, LATE_EVENING + 3600)
            .await
            .unwrap()
    );
    assert!(
        !activities::check_in(f.db(), actor, LATE_EVENING + 3660)
            .await
            .unwrap()
    );
    let day = |time: i64| (time + 8 * 3600).div_euclid(86400);
    let keys: Vec<String> = db::query_scalar(
        "SELECT event_key FROM gonof_common_credit_ledger WHERE uid=9831 AND kind='signin' ORDER BY id",
    )
    .fetch_all(f.db().executor())
    .await
    .unwrap();
    assert_eq!(
        keys,
        [
            format!("signin:9831:{}", day(LATE_EVENING)),
            format!("signin:9831:{}", day(LATE_EVENING + 3600)),
        ]
    );
    // Site time zone changed to UTC: the same 15:30 and 16:30 UTC fall on the same UTC day, so only one check-in
    // succeeds. Check-in reads the write gate inside the transaction, so a database change takes effect immediately.
    db::query(
        "UPDATE gonof_common_settings SET svalue='0' WHERE skey='settings.time.offset_minutes'",
    )
    .execute(f.db().executor())
    .await
    .unwrap();
    let next = LATE_EVENING + 2 * 86400;
    assert!(activities::check_in(f.db(), actor, next).await.unwrap());
    assert!(
        !activities::check_in(f.db(), actor, next + 3600)
            .await
            .unwrap(),
        "on a UTC site 16:30 and 15:30 are the same day"
    );
    let last: String = db::query_scalar(
        "SELECT event_key FROM gonof_common_credit_ledger WHERE uid=9831 AND kind='signin' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(f.db().executor())
    .await
    .unwrap();
    assert_eq!(last, format!("signin:9831:{}", next.div_euclid(86400)));
    f.services.cleanup_ref().await.unwrap();
}

// ---------------------------------------------------------------------------
// Bio links
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bio_links_carry_the_agreed_attributes_and_limits() {
    let f = Fixture::new().await;
    let mut owner = f
        .member(
            9841,
            "bio-owner",
            // "\u{ff0c}" is a full-width comma: the link must end before it.
            "Blog https://blog.example.com/a?x=1&y=2\u{ff0c}intranet http://192.168.1.1/ more https://b.example.org/ https://c.example.org/ https://d.example.org/ <b>bold</b>",
        )
        .await;
    let mut anonymous = f.anonymous().await;
    let rel = "rel=\"nofollow ugc noopener noreferrer\" target=\"_blank\" translate=\"no\"";
    for (jar, path) in [(&mut anonymous, "/space/9841"), (&mut owner, "/my")] {
        let page = get(&f.app, path, jar).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        assert!(
            page.body.contains(&format!(
                "<a href=\"https://blog.example.com/a?x=1&#38;y=2\" {rel} title=\"https://blog.example.com/a?x=1&#38;y=2\">https://blog.example.com/a?x=1&#38;y=2</a>\u{ff0c}intranet http://192.168.1.1/ more"
            )),
            "{path}"
        );
        assert_eq!(page.body.matches(rel).count(), 3, "at most 3 links: {path}");
        assert!(
            page.body
                .contains(" https://d.example.org/ &#60;b&#62;bold&#60;/b&#62;")
        );
        assert!(!page.body.contains("href=\"http://192.168.1.1/"));
    }
    f.services.cleanup_ref().await.unwrap();
}
