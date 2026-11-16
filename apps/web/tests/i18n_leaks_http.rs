//! Missing-translation probe (docs/architecture.md, "Languages"): with the site in English and a fixture without any
//! Chinese (forum names, user names, site name), pages must not contain Han characters. The tests cover the page
//! frame (the `<html>` start tag, everything from `<body>` up to `<main>`: skip link, header, mobile menu, banners;
//! the footer and the section labels of the two layouts) and the script catalog `/static/i18n/en.js`; the whole
//! public reading pages (home, forums, threads, search, profiles, the site pages and error pages); the account and
//! settings pages, the personal center, the editor, the six kinds of thread with their activity cards, the points and
//! attachment pages and built-in mail; and the admin area (`/manage`) for every staff role, its saved notices and form
//! errors, and the debug panel, each checked whole.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Identity, SessionGrant},
    test_support::{TestFixture, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "I18n-leak-probe-password-2026!";
const ORIGIN: &str = "http://127.0.0.1:58833";
/// Members of the member pages probe: one posts the six kinds of thread, the other takes part.
const MEMBER: i64 = 9403;
const OTHER: i64 = 9404;
const ADMIN: i64 = 9400;
/// A member whose email is not verified yet: the header shows the verification banner.
const UNVERIFIED: i64 = 9401;
/// A muted member: the header shows the muted banner.
const MUTED: i64 = 9402;

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn member(db: &Database, uid: i64, name: &str, role: i64, verified: bool) {
    let salt = SaltString::encode_b64(b"i18n-leak-probe-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(uid)
        .bind(name)
        .bind(format!("{name}@example.test"))
        .bind(hash)
        .bind(i64::from(verified))
        .bind(role)
        .bind(identity::now() - 30 * 86_400)
        .execute(db.executor())
        .await
        .unwrap();
}

/// Characters of the Han script and the CJK punctuation and full-width forms that come with Chinese text.
fn han(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| {
            matches!(u32::from(*c),
                0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
        })
        .collect()
}

/// The part of the page from `start` up to the next `end` after it (with `end`); an empty string when `start` is
/// absent.
fn section<'a>(page: &'a str, start: &str, end: &str) -> &'a str {
    match page.find(start) {
        Some(at) => {
            let stop = page[at..]
                .find(end)
                .map_or(page.len(), |found| at + found + end.len());
            &page[at..stop]
        }
        None => "",
    }
}

/// The frame regions of one page (see the module documentation).
fn frame(page: &str) -> Vec<(&'static str, &str)> {
    vec![
        ("<html>", section(page, "<html", ">")),
        ("header", section(page, "<body", "<main")),
        ("footer", section(page, "<footer", "</footer>")),
        (
            "admin layout",
            section(page, "<nav class=\"admin-menu\" aria-label=\"", "\">"),
        ),
        // The personal center layout's sidebar; the home page sidebar has the same class but is page content.
        (
            "my layout",
            if page.contains("panel--keep person-card") {
                section(page, "<aside class=\"layout__aside\" aria-label=\"", "\">")
            } else {
                ""
            },
        ),
    ]
}

struct Http {
    app: Router,
    identity: Identity,
}

impl Http {
    async fn page(&self, path: &str, grant: Option<&SessionGrant>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(grant) = grant {
            request = request.header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// A form post the way a browser sends it: CSRF token, same-origin headers and a client address.
    async fn post(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &grant.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
            .body(Body::from(form.finish()))
            .unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            "192.0.2.44:48321".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn login(&self, name: &str) -> SessionGrant {
        self.identity.login(name, PASSWORD).await.unwrap()
    }
}

/// The English site of the probe: the English sample site with the dates following the language, the brand words
/// other than the site name left to the language's defaults, and twelve forums so that the header needs "More".
async fn english_site(f: &TestFixture) {
    for (key, value) in [
        ("settings.time.date_format", "locale"),
        ("settings.site.description", ""),
        ("settings.site.read_only_message", ""),
        ("settings.growth.level_names", ""),
        ("settings.site.brand_text", ""),
        ("settings.site.credit_name", ""),
        ("settings.site.index_name", ""),
        ("settings.time.relative", "true"),
    ] {
        set_setting(&f.db, key, value).await.unwrap();
    }
    content::seed_forums(&f.db).await.unwrap();
    for extra in 0..6 {
        execute(
            &f.db,
            &format!(
                "INSERT INTO gonof_forum_forum (name, slug, description, guidance, displayorder, status, visibility) VALUES ('Extra forum {extra}', 'extra-{extra}', '', '', {}, 1, 'public')",
                100 + extra
            ),
        )
        .await;
    }
    member(&f.db, ADMIN, "probe-admin", 1, true).await;
    member(&f.db, UNVERIFIED, "probe-unverified", 0, false).await;
    member(&f.db, MUTED, "probe-muted", 0, true).await;
    db::query("UPDATE gonof_common_member SET mute_until=? WHERE uid=?")
        .bind(identity::now() + 86_400)
        .bind(MUTED)
        .execute(f.db.executor())
        .await
        .unwrap();
    f.global.clear_local();
}

#[tokio::test]
async fn english_frame_has_no_untranslated_text() {
    let f = TestFixture::migrated().await.unwrap();
    english_site(&f).await;
    let http = Http {
        app: gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        ),
        identity: Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        ),
    };
    let admin = http.login("probe-admin").await;
    let unverified = http.login("probe-unverified").await;
    let muted = http.login("probe-muted").await;
    let mut checked = 0;
    let mut leaks = Vec::new();
    for read_only in [false, true] {
        set_setting(
            &f.db,
            "settings.site.read_only",
            if read_only { "true" } else { "false" },
        )
        .await
        .unwrap();
        f.global.clear_local();
        for (path, grant) in [
            ("/", None),
            ("/forums", None),
            ("/login", None),
            ("/register", None),
            ("/search?q=probe", None),
            ("/rules", None),
            ("/", Some(&admin)),
            ("/my", Some(&admin)),
            ("/manage", Some(&admin)),
            ("/", Some(&unverified)),
            ("/", Some(&muted)),
            ("/my/sanctions", Some(&muted)),
        ] {
            let (status, page) = http.page(path, grant).await;
            assert!(status.is_success(), "{path}: {status}");
            assert!(
                page.starts_with("<!doctype html>\n<html lang=\"en\" data-base="),
                "{path}"
            );
            for (region, text) in frame(&page) {
                let found = han(text);
                if !found.is_empty() {
                    leaks.push(format!(
                        "{path} ({region}): {}",
                        found.iter().collect::<String>()
                    ));
                }
            }
            checked += 1;
        }
    }
    assert!(
        leaks.is_empty(),
        "Chinese text left in the frame: {leaks:#?}"
    );
    assert_eq!(checked, 24);

    // The banners and the menus the probe meant to cover were really on the pages.
    let (_, page) = http.page("/", Some(&unverified)).await;
    assert!(page.contains("Verify your email to create invites and post."));
    let (_, page) = http.page("/", Some(&muted)).await;
    let banner = section(&page, "data-mute-banner>", "</p>");
    assert!(banner.contains("You are muted until "), "{banner}");
    assert!(
        banner.contains(". <a href=\"/my/sanctions\">See records and appeal</a>"),
        "{banner}"
    );
    let (_, page) = http.page("/", None).await;
    assert!(page.contains("<summary class=\"masthead__board forum-more__toggle\">More<svg"));
    assert!(page.contains(">All forums</a>"));
    assert!(
        page.contains(">Gono Forum</a>"),
        "an empty header brand text shows the product name"
    );
    assert!(page.contains("<title>Sample Community</title>"));
    assert!(page.contains("<meta property=\"og:locale\" content=\"en_US\">"));
    assert!(page.contains("/static/i18n/en.js?v="));

    // The script catalog of the frame's scripts.
    let (status, script) = http.page("/static/i18n/en.js?v=1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(han(&script).is_empty(), "{script}");
    assert!(script.contains("\"js-theme-system\":\"Theme: system\""));
    assert_eq!(
        http.page("/static/i18n/fr.js", None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, chinese) = http.page("/static/i18n/zh-CN.js", None).await;
    assert!(chinese.contains(&format!(
        "\"js-jump-top\":\"{}\"",
        gono_forum_web::i18n::Lang::parse("zh-CN")
            .unwrap()
            .js_jump_top()
    )));

    f.cleanup().await.unwrap();
}

/// `page` without the parts from each `start` to the next `end` after it: regions a later batch extracts.
fn without(page: &str, cuts: &[(&str, &str)]) -> String {
    let mut text = page.to_owned();
    for (start, end) in cuts {
        while let Some(at) = text.find(start) {
            let stop = text[at..]
                .find(end)
                .map_or(text.len(), |found| at + found + end.len());
            text.replace_range(at..stop, "");
        }
    }
    text
}

#[tokio::test]
async fn english_reading_pages_have_no_untranslated_text() {
    let f = TestFixture::migrated().await.unwrap();
    english_site(&f).await;
    let fid: i64 = db::query_scalar("SELECT MIN(fid) FROM gonof_forum_forum")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let slug: String = db::query_scalar("SELECT slug FROM gonof_forum_forum WHERE fid=?")
        .bind(fid)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let thread = writer
        .publish(
            ADMIN,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Probe thread".into(),
                body: "Opening post with **bold** text and a [link](https://example.test/).".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let root = writer
        .reply(
            ADMIN,
            NewReply {
                tid: thread.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    writer
        .reply(
            ADMIN,
            NewReply {
                tid: thread.tid,
                root_pid: Some(root.pid),
                reply_to_pid: Some(root.pid),
                body: "A nested reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    f.global.clear_local();
    let http = Http {
        app: gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        ),
        identity: Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        ),
    };
    let admin = http.login("probe-admin").await;
    let tid = thread.tid;
    let pages: Vec<(String, Option<&SessionGrant>)> = vec![
        ("/".into(), None),
        ("/?page=2".into(), None),
        ("/forums".into(), None),
        (format!("/{slug}"), None),
        ("/search?q=probe".into(), None),
        ("/search/users".into(), None),
        ("/search/users?q=probe".into(), None),
        ("/search/users?q=nobody".into(), None),
        (format!("/thread-{tid}"), None),
        (format!("/thread-{tid}?view=flat"), None),
        (format!("/space/{ADMIN}"), None),
        (format!("/space/{ADMIN}/threads"), None),
        (format!("/space/{ADMIN}/replies"), None),
        ("/ranks".into(), None),
        ("/verified".into(), None),
        ("/links".into(), None),
        ("/feeds".into(), None),
        ("/credits/rules".into(), None),
        ("/no-such-page".into(), None),
        ("/".into(), Some(&admin)),
        (format!("/space/{ADMIN}"), Some(&admin)),
    ];
    let mut leaks = Vec::new();
    for (path, grant) in &pages {
        let (status, page) = http.page(path, *grant).await;
        assert!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "{path}: {status}"
        );
        // The built-in explainers of the directory pages are left out of this check.
        let page = without(
            &page,
            &[(
                "<section class=\"directory-section directory-explainer\"",
                "</section>",
            )],
        );
        let found = han(&page);
        if !found.is_empty() {
            let at = page.find(found[0]).unwrap_or(0);
            let context: String = page[at.saturating_sub(120)..].chars().take(200).collect();
            leaks.push(format!(
                "{path}: {}\n  …{context}…",
                found.iter().collect::<String>()
            ));
        }
    }
    assert!(
        leaks.is_empty(),
        "Chinese text left in the page: {leaks:#?}"
    );
    let (_, page) = http.page(&format!("/thread-{tid}"), None).await;
    assert!(
        page.contains("Log in and verify your email to reply."),
        "{page}"
    );
    assert!(page.contains("Reply to probe-admin · #2"), "{page}");
    let (_, page) = http.page("/", None).await;
    assert!(page.contains("<h2 class=\"panel__title\">Site statistics</h2>"));
    assert!(
        page.contains("<dt>Activity Index</dt>"),
        "an empty index name shows the language default"
    );

    f.cleanup().await.unwrap();
}

/// The places a page may still hold Chinese and the first characters around them, for a readable failure.
fn leak(path: &str, status: StatusCode, page: &str) -> Option<String> {
    let found = han(page);
    if found.is_empty() {
        return None;
    }
    let at = page.find(found[0]).unwrap_or(0);
    let context: String = page[at.saturating_sub(120)..].chars().take(200).collect();
    Some(format!(
        "{path} ({status}): {}\n  …{context}…",
        found.iter().collect::<String>()
    ))
}

#[tokio::test]
async fn english_member_pages_have_no_untranslated_text() {
    let f = TestFixture::migrated().await.unwrap();
    english_site(&f).await;
    gono_forum_web::test_support::raise_posting_quotas(&f.db)
        .await
        .unwrap();
    for (key, value) in [
        ("settings.tip.enabled", "true"),
        ("settings.growth.activity_level", "1"),
    ] {
        set_setting(&f.db, key, value).await.unwrap();
    }
    execute(&f.db, "UPDATE gonof_common_credit_rule SET enabled=1").await;
    member(&f.db, MEMBER, "probe-member", 0, true).await;
    member(&f.db, OTHER, "probe-other", 0, true).await;
    f.global.clear_local();
    let http = Http {
        app: gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        ),
        identity: Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        ),
    };
    let admin = http.login("probe-admin").await;
    let member_grant = http.login("probe-member").await;
    let other = http.login("probe-other").await;
    for uid in [MEMBER, OTHER] {
        let key = identity::random_token();
        let (status, _) = http
            .post(
                "/manage/credits/adjust",
                &admin,
                &[
                    ("uid", &uid.to_string()),
                    ("amount", "1000"),
                    ("reason", "Probe budget"),
                    ("request_key", &key),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }
    let fid: i64 = db::query_scalar("SELECT MIN(fid) FROM gonof_forum_forum")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let fid = fid.to_string();
    let slug: String = db::query_scalar("SELECT slug FROM gonof_forum_forum WHERE fid=?")
        .bind(&fid)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let deadline = chrono::DateTime::<chrono::Utc>::from_timestamp(identity::now() + 9 * 3_600, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    let mut tids = Vec::new();
    for category in 0..=5 {
        let category_text = category.to_string();
        let key = identity::random_token();
        let title = format!("Probe thread {category}");
        let mut fields = vec![
            ("fid", fid.as_str()),
            ("category", category_text.as_str()),
            ("title", title.as_str()),
            ("body", "Probe body with **bold** and `code`."),
            ("request_key", key.as_str()),
            ("budget", "12"),
            ("shares", "3"),
            ("distribution", "equal"),
            ("deadline", deadline.as_str()),
            ("max_choices", "2"),
            ("options", "Alpha\nBeta\nGamma"),
            ("source_url", "https://example.test/news"),
            ("source_name", "Probe source"),
            ("source_author", "Probe author"),
        ];
        if category == 3 {
            fields.extend([
                ("entry_mode", "click"),
                ("requirements", "Follow the author"),
                ("review", "1"),
                ("early_draw", "1"),
                (
                    "prizes",
                    "Member card | 1 | Code | | Yearly\nPoints prize | 2 | Points | 5",
                ),
                ("lottery_codes", "PROBE-CODE-1"),
            ]);
        }
        let (status, page) = http.post("/new", &member_grant, &fields).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{category}: {page}");
        tids.push(
            db::query_scalar::<i64>("SELECT MAX(tid) FROM gonof_forum_thread")
                .fetch_one(f.db.executor())
                .await
                .unwrap(),
        );
    }
    let plain = tids[0];
    for tid in &tids {
        let key = identity::random_token();
        http.post(
            &format!("/api/thread-{tid}/comments"),
            &other,
            &[("body", "A probe reply"), ("request_key", &key)],
        )
        .await;
    }
    let key = identity::random_token();
    http.post(
        &format!("/api/thread-{plain}/tip"),
        &other,
        &[("amount", "5"), ("request_key", &key)],
    )
    .await;
    http.post(
        &format!("/api/thread-{}/enter", tids[3]),
        &other,
        &[("confirm_1", "1")],
    )
    .await;
    let key = identity::random_token();
    http.post(
        &format!("/api/thread-{}/vote", tids[4]),
        &other,
        &[("choice_1", "1"), ("request_key", &key)],
    )
    .await;
    http.post("/api/check-in", &member_grant, &[]).await;
    let key = identity::random_token();
    let (status, _) = http
        .post(
            "/my/messages/new",
            &other,
            &[
                ("username", "probe-member"),
                ("body", "A probe message"),
                ("request_key", &key),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let conversation: i64 =
        db::query_scalar("SELECT COALESCE(MAX(id),0) FROM gonof_home_message_thread")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    let mut pages: Vec<(String, Option<&SessionGrant>)> = vec![
        ("/login".into(), None),
        ("/register".into(), None),
        ("/forgot-password".into(), None),
        ("/reset-password?token=0".into(), None),
        ("/appeals".into(), None),
        ("/data/access".into(), None),
        ("/static/i18n/en.js".into(), None),
    ];
    for path in [
        "/my".to_owned(),
        "/my/account".into(),
        "/my/profile".into(),
        "/my/reading".into(),
        "/my/style".into(),
        "/my/feed".into(),
        "/my/api".into(),
        "/my/invites".into(),
        "/my/messages".into(),
        "/my/messages/new".into(),
        format!("/my/messages/{conversation}"),
        "/my/notifications".into(),
        "/my/favorites".into(),
        "/my/following/threads".into(),
        "/my/following/forums".into(),
        "/my/following/members".into(),
        "/my/following/feed".into(),
        "/my/followers".into(),
        "/my/blocked-users".into(),
        "/my/threads".into(),
        "/my/posts".into(),
        "/my/reports".into(),
        format!("/my/reports/new?target_type=profile&target_id={OTHER}"),
        "/my/sanctions".into(),
        "/my/restorations".into(),
        "/my/data".into(),
        "/my/preferences".into(),
        "/my/forum-filters".into(),
        "/my/credits".into(),
        "/my/credits?done=signin".into(),
        "/my/credits/growth".into(),
        "/my/prizes".into(),
        "/my/attachments".into(),
        "/my/attachments/trash".into(),
        "/new".into(),
        format!("/new/{slug}"),
        format!("/edit/thread-{plain}"),
        format!("/threads/{}/lottery", tids[3]),
        format!("/space/{OTHER}"),
    ] {
        pages.push((path, Some(&member_grant)));
    }
    for tid in &tids {
        pages.push((format!("/thread-{tid}"), None));
        pages.push((format!("/thread-{tid}"), Some(&member_grant)));
        pages.push((format!("/thread-{tid}"), Some(&other)));
    }
    let mut leaks = Vec::new();
    for (path, grant) in &pages {
        let (status, page) = http.page(path, *grant).await;
        assert!(
            status.is_success() || status == StatusCode::BAD_REQUEST,
            "{path}: {status}"
        );
        leaks.extend(leak(path, status, &page));
    }
    // Error messages of posting come back in the form.
    for fields in [
        vec![
            ("fid", fid.as_str()),
            ("category", "0"),
            ("title", ""),
            ("body", "x"),
        ],
        vec![
            ("fid", fid.as_str()),
            ("category", "3"),
            ("title", "Bad prize"),
            ("body", "x"),
            ("prizes", "Bad | x | Code"),
            ("deadline", deadline.as_str()),
        ],
    ] {
        let key = identity::random_token();
        let mut fields = fields.clone();
        fields.push(("request_key", &key));
        let (status, page) = http.post("/new", &member_grant, &fields).await;
        leaks.extend(leak("POST /new", status, &page));
    }
    // Built-in mail in English (docs/architecture.md, "Languages"): account mail, and the private message and reply
    // notifications queued above, delivered to local files.
    http.identity
        .request_password_reset("probe-member@example.test")
        .await
        .unwrap();
    http.identity.resend_verification(UNVERIFIED).await.unwrap();
    let mailbox = f.root.join(".local/i18n-probe-mail");
    while gono_forum_web::test_support::deliver_mail(&f.db, &mailbox, ORIGIN, identity::now())
        .await
        .unwrap()
        .claimed
        > 0
    {}
    let mut mails = 0;
    for entry in std::fs::read_dir(&mailbox).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "eml") {
            let text = std::fs::read_to_string(&path).unwrap();
            leaks.extend(leak(&path.display().to_string(), StatusCode::OK, &text));
            mails += 1;
        }
    }
    assert!(mails >= 2, "{mails}");
    assert!(
        leaks.is_empty(),
        "Chinese text left in the page: {leaks:#?}"
    );

    f.cleanup().await.unwrap();
}

/// The admin area in English: every module and page, for the administrator and the four other staff roles, with data
/// in the queues, the saved notices and the form errors coming back, and the debug panel. The only Chinese allowed is
/// a language's own name (the Simplified Chinese one, on the language pickers) and the sample of the Chinese date
/// format (`ymd-cn`) in its dropdown, which are cut out before checking.
/// A form post of the admin probe: path, who sends it and the fields.
type FormPost<'a> = (String, &'a SessionGrant, Vec<(&'static str, String)>);

#[tokio::test]
async fn english_admin_pages_have_no_untranslated_text() {
    const GMOD: i64 = 9410;
    const MODERATOR: i64 = 9411;
    const TRAINEE: i64 = 9412;
    const REVIEWER: i64 = 9413;
    const PUNISHED: i64 = 9414;
    let f = TestFixture::migrated().await.unwrap();
    english_site(&f).await;
    gono_forum_web::test_support::raise_posting_quotas(&f.db)
        .await
        .unwrap();
    set_setting(&f.db, "settings.tip.enabled", "true")
        .await
        .unwrap();
    member(&f.db, MEMBER, "probe-member", 0, true).await;
    member(&f.db, OTHER, "probe-other", 0, true).await;
    member(&f.db, GMOD, "probe-gmod", 2, true).await;
    member(&f.db, MODERATOR, "probe-moderator", 3, true).await;
    member(&f.db, TRAINEE, "probe-trainee", 4, true).await;
    member(&f.db, REVIEWER, "probe-reviewer", 5, true).await;
    member(&f.db, PUNISHED, "probe-punished", 0, true).await;
    let fid: i64 = db::query_scalar("SELECT MIN(fid) FROM gonof_forum_forum")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    // The two honor groups the fixture restores are rows of the database: English names, like the forums.
    execute(
        &f.db,
        "UPDATE gonof_common_honor SET name='Contributor', description='Seeded group' WHERE id=1",
    )
    .await;
    execute(
        &f.db,
        "UPDATE gonof_common_honor SET name='Sponsor', description='Seeded group' WHERE id=2",
    )
    .await;
    for uid in [MODERATOR, TRAINEE] {
        db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(?,?)")
            .bind(uid)
            .bind(fid)
            .execute(f.db.executor())
            .await
            .unwrap();
    }
    f.global.clear_local();
    let http = Http {
        app: gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        ),
        identity: Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        ),
    };
    let admin = http.login("probe-admin").await;
    let member_grant = http.login("probe-member").await;
    let other = http.login("probe-other").await;
    let gmod = http.login("probe-gmod").await;
    let moderator = http.login("probe-moderator").await;
    let trainee = http.login("probe-trainee").await;
    let reviewer = http.login("probe-reviewer").await;
    let punished = http.login("probe-punished").await;
    let scalar = |sql: &'static str| {
        let db = f.db.clone();
        async move {
            db::query_scalar::<i64>(sql)
                .fetch_one(db.executor())
                .await
                .unwrap()
        }
    };
    let fid_text = fid.to_string();
    let mut tids = Vec::new();
    for title in ["Probe thread one", "Probe thread two", "Probe thread three"] {
        let key = identity::random_token();
        let (status, _) = http
            .post(
                "/new",
                &member_grant,
                &[
                    ("fid", fid_text.as_str()),
                    ("category", "0"),
                    ("title", title),
                    ("body", "Opening post with **bold** text."),
                    ("request_key", key.as_str()),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{title}");
        tids.push(scalar("SELECT MAX(tid) FROM gonof_forum_thread").await);
    }
    let key = identity::random_token();
    http.post(
        &format!("/api/thread-{}/comments", tids[0]),
        &other,
        &[("body", "A reply"), ("request_key", key.as_str())],
    )
    .await;
    // Governance: sanctions, an appeal, reports, a hidden thread with a restoration request, a data request.
    let version = || {
        let db = f.db.clone();
        async move {
            db::query_scalar::<i64>(
                "SELECT credentials_version FROM gonof_common_member WHERE uid=?",
            )
            .bind(PUNISHED)
            .fetch_one(db.executor())
            .await
            .unwrap()
            .to_string()
        }
    };
    for (kind, hours) in [("warning", "0"), ("mute", "2")] {
        let expected = version().await;
        let key = identity::random_token();
        let (status, _) = http
            .post(
                "/manage/sanctions",
                &admin,
                &[
                    ("uid", &PUNISHED.to_string()),
                    ("expected_version", expected.as_str()),
                    ("kind", kind),
                    ("hours", hours),
                    ("reason", "Probe sanction"),
                    ("request_key", key.as_str()),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{kind}");
    }
    let sanction = scalar("SELECT MIN(id) FROM gonof_common_member_sanction").await;
    let key = identity::random_token();
    http.post(
        &format!("/my/sanctions/{sanction}/appeal"),
        &punished,
        &[("reason", "Probe appeal"), ("request_key", key.as_str())],
    )
    .await;
    let appeal = scalar("SELECT COALESCE(MAX(id),0) FROM gonof_common_member_appeal").await;
    for (target_type, target_id) in [("profile", OTHER), ("thread", tids[1])] {
        let key = identity::random_token();
        http.post(
            "/my/reports",
            &punished,
            &[
                ("target_type", target_type),
                ("target_id", &target_id.to_string()),
                ("reason", "Probe report"),
                ("request_key", key.as_str()),
            ],
        )
        .await;
    }
    let report = scalar("SELECT COALESCE(MAX(id),0) FROM gonof_common_report").await;
    let editor = content::edits::ContentEditor::new(f.db.clone(), WriteConfig::default(), 86_400);
    let hidden: i64 = db::query_scalar("SELECT pid FROM gonof_forum_post WHERE tid=? AND first=1")
        .bind(tids[2])
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let post = editor
        .editable_at(admin.member.actor(), hidden, identity::now())
        .await
        .unwrap();
    editor
        .change_at(
            admin.member.actor(),
            content::edits::ChangeRequest {
                pid: hidden,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                request_key: identity::random_token(),
                reason: "Probe hide".into(),
                action: content::edits::ContentAction::Hide,
            },
            identity::now(),
        )
        .await
        .unwrap();
    let key = identity::random_token();
    http.post(
        "/my/restorations",
        &member_grant,
        &[
            ("pid", &hidden.to_string()),
            ("reason", "Probe restoration"),
            ("request_key", key.as_str()),
        ],
    )
    .await;
    let restoration = scalar("SELECT COALESCE(MAX(id),0) FROM gonof_forum_restore_request").await;
    let key = identity::random_token();
    http.post(
        "/my/data/requests",
        &punished,
        &[
            ("kind", "export"),
            ("content_mode", "retain"),
            ("request_key", key.as_str()),
            ("password", PASSWORD),
            ("confirm", "yes"),
        ],
    )
    .await;
    let task = scalar("SELECT COALESCE(MIN(eventid),0) FROM gonof_common_outbox").await;
    let first_pid: i64 =
        db::query_scalar("SELECT pid FROM gonof_forum_post WHERE tid=? AND first=1")
            .bind(tids[0])
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    // Module data: an invite batch, an honor group, a verification, a link application, a second domain.
    let key = identity::random_token();
    http.post(
        "/manage/invites",
        &admin,
        &[
            ("request_key", key.as_str()),
            ("count", "3"),
            ("reason", "Probe invites"),
        ],
    )
    .await;
    http.post(
        "/manage/honors",
        &admin,
        &[
            ("name", "Helper"),
            ("description", "Helps others"),
            ("displayorder", "5"),
            ("enabled", "1"),
        ],
    )
    .await;
    let honor = scalar("SELECT COALESCE(MAX(id),0) FROM gonof_common_honor").await;
    http.post(
        &format!("/manage/verifications/{MEMBER}"),
        &admin,
        &[
            ("category", "blogger"),
            ("title", "Probe blogger"),
            ("site_url", "https://example.test"),
            ("expires", ""),
        ],
    )
    .await;
    let key = identity::random_token();
    http.post(
        "/links/apply",
        &member_grant,
        &[
            ("name", "Probe link"),
            ("category", "blog"),
            ("url", "https://probe-link.example.test/"),
            ("backlink_url", "https://probe-link.example.test/links"),
            ("note", "Swap links"),
            ("request_key", key.as_str()),
        ],
    )
    .await;
    let link = scalar("SELECT COALESCE(MAX(id),0) FROM gonof_common_friend_link").await;
    set_setting(&f.db, "settings.site.url", ORIGIN)
        .await
        .unwrap();
    f.global.clear_local();
    http.post("/manage/domains/switch", &admin, &[("enabled", "on")])
        .await;
    let domain_fields = |site_url: &'static str| {
        vec![
            ("domain_id", "0".to_owned()),
            ("version", "0".to_owned()),
            ("site_url", site_url.to_owned()),
            ("site_name", "Probe second".to_owned()),
            ("canonical", "self".to_owned()),
            ("logo_aid", "0".to_owned()),
            ("favicon_aid", "0".to_owned()),
            ("verification", String::new()),
            ("enabled", "1".to_owned()),
            (
                Box::leak(format!("forum_{fid}").into_boxed_str()) as &str,
                "1".to_owned(),
            ),
        ]
    };
    let fields = domain_fields("http://probe2.example.test");
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    http.post("/manage/domains", &admin, &fields).await;
    let domain = scalar("SELECT COALESCE(MAX(domain_id),0) FROM gonof_common_site_domain").await;

    let mut pages: Vec<(String, &SessionGrant)> = [
        "/manage",
        "/manage/settings",
        "/manage/settings?saved=1",
        "/manage/settings/registration",
        "/manage/login",
        "/manage/settings/search",
        "/manage/settings/search?saved=cache",
        "/manage/settings/search?saved=seo&notice=robots-blocks-all",
        "/manage/policies/terms",
        "/manage/policies/privacy",
        "/manage/policies/rules",
        "/manage/policies/contact",
        "/manage/policies/links",
        "/manage/policies/verified",
        "/manage/credits",
        "/manage/credits?done=saved",
        "/manage/storage",
        "/manage/languages",
        "/manage/appearance/parameters",
        "/manage/appearance/colors",
        "/manage/appearance/colors?saved=saved",
        "/manage/navigation",
        "/manage/branding",
        "/manage/links",
        "/manage/reports",
        "/manage/restorations",
        "/manage/appeals",
        "/manage/users",
        "/manage/users?q=probe",
        "/manage/registration-sources",
        "/manage/invites",
        "/manage/sanctions",
        "/manage/data-requests",
        "/manage/verifications",
        "/manage/honors",
        "/manage/content",
        "/manage/forums",
        "/manage/moderators",
        "/manage/domains",
        "/manage/domains/new",
        "/manage/security/posting",
        "/manage/security/account",
        "/manage/mail",
        "/manage/logs",
        "/manage/tasks",
        "/manage/maintenance",
        "/manage/updates",
        "/manage/nothing",
    ]
    .iter()
    .map(|path| (path.to_string(), &admin))
    .collect();
    for path in [
        format!("/manage/users/{PUNISHED}"),
        format!("/manage/users/{MODERATOR}"),
        format!("/manage/sanctions?uid={PUNISHED}"),
        format!("/manage/reports/{report}"),
        format!("/manage/appeals/{appeal}"),
        format!("/manage/restorations/{restoration}"),
        format!("/manage/content/{first_pid}"),
        format!("/manage/content/{hidden}"),
        format!("/manage/forums?fid={fid}"),
        format!("/manage/moderators?uid={MODERATOR}"),
        format!("/manage/threads/{}/organization", tids[0]),
        format!("/manage/verifications/{MEMBER}"),
        format!("/manage/honors/members/{MEMBER}"),
        format!("/manage/tasks/{task}"),
        format!("/appeals/sanctions/{sanction}"),
    ] {
        pages.push((path, &admin));
    }
    for (kind, _) in gono_forum_web::mail::templates::KINDS {
        pages.push((format!("/manage/mail/templates/{kind}"), &admin));
    }
    for grant in [&gmod, &moderator, &trainee, &reviewer] {
        for path in [
            "/manage".to_string(),
            "/manage/reports".into(),
            format!("/manage/reports/{report}"),
            "/manage/restorations".into(),
            "/manage/content".into(),
            "/manage/logs".into(),
            "/manage/sanctions".into(),
            format!("/manage/sanctions?uid={MEMBER}"),
            "/manage/settings".into(),
        ] {
            pages.push((path, grant));
        }
    }
    pages.push((format!("/appeals/sanctions/{sanction}"), &punished));
    for path in [
        format!("/manage/honors/{honor}"),
        format!("/manage/links/{link}"),
        "/manage/links?status=pending".to_string(),
        format!("/manage/domains/{domain}"),
        "/manage/domains?saved=on".into(),
        "/manage/invites?status=available".into(),
    ] {
        pages.push((path, &admin));
    }

    // Allowed Chinese: the language's own name and the sample of the Chinese date format. The first cut runs from the
    // self-name of Simplified Chinese to its own last two characters ("\u{4e2d}\u{6587}", "Chinese"), so exactly the
    // name; the second from the year mark "\u{5e74}" of the `ymd-cn` sample to its day mark "\u{65e5}".
    let cuts = [
        ("\u{7b80}\u{4f53}\u{4e2d}\u{6587}", "\u{4e2d}\u{6587}"),
        ("2026\u{5e74}", "\u{65e5}"),
    ];
    let mut leaks = Vec::new();
    for (path, grant) in &pages {
        let (status, page) = http.page(path, Some(*grant)).await;
        leaks.extend(leak(path, status, &without(&page, &cuts)));
    }
    let posts: Vec<FormPost> = vec![
        (
            "/manage/sanctions".into(),
            &admin,
            vec![
                ("uid", MEMBER.to_string()),
                ("expected_version", "1".into()),
                ("kind", "mute".into()),
                ("hours", "99999".into()),
                ("reason", String::new()),
                ("request_key", identity::random_token()),
            ],
        ),
        (
            format!("/manage/reports/{report}"),
            &admin,
            vec![("status", "resolved".into()), ("resolution", String::new())],
        ),
        (
            format!("/manage/appeals/{appeal}"),
            &admin,
            vec![("status", "accepted".into()), ("resolution", String::new())],
        ),
        (
            format!("/manage/restorations/{restoration}"),
            &admin,
            vec![("status", "accepted".into()), ("resolution", String::new())],
        ),
        (
            "/manage/credits/adjust".into(),
            &admin,
            vec![
                ("uid", MEMBER.to_string()),
                ("amount", "0".into()),
                ("reason", String::new()),
                ("request_key", identity::random_token()),
            ],
        ),
        (
            "/manage/settings".into(),
            &admin,
            vec![("site_name", "x".repeat(200))],
        ),
        (
            "/manage/settings/registration".into(),
            &admin,
            vec![("signup_ip_hourly_limit", "0".into())],
        ),
        (
            "/manage/appearance/parameters".into(),
            &admin,
            vec![("display_list_thumbnails", "99".into())],
        ),
        (
            "/manage/appearance/colors".into(),
            &admin,
            vec![("primary", "red".into()), ("secondary", "#000000".into())],
        ),
        (
            "/manage/appearance/colors/show".into(),
            &admin,
            vec![
                ("primary", "#0f766e".into()),
                ("secondary", "#f97316".into()),
            ],
        ),
        (
            "/manage/moderators".into(),
            &admin,
            vec![
                ("uid", MEMBER.to_string()),
                ("role", "9".into()),
                ("reason", String::new()),
                ("request_key", identity::random_token()),
            ],
        ),
    ];
    let bad_domain = domain_fields("ftp://bad");
    let mut posts = posts;
    posts.push(("/manage/domains".into(), &admin, bad_domain));
    posts.push((
        "/manage/invites".into(),
        &admin,
        vec![
            ("request_key", identity::random_token()),
            ("count", "0".into()),
            ("reason", String::new()),
        ],
    ));
    posts.push((
        "/manage/honors".into(),
        &admin,
        vec![
            ("name", String::new()),
            ("description", String::new()),
            ("displayorder", "x".into()),
        ],
    ));
    posts.push((
        format!("/manage/verifications/{OTHER}"),
        &admin,
        vec![("category", String::new()), ("title", String::new())],
    ));
    for (kind, _) in gono_forum_web::mail::templates::KINDS {
        let body = gono_forum_web::mail::templates::variables(kind)
            .iter()
            .map(|name| format!("{name}: {{{{{name}}}}}"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        posts.push((
            format!("/manage/mail/templates/{kind}"),
            &admin,
            vec![
                ("subject", "Probe {{site_name}}".into()),
                ("body", body),
                ("operation", "preview".into()),
            ],
        ));
    }
    posts.push((
        "/manage/mail/templates/email_verification".into(),
        &admin,
        vec![
            ("subject", "x".into()),
            ("body", "no link".into()),
            ("operation", "save".into()),
        ],
    ));
    for (path, grant, fields) in &posts {
        let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (status, page) = http.post(path, grant, &fields).await;
        leaks.extend(leak(
            &format!("POST {path}"),
            status,
            &without(&page, &cuts),
        ));
    }
    // The debug panel on a few pages.
    set_setting(&f.db, "settings.debug.panel", "true")
        .await
        .unwrap();
    f.global.clear_local();
    for path in ["/manage", "/manage/settings", "/"] {
        let (status, page) = http.page(path, Some(&admin)).await;
        assert!(page.contains("gonof-debug"), "{path}");
        leaks.extend(leak(
            &format!("{path} (debug)"),
            status,
            &without(&page, &cuts),
        ));
    }
    assert!(
        leaks.is_empty(),
        "Chinese text left in the admin pages: {leaks:#?}"
    );

    f.cleanup().await.unwrap();
}
