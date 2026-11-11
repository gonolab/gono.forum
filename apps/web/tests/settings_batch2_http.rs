//! Settings pages and their output: SEO output, the admin settings tabs, the site default language and the forum URL
//! in e-mail links.
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    identity::{self, Actor},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9801;

struct Fixture {
    f: TestFixture,
    app: Router,
    admin: identity::SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        content::seed_fixture(&f.db).await.unwrap();
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,1,1)")
            .bind(ADMIN)
            .bind("batch2-admin")
            .bind("batch2-admin@example.test")
            .execute(f.db.executor())
            .await
            .unwrap();
        let identity = identity::Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let admin = http_auth::login(&identity, &f.db, ADMIN).await;
        // Shares the same in-process cache handle with the save functions: the app's next request after a save sees
        // the new value.
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, admin }
    }

    async fn update(&self, edit: impl FnOnce(&mut runtime::SiteSettings)) {
        runtime::update(
            &self.f.db,
            &self.f.global,
            Actor {
                uid: ADMIN,
                credentials_version: self.admin.member.credentials_version,
                api_key: None,
            },
            "site.settings.save",
            edit,
        )
        .await
        .unwrap();
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
        let mut request = Request::builder().uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn admin_get(&self, path: &str) -> (StatusCode, String) {
        let cookie = format!("gonof_session={}", self.admin.session_token);
        let (status, _, body) = self.get(path, &[("cookie", &cookie)]).await;
        (status, body)
    }

    async fn admin_post(
        &self,
        path: &str,
        fields: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session={}", self.admin.session_token),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }
}

fn title(html: &str) -> &str {
    let start = html.find("<title>").expect("the page has no title") + "<title>".len();
    &html[start..start + html[start..].find("</title>").unwrap()]
}

fn meta<'a>(html: &'a str, attr: &str) -> Option<&'a str> {
    let marker = format!("{attr} content=\"");
    let start = html.find(&marker)? + marker.len();
    Some(&html[start..start + html[start..].find('"').unwrap()])
}

/// robots.txt: with indexing off always `Disallow: /`; with it on, output the admin text with a system-appended
/// `Sitemap:`; when the forum URL is empty it is derived from the request's `Host`, and without `Host` it is an
/// error.
#[tokio::test]
async fn robots_follow_the_indexing_switch_and_the_site_address() {
    let f = Fixture::new().await;
    f.update(|s| s.seo.robots = "User-agent: *\nDisallow: /private".into())
        .await;
    let (status, headers, body) = f.get("/robots.txt", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "User-agent: *\nDisallow: /\n",
        "with indexing off the edited text is not read"
    );
    assert!(headers.get(header::SET_COOKIE).is_none());

    f.update(|s| s.seo.indexing = true).await;
    let (_, _, body) = f.get("/robots.txt", &[]).await;
    assert_eq!(
        body,
        format!("User-agent: *\nDisallow: /private\nSitemap: {TEST_SITE_URL}/sitemap.xml\n")
    );
    f.update(|s| s.seo.robots.clear()).await;
    let (_, _, body) = f.get("/robots.txt", &[]).await;
    assert_eq!(body, format!("Sitemap: {TEST_SITE_URL}/sitemap.xml\n"));

    // Forum URL empty: derive it from the request's Host; without Host, state which setting is missing.
    f.update(|s| s.site.url.clear()).await;
    let (_, _, body) = f
        .get("/robots.txt", &[("host", "forum.example.test:8080")])
        .await;
    assert_eq!(
        body,
        "Sitemap: http://forum.example.test:8080/sitemap.xml\n"
    );
    let (status, _, body) = f.get("/robots.txt", &[]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("settings.site.url"), "{body}");
    let (_, _, home) = f.get("/", &[("host", "forum.example.test:8080")]).await;
    assert!(
        home.contains("<link rel=\"canonical\" href=\"http://forum.example.test:8080/\">"),
        "the page canonical is derived from Host as well"
    );
    f.f.cleanup_ref().await.unwrap();
}

/// Title, description, sharing meta information and webmaster verification.
#[tokio::test]
async fn head_metadata_follows_the_seo_rules() {
    let f = Fixture::new().await;
    f.update(|s| s.seo.verification = "google-site-verification=abc_DEF-1".into())
        .await;
    let site = runtime::read(&f.f.db).await.unwrap().site;

    let (_, _, home) = f.get("/", &[]).await;
    assert_eq!(
        title(&home),
        site.name,
        "the title of home page 1 is the site name"
    );
    assert_eq!(
        meta(&home, "name=\"description\""),
        Some(site.description.as_str())
    );
    assert_eq!(
        meta(&home, "property=\"og:site_name\""),
        Some(site.name.as_str())
    );
    assert_eq!(meta(&home, "property=\"og:locale\""), Some("en_US"));
    assert!(
        home.contains("<meta name=\"google-site-verification\" content=\"abc_DEF-1\">"),
        "site verification is only on home page 1, whatever the indexing switch"
    );

    let (status, _, second) = f.get("/page-2", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        title(&second),
        format!("Latest replies - Page 2 - {}", site.name)
    );
    assert!(!second.contains("google-site-verification"));
    let (_, _, created) = f.get("/?sort=created", &[]).await;
    assert_eq!(title(&created), format!("Newest threads - {}", site.name));
    let (status, _, thread) = f.get("/thread-103-2", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        title(&thread).ends_with(&format!(" - Page 2 - {}", site.name)),
        "{}",
        title(&thread)
    );
    assert!(!thread.contains("google-site-verification"));

    // The forum page description uses the forum's description, falling back to the site description when empty.
    let (_, _, forum) = f.get("/daily", &[]).await;
    let described: String =
        db::query_scalar("SELECT description FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(f.f.db.executor())
            .await
            .unwrap();
    assert_eq!(
        meta(&forum, "name=\"description\""),
        Some(described.as_str())
    );
    db::query("UPDATE gonof_forum_forum SET description='' WHERE slug='daily'")
        .execute(f.f.db.executor())
        .await
        .unwrap();
    // Direct database change: readable forums are in the in-process cache, so it is only visible after clearing this
    // process's layer.
    f.f.global.clear_local();
    let (_, _, forum) = f.get("/daily", &[]).await;
    assert_eq!(
        meta(&forum, "name=\"description\""),
        Some(site.description.as_str())
    );

    // Error pages do not output canonical.
    let (status, _, missing) = f.get("/not-existing-page", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!missing.contains("rel=\"canonical\""));
    assert!(!missing.contains("property=\"og:url\""));
    f.f.cleanup_ref().await.unwrap();
}

/// Admin SEO block: robots line endings are normalized to LF, blocking the whole site saves successfully with a
/// notice; forbidden names for webmaster verification are refused.
#[tokio::test]
async fn seo_block_saves_four_rows_and_warns_about_blocking_everything() {
    let f = Fixture::new().await;
    let (status, page) = f.admin_get("/manage/settings/search").await;
    assert_eq!(status, StatusCode::OK);
    for field in [
        "name=\"robots\"",
        "name=\"verification\"",
        "name=\"feed_items\"",
    ] {
        assert!(page.contains(field), "missing field {field}");
    }
    let (status, headers, _) = f
        .admin_post(
            "/manage/settings/search/seo",
            &[
                ("indexing_enabled", "true"),
                ("description", "A new site description"),
                ("robots", "User-agent: *\r\nDisallow: /\r\n\r\n"),
                ("verification", "  msvalidate.01=ABCDEF  \n\n"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/manage/settings/search?saved=seo&notice=robots-blocks-all"
    );
    let stored = runtime::read(&f.f.db).await.unwrap();
    assert!(stored.seo.indexing);
    assert_eq!(stored.seo.robots, "User-agent: *\nDisallow: /");
    assert_eq!(stored.seo.verification, "msvalidate.01=ABCDEF");
    assert_eq!(stored.site.description, "A new site description");
    let (_, page) = f
        .admin_get("/manage/settings/search?saved=seo&notice=robots-blocks-all")
        .await;
    assert!(page.contains("search engines will stop crawling the whole site"));

    for verification in ["Robots=abc", "a=b c", "x"] {
        let (status, _, page) = f
            .admin_post(
                "/manage/settings/search/seo",
                &[("verification", verification)],
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{verification}");
        assert!(
            page.contains("Site verification goes one"),
            "{verification}"
        );
    }
    let (status, _, _) = f
        .admin_post("/manage/settings/search/feed", &[("feed_items", "12")])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(runtime::read(&f.f.db).await.unwrap().feed.items, 12);
    f.f.cleanup_ref().await.unwrap();
}

/// Sitemap: threads carry `<lastmod>`, page 1 lists the forum directory, no cookie is sent, and an invalid page
/// number is a plain-text 404.
#[tokio::test]
async fn sitemap_lists_forums_and_thread_modification_times() {
    let f = Fixture::new().await;
    f.update(|s| s.seo.indexing = true).await;
    let (status, headers, xml) = f.get("/sitemap.xml?page=1", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "the sitemap skips the session middleware"
    );
    assert!(xml.contains(&format!("<loc>{TEST_SITE_URL}/forums</loc>")));
    assert!(!xml.contains(&format!("<loc>{TEST_SITE_URL}/tags</loc>")));
    assert!(!xml.contains(&format!("<loc>{TEST_SITE_URL}/tag-")));
    assert!(xml.contains(&format!("<loc>{TEST_SITE_URL}/thread-103</loc><lastmod>")));
    let (status, headers, body) = f.get("/sitemap.xml?page=99", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    assert!(body.contains("Invalid sitemap page"), "{body}");
    f.f.cleanup_ref().await.unwrap();
}

/// Every settings tab can be shown and saved, and takes effect in this process right after saving
/// (docs/settings-and-caching.md, "When a change takes effect").
#[tokio::test]
async fn settings_pages_render_save_and_take_effect() {
    let f = Fixture::new().await;
    for (path, field) in [
        ("/manage/settings/registration", "invite_daily_limit"),
        ("/manage/security/posting", "posting_daily_threads"),
        ("/manage/security/account", "account_session_idle_days"),
        ("/manage/appearance/parameters", "display_threads_per_page"),
        ("/manage/maintenance", "debug_panel"),
        ("/manage/credits", "growth_levels"),
        ("/manage/links", "links_home_count"),
    ] {
        let (status, page) = f.admin_get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            page.contains(&format!("name=\"{field}\"")),
            "{path} lacks {field}"
        );
    }

    // The member API sub-block is only shown when the API is open; fields not shown are kept as they are on save.
    let (_, page) = f.admin_get("/manage/settings/registration").await;
    assert!(page.contains("name=\"api_key_ttl_days\""));
    let (status, _, _) = f
        .admin_post(
            "/manage/settings/registration",
            &[
                ("registration", "open"),
                ("invitations_enabled", "true"),
                ("invite_daily_limit", "7"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let stored = runtime::read(&f.f.db).await.unwrap();
    assert_eq!(
        (
            stored.signup.mode.as_str(),
            stored.invite.daily_limit,
            stored.api.enabled
        ),
        ("open", 7, false)
    );
    assert_eq!(
        stored.api.key_ttl_days, 365,
        "API settings not shown stay as they are"
    );
    let (_, page) = f.admin_get("/manage/settings/registration").await;
    assert!(!page.contains("name=\"api_key_ttl_days\""));

    // Cross-field constraints are reported on the form: the daily thread quota cannot be less than the quota during
    // the new-member period.
    let (status, _, page) = f
        .admin_post(
            "/manage/security/posting",
            &[
                ("posting_first_day_threads", "6"),
                ("posting_daily_threads", "5"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        page.contains("no less than the threads allowed while new"),
        "{page}"
    );
    assert!(
        page.contains("value=\"6\""),
        "the form is refilled after a failure"
    );
    let (status, _, page) = f
        .admin_post(
            "/manage/security/account",
            &[("account_password_min_length", "abc")],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        page.contains(
            "\u{201c}Minimum password length (characters)\u{201d} must be a whole number"
        ),
        "{page}"
    );

    // Footer metrics line and debug panel: the same app's next request after a save uses the new value.
    let (_, _, home) = f.get("/", &[]).await;
    assert!(home.contains("Processed in "));
    assert!(
        home.matches("data-thread-id=").count() > 10,
        "the fixture has more threads than the new page size"
    );
    let (status, parameters) = f.admin_get("/manage/appearance/parameters").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!parameters.contains("reading_language"));
    let (status, _, _) = f
        .admin_post(
            "/manage/appearance/parameters",
            &[("display_threads_per_page", "10")],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, _, home) = f.get("/", &[]).await;
    assert!(
        !home.contains("Processed in "),
        "an unsubmitted checkbox turns the footer metrics line off"
    );
    assert_eq!(
        home.matches("data-thread-id=").count(),
        10,
        "the home page uses the new page size"
    );
    let (_, admin_home) = f.admin_get("/").await;
    assert!(!admin_home.contains("id=\"gonof-debug\""));
    let (status, _, _) = f
        .admin_post("/manage/maintenance", &[("debug_panel", "true")])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, admin_home) = f.admin_get("/").await;
    assert!(
        admin_home.contains("id=\"gonof-debug\""),
        "with the debug panel on, administrator pages end with it"
    );
    let (_, _, anonymous) = f.get("/", &[]).await;
    assert!(!anonymous.contains("id=\"gonof-debug\""));

    // Growth block: thresholds and names are saved together and validated together.
    let (status, _, _) = f
        .admin_post(
            "/manage/credits/growth-settings",
            &[
                ("growth_thread", "4"),
                ("growth_levels", "0, 10"),
                ("growth_level_names", "Seedling\nFour-Star Member"),
                ("ranks_refresh_seconds", "120"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let stored = runtime::read(&f.f.db).await.unwrap();
    assert_eq!(stored.growth.levels, [0, 10]);
    assert_eq!(stored.growth.level_names, ["Seedling", "Four-Star Member"]);
    assert_eq!(
        (stored.growth.thread, stored.ranks.refresh_seconds),
        (4, 120)
    );
    let (status, _, page) = f
        .admin_post(
            "/manage/credits/growth-settings",
            &[("growth_levels", "0,10,20")],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        page.contains("as many as there are level thresholds"),
        "{page}"
    );

    // Friend link application rules.
    let (status, _, _) = f
        .admin_post(
            "/manage/links/rules",
            &[("link_applications", "true"), ("links_home_count", "3")],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(runtime::read(&f.f.db).await.unwrap().links.home_count, 3);
    f.f.cleanup_ref().await.unwrap();
}

/// E-mail has no request to derive the URL from: with the forum URL empty, delivery fails (retried with backoff),
/// and once it is filled in mail goes out as usual (9.10 item 1d).
#[tokio::test]
async fn account_mail_needs_the_forum_address() {
    let f = Fixture::new().await;
    let identity = identity::Identity::new(
        f.f.db.clone(),
        f.f.config.identity.clone(),
        f.f.root.clone(),
        f.f.global.clone(),
    );
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=?")
        .bind(ADMIN)
        .execute(f.f.db.executor())
        .await
        .unwrap();
    identity.resend_verification(ADMIN).await.unwrap();
    f.update(|s| s.site.url.clear()).await;
    let mailbox = f.f.root.join("batch2-mail");
    let mailer = gono_forum_web::mail::Mailer::new(
        f.f.db.clone(),
        gono_forum_web::secrets::CommonConfig::default(),
        mailbox.clone(),
        f.f.global.clone(),
    );
    let now = identity::now();
    let report = mailer.deliver_pending(now).await.unwrap();
    assert_eq!(
        report.delivered, 0,
        "with the forum URL empty, mail with links is not sent"
    );
    f.update(|s| s.site.url = TEST_SITE_URL.into()).await;
    let report = mailer.deliver_pending(now + 3600).await.unwrap();
    assert_eq!(report.delivered, 1);
    let sent = std::fs::read_dir(&mailbox)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
        .unwrap();
    let text = std::fs::read_to_string(sent.path()).unwrap();
    assert!(text.contains(&format!("{TEST_SITE_URL}/verify-email?token=")));
    f.f.cleanup_ref().await.unwrap();
}

/// "Refresh caches": after editing the database directly, bypassing the admin area, the old value is
/// still shown within the process cache TTL (seeded 3600 seconds); one click clears it and the next request reads
/// the new value. A submission without login is redirected to login and does not clear the cache.
#[tokio::test]
async fn refreshing_the_cache_shows_direct_database_edits_at_once() {
    let f = Fixture::new().await;
    let (status, page) = f.admin_get("/manage/settings/search").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains("/manage/settings/search/cache/refresh"),
        "missing the refresh caches button"
    );
    assert!(page.contains("name=\"ttl_seconds\" value=\"3600\" min=\"0\" max=\"86400\""));
    assert!(f.get("/", &[]).await.2.contains("Sample Community"));
    db::query(
        "UPDATE gonof_common_settings SET svalue='Name edited by hand' WHERE skey='settings.site.name'",
    )
    .execute(f.f.db.executor())
    .await
    .unwrap();
    assert!(
        !f.get("/", &[]).await.2.contains("Name edited by hand"),
        "within the lifetime the old value in the process cache is used"
    );

    let anonymous = Request::builder()
        .method("POST")
        .uri("/manage/settings/search/cache/refresh")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("_csrf=x"))
        .unwrap();
    let response = f.app.clone().oneshot(anonymous).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/login")
    );
    assert!(
        !f.get("/", &[]).await.2.contains("Name edited by hand"),
        "a request without login does not clear the cache"
    );

    let (status, headers, _) = f
        .admin_post("/manage/settings/search/cache/refresh", &[])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/manage/settings/search?saved=refresh"
    );
    assert!(
        f.get("/", &[]).await.2.contains("Name edited by hand"),
        "after the click the new value is read at once"
    );
    let (_, page) = f.admin_get("/manage/settings/search?saved=refresh").await;
    assert!(page.contains("cache was cleared"), "{page}");
    f.f.cleanup_ref().await.unwrap();
}
