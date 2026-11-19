mod support;

use axum::http::StatusCode;
use gono_forum_web::{content, project_root, router};

async fn database() -> gono_forum_web::test_support::TestFixture {
    gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap()
}

async fn app() -> (axum::Router, gono_forum_web::test_support::TestFixture) {
    let services = database().await;
    let db = services.db.clone();
    let config = services.config.clone();
    content::seed_forums(&db).await.unwrap();
    content::seed_fixture(&db).await.unwrap();
    (router::build(config, &project_root(), db), services)
}

#[tokio::test]
async fn renders_complete_local_pages_with_and_without_htmx() {
    let (app, services) = app().await;
    for htmx in [false, true] {
        for path in [
            "/",
            "/?sort=created",
            "/?sort=pinned",
            "/?sort=featured",
            "/ai",
            "/page-2",
            "/thread-103",
            "/thread-103-2?view=flat",
        ] {
            let (status, headers, body) = support::fetch(&app, path, htmx).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(body.starts_with("<!doctype html>"));
            assert!(body.contains("Sample Forum"));
            assert!(body.contains("rel=\"canonical\""));
            assert!(!body.contains("<script src=\"http"));
            assert!(!body.contains("name=\"_csrf\""));
            assert_eq!(headers["vary"], "HX-Request");
            assert_eq!(headers["cache-control"], "private, no-store");
        }
    }
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn redirects_aliases_once_and_keeps_effective_filters() {
    let (app, services) = app().await;
    for (path, target) in [
        ("/page-01?sort=created&utm_source=test", "/?sort=created"),
        ("/daily-01", "/daily"),
        ("/thread-00103-01?view=flat", "/thread-103?view=flat"),
    ] {
        let (status, headers, _) = support::fetch(&app, path, false).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers["location"], target);
        assert_eq!(support::fetch(&app, target, false).await.0, StatusCode::OK);
    }
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_routes_pages_and_query_parameters() {
    let (app, services) = app().await;
    for path in [
        "/page-0",
        "/page-3",
        "/page-18446744073709551615",
        "/daily-2",
        "/thread-0",
        "/thread-999",
        "/thread-103-3",
        "/thread-103?sub_root=999",
        "/thread-103?sub_root=103002&sub_page=3",
    ] {
        assert_eq!(
            support::fetch(&app, path, false).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    for path in [
        "/?sort=wrong",
        "/thread-103?view=wrong",
        "/thread-103?sub_page=0",
        "/thread-103?sub_page=1",
    ] {
        assert_eq!(
            support::fetch(&app, path, false).await.0,
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn search_escapes_user_input_and_matches_titles_without_body_search() {
    let (app, services) = app().await;
    let (_, _, body) =
        support::fetch(&app, "/search?q=%3Cscript%3Ealert(1)%3C/script%3E", false).await;
    assert!(!body.contains("<script>alert(1)</script>"));
    assert!(body.contains("&#60;script&#62;"));
    // "detail" is in every fixture body but in no title.
    let (_, _, body) = support::fetch(&app, "/search?q=detail", false).await;
    assert!(body.contains("No threads match."));
    let title = content::fixtures()[0].title.clone();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", &title)
        .finish();
    let (_, _, body) = support::fetch(&app, &format!("/search?{query}"), false).await;
    assert!(body.contains("class=\"row-title\""));
    let (_, _, body) = support::fetch(&app, "/search?q=not-present-anywhere", false).await;
    assert!(body.contains("No threads match."));
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn two_level_and_flat_views_keep_global_positions_and_sub_pages() {
    let (app, services) = app().await;
    let (_, _, tree) = support::fetch(&app, "/thread-103", false).await;
    assert!(tree.contains("id=\"floor-2\""));
    assert!(tree.contains("id=\"floor-75\""));
    assert!(!tree.contains("id=\"floor-76\""));
    assert!(!tree.contains("id=\"floor-52\""));
    let (_, _, next) = support::fetch(&app, "/thread-103?sub_root=103002&sub_page=2", false).await;
    assert!(next.contains("id=\"floor-76\""));
    assert!(!next.contains("id=\"floor-56\""));
    assert!(next.contains("sub_root=103002&#38;sub_page=2#floor-76"));
    let (_, _, flat) = support::fetch(&app, "/thread-103-2?view=flat", false).await;
    assert!(flat.contains("id=\"floor-52\""));
    assert!(flat.contains("In reply #2"));
    assert!(flat.contains("/thread-103?sub_root=103002&#38;sub_page=2#floor-76"));
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn nullable_authors_avatars_and_positions_decode_without_sql_defaults() {
    let (_, services) = app().await;
    let db = &services.db;
    let original = content::thread(db, 103).await.unwrap().unwrap();
    let profile = content::public_member(
        db,
        &original.author,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(profile.avatar_aid, 0);
    // The code has no write path that changes a thread's author (pinned by a source scan); this builds the state "the
    // author row no longer exists", setting the author copies to an empty name and no avatar.
    for sql in [
        "UPDATE gonof_forum_thread SET authorid=NULL,author='' WHERE tid=103",
        "UPDATE gonof_forum_post SET authorid=NULL WHERE tid=103 AND (first=1 OR pid=103002)",
    ] {
        gono_forum_web::db::query(sql)
            .execute(db.executor())
            .await
            .unwrap();
    }
    let mut thread = content::thread(db, 103).await.unwrap().unwrap();
    assert_eq!(thread.author, "Deleted user");
    assert_eq!((thread.avatar, thread.avatar_aid), (0, 0));
    content::load_replies(
        db,
        &mut thread,
        1,
        false,
        None,
        1,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    let root = thread.posts.iter().find(|post| post.pid == 103002).unwrap();
    assert_eq!(root.author, "Deleted user");
    assert_eq!((root.avatar, root.avatar_aid), (0, 0));
    assert_eq!(
        (root.root_position, root.sub_position, root.root_floor),
        (1, 0, 0)
    );
    assert!(root.parent_label.is_empty());
    let child = thread.posts.iter().find(|post| post.pid == 103056).unwrap();
    assert_eq!(
        (child.root_position, child.sub_position, child.root_floor),
        (1, 1, 2)
    );
    assert_eq!(child.parent_label, "Reply to Deleted user · #2");
    for (pid, flat, expected) in [
        (thread.first_pid, false, "/thread-103#floor-1"),
        (103002, false, "/thread-103#floor-2"),
        (
            103076,
            false,
            "/thread-103?sub_root=103002&sub_page=2#floor-76",
        ),
        (103076, true, "/thread-103-2?view=flat#floor-76"),
    ] {
        assert_eq!(
            content::post_location(
                db,
                103,
                pid,
                flat,
                gono_forum_web::test_support::SEEDED_PAGING
            )
            .await
            .unwrap()
            .as_deref(),
            Some(expected)
        );
    }
    let list = content::list_threads(db, "", gono_forum_web::urls::Sort::Latest, None, 1, 100)
        .await
        .unwrap();
    let listed = list.threads.iter().find(|thread| thread.id == 103).unwrap();
    assert_eq!((listed.avatar, listed.avatar_aid), (0, 0));
    assert_eq!(
        (listed.first_pid, listed.first_version, listed.edited_at),
        (0, 0, 0)
    );
    assert_eq!(listed.body, [String::new()]);
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn custom_forum_configuration_cannot_panic() {
    let services = database().await;
    let db = services.db.clone();
    // Forums come only from the database: with only one forum in the database, the home page and that forum work as
    // usual and other URLs return 404.
    let config = services.config.clone();
    content::seed_forums(&db).await.unwrap();
    gono_forum_web::db::query("DELETE FROM gonof_forum_forum WHERE slug<>'daily'")
        .execute(db.executor())
        .await
        .unwrap();
    let app = router::build(config, &project_root(), db);
    assert_eq!(support::fetch(&app, "/", false).await.0, StatusCode::OK);
    assert_eq!(
        support::fetch(&app, "/daily", false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        support::fetch(&app, "/ai", false).await.0,
        StatusCode::NOT_FOUND
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn serves_static_assets_without_exposing_workspace() {
    let (app, services) = app().await;
    let (status, headers, _) = support::fetch(&app, "/static/css/forum.css", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    for path in [
        "/config/default.toml",
        "/.local/reference/home.html",
        "/static/../Cargo.toml",
    ] {
        assert_eq!(
            support::fetch(&app, path, false).await.0,
            StatusCode::NOT_FOUND
        );
    }
    services.cleanup().await.unwrap();
}

/// Pages link assets with this process's asset version; only that `v=` is cached for good. An address with another
/// build's version, which a process of the old build answers with its old bytes during a rolling release, is
/// revalidated instead. The script catalogs are served by the application in every build.
#[tokio::test]
async fn only_this_processs_asset_version_is_cached_for_good() {
    let (app, services) = app().await;
    let (_, _, home) = support::fetch(&app, "/", false).await;
    let start =
        home.find("/static/js/site.js?v=").expect("site.js") + "/static/js/site.js?v=".len();
    let version = &home[start..start + home[start..].find('"').unwrap()];
    assert!(
        home.contains(&format!("/static/i18n/en.js?v={version}\"")),
        "{home}"
    );
    for (path, rule) in [
        (
            format!("/static/i18n/en.js?v={version}"),
            "public, max-age=31536000, immutable",
        ),
        ("/static/i18n/en.js?v=0123456789ab".to_owned(), "no-cache"),
        ("/static/i18n/en.js".to_owned(), "no-cache"),
    ] {
        let (status, headers, _) = support::fetch(&app, &path, false).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers["cache-control"], rule, "{path}");
    }
    services.cleanup().await.unwrap();
}

/// The header forum navigation, with the aria-current that varies with the current page removed.
fn forum_nav(body: &str) -> String {
    let start = body
        .find("<nav class=\"masthead__boards\"")
        .expect("the header must have the forum navigation");
    body[start..start + body[start..].find("</nav>").unwrap()].replace(" aria-current=\"page\"", "")
}

// The admin area validates the navigation when it is saved, but existing data may still be invalid (rules tightened,
// database edited directly). Navigation only affects the header, so a parse failure must fall back to the default forum
// navigation rather than make every page fail, including the admin page used to fix it.
#[tokio::test]
async fn invalid_stored_navigation_falls_back_to_forum_links() {
    let (app, services) = app().await;
    let (_, _, before) = support::fetch(&app, "/", false).await;
    let default_nav = forum_nav(&before);
    for slug in ["daily", "showcase", "ai", "hosting", "releases", "meta"] {
        assert!(
            default_nav.contains(&format!("href=\"/{slug}\"")),
            "{default_nav}"
        );
    }
    gono_forum_web::db::query(
        "INSERT INTO gonof_common_settings(skey,svalue) VALUES('settings.navigation.body',?) \
         ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)",
    )
    .bind("A line without a separator\nOther site | https://example.com")
    .execute(services.db.executor())
    .await
    .unwrap();
    for htmx in [false, true] {
        for path in ["/", "/ai", "/thread-103"] {
            let (status, _, body) = support::fetch(&app, path, htmx).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(forum_nav(&body), default_nav, "{path}");
            assert!(!body.contains("example.com"));
        }
    }
    services.cleanup().await.unwrap();
}

// The whole site has `<body hx-boost="true">`. If an attachment link in a post body were boosted, htmx would swap the
// file content into body as a page and the address bar would become /media/…, instead of downloading.
#[tokio::test]
async fn attachment_links_and_downloads_bypass_boost() {
    let (app, services) = app().await;
    let pid: i64 =
        gono_forum_web::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE first_tid=103")
            .fetch_one(&services.db.pool)
            .await
            .unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_post SET message=? WHERE pid=?")
        .bind("See [notes a.pdf](attachment:9), and also [rules](/rules).")
        .bind(pid)
        .execute(services.db.executor())
        .await
        .unwrap();
    let (status, _, body) = support::fetch(&app, "/thread-103", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<body hx-boost=\"true\""));
    assert!(
        body.contains(&format!(
            "<a href=\"/media/9?pid={pid}\" hx-boost=\"false\" rel=\"nofollow ugc noreferrer\">notes a.pdf</a>"
        )),
        "attachment links must turn boost off"
    );
    assert!(body.contains("<a href=\"/rules\" rel=\"nofollow ugc noreferrer\">rules</a>"));
    // /media links that are still boosted (e.g. a hand-written <a>) are caught by the download endpoint: it does not
    // read the object, and tells htmx to fall back to a native browser navigation.
    let target = format!("/media/9?pid={pid}");
    let (status, headers, body) = support::fetch(&app, &target, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], target.as_str());
    assert_eq!(headers["cache-control"], "private, no-store");
    assert!(body.is_empty());
    let (status, headers, _) = support::fetch(&app, &target, false).await;
    assert_ne!(status, StatusCode::OK);
    assert!(!headers.contains_key("hx-redirect"));
    services.cleanup().await.unwrap();
}

// navigation.js's popstate handler must be registered before htmx to intercept its history restore; adding
// defer/async or swapping the order silently falls back to htmx's old content path. The script tags are static text in
// base.html that rendering does not change, so the template source is checked directly, with no database fixture.
#[test]
fn navigation_script_stays_synchronous_and_before_htmx() {
    const BASE: &str = include_str!("../templates/layout/base.html");
    let navigation = BASE
        .find("/static/js/navigation.js")
        .expect("base.html must load navigation.js");
    let htmx = BASE
        .find("/static/js/vendor/htmx.min.js")
        .expect("base.html must load htmx");
    assert!(
        navigation < htmx,
        "navigation.js must come before htmx, or the popstate listeners run in the wrong order"
    );
    let start = BASE[..navigation]
        .rfind('<')
        .expect("navigation.js must sit inside a tag");
    let end = navigation
        + BASE[navigation..]
            .find('>')
            .expect("the navigation.js tag must be closed");
    let tag = &BASE[start..=end];
    assert!(
        !tag.contains("defer") && !tag.contains("async"),
        "navigation.js must not have defer/async: {tag}"
    );
}

/// Sitemap pages split the thread ids; with a large id and few threads before it, the span grows so that the thread
/// still has a page within the 50,000 the index may list.
#[tokio::test]
async fn a_thread_past_250_million_ids_is_in_the_sitemap() {
    const FAR: i64 = 250_000_001;
    let services = database().await;
    let db = services.db.clone();
    content::seed_fixture(&db).await.unwrap();
    gono_forum_web::test_support::set_setting(&db, "settings.seo.indexing", "true")
        .await
        .unwrap();
    let pid: i64 =
        gono_forum_web::db::query_scalar("SELECT MAX(pid) + 1 FROM gonof_forum_post_tableid")
            .fetch_one(db.executor())
            .await
            .unwrap();
    for sql in [
        format!(
            "INSERT INTO gonof_forum_thread (tid,fid,authorid,subject,dateline,lastpost,visibility,author,family_fid,maxposition)
             SELECT {FAR},fid,authorid,'Far away',dateline,lastpost,visibility,author,family_fid,1 FROM gonof_forum_thread WHERE tid=103"
        ),
        format!("INSERT INTO gonof_forum_post_tableid (pid) VALUES ({pid})"),
        format!(
            "INSERT INTO gonof_forum_post (pid,fid,tid,authorid,first,first_tid,position,message,dateline)
             SELECT {pid},fid,{FAR},authorid,1,{FAR},1,message,dateline FROM gonof_forum_post WHERE first_tid=103"
        ),
    ] {
        gono_forum_web::db::query(&sql)
            .execute(db.executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let app = router::build(services.config.clone(), &project_root(), db);
    let (status, _, index) = support::fetch(&app, "/sitemap.xml", false).await;
    assert_eq!(status, StatusCode::OK);
    let pages = index.matches("<sitemap>").count();
    assert!(pages <= 50_000, "{pages} sitemaps");
    let last = format!("/sitemap.xml?page={pages}<");
    assert!(index.contains(&last), "the index ends at page {pages}");
    let (status, _, xml) = support::fetch(&app, &format!("/sitemap.xml?page={pages}"), false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(xml.contains(&format!("/thread-{FAR}<")), "{xml}");
    services.cleanup().await.unwrap();
}
