//! Sub-path deployment: when the forum URL has a path, the entry layer serves only under
//! the prefix and strips it before routing; in-site URLs in pages, redirects, HX headers, path-scoped Cookies,
//! canonical, sitemap, feeds, robots, manifest, body text and mail links all carry the prefix without doubling it;
//! changing the path requires a second confirmation by typing confirm, and the command line can rescue it directly.
#[path = "support/http_auth.rs"]
mod http_auth;
use std::collections::{BTreeSet, VecDeque};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content,
    content::writes::{ContentWriter, NewThread, WriteConfig},
    db,
    i18n::Lang,
    identity::{self, Actor},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9961;
const MEMBER: i64 = 9962;
const PREFIX: &str = "/bbs";

struct Fixture {
    f: TestFixture,
    app: Router,
    admin: identity::SessionGrant,
    identity: identity::Identity,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        for (uid, role, verified) in [(ADMIN, 1, 1), (MEMBER, 0, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,?,?,1)")
                .bind(uid)
                .bind(format!("subpath-{uid}"))
                .bind(format!("subpath-{uid}@example.test"))
                .bind(verified)
                .bind(role)
                .execute(f.db.executor())
                .await
                .unwrap();
        }
        let identity = identity::Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let admin = http_auth::login(&identity, &f.db, ADMIN).await;
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self {
            f,
            app,
            admin,
            identity,
        }
    }

    fn actor(&self) -> Actor {
        Actor {
            uid: ADMIN,
            credentials_version: self.admin.member.credentials_version,
            api_key: None,
        }
    }

    async fn send(&self, request: Request<Body>) -> Reply {
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

    /// The administrator's session cookie as the forum named by the start of `path` calls it: after its path
    /// prefix and port (docs/install.md, "Reverse proxy").
    fn session_cookie(&self, path: &str) -> String {
        let prefix = ["/bbs", "/forum"]
            .into_iter()
            .find(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
            .unwrap_or_default()
            .replace('/', "_");
        format!(
            "gonof_session{prefix}__port58833={}",
            self.admin.session_token
        )
    }

    async fn get(&self, path: &str, signed_in: bool) -> Reply {
        let mut request = Request::builder().uri(path);
        if signed_in {
            request = request.header(header::COOKIE, self.session_cookie(path));
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::COOKIE, self.session_cookie(path))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body.finish()))
                .unwrap(),
        )
        .await
    }

    async fn thread(&self, title: &str, body: &str) -> i64 {
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(self.f.db.executor())
            .await
            .unwrap();
        ContentWriter::new(self.f.db.clone(), WriteConfig::default())
            .publish_at(
                ADMIN,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                identity::now() - 60,
            )
            .await
            .unwrap()
            .tid
    }

    async fn site_url(&self) -> String {
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey='settings.site.url'")
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }
}

/// In-site (root-relative) URLs written in URL attributes of the page, together with the attribute names.
fn local_urls(html: &str) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for attr in [
        "href",
        "src",
        "action",
        "hx-post",
        "formaction",
        "data-alternate-href",
    ] {
        let needle = format!("{attr}=\"");
        for (at, _) in html.match_indices(&needle) {
            if html[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            let value = &html[at + needle.len()..];
            let value = &value[..value.find('"').unwrap_or(value.len())];
            if value.starts_with('/') && !value.starts_with("//") {
                found.push((attr, value.replace("&amp;", "&")));
            }
        }
    }
    found
}

/// In-site URLs are all under the prefix, and none are doubled.
fn assert_prefixed(page: &str, url: &str) {
    assert!(
        url == PREFIX
            || url.starts_with(&format!("{PREFIX}/"))
            || url.starts_with(&format!("{PREFIX}?")),
        "{url} in {page} lacks the prefix"
    );
    assert!(
        !url.contains("/bbs/bbs"),
        "{url} in {page} has the prefix twice"
    );
}

/// Walks the in-site links starting from the home page (and the admin area): in-site URLs on every page carry the
/// prefix, redirects stay under the prefix, and links do not 404 or 5xx.
async fn crawl(f: &Fixture, starts: &[&str], signed_in: bool, budget: usize) -> usize {
    let mut queue: VecDeque<String> = starts.iter().map(|s| (*s).to_owned()).collect();
    let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
    let mut visited = 0;
    while let Some(url) = queue.pop_front() {
        if visited == budget {
            break;
        }
        visited += 1;
        let reply = f.get(&url, signed_in).await;
        assert!(
            reply.status != StatusCode::NOT_FOUND && !reply.status.is_server_error(),
            "{url} returned {}",
            reply.status
        );
        if reply.status.is_redirection() {
            let location = reply.headers[header::LOCATION].to_str().unwrap();
            let location = location
                .strip_prefix("http://127.0.0.1:58833")
                .unwrap_or(location);
            assert_prefixed(&url, location);
            continue;
        }
        let html = reply
            .headers
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.to_str().unwrap().starts_with("text/html"));
        if !html {
            continue;
        }
        for (attr, link) in local_urls(&reply.body) {
            assert_prefixed(&url, &link);
            // Only follow links; form targets are mostly POST-only URLs.
            if attr != "href" {
                continue;
            }
            let page = link.split('#').next().unwrap().to_owned();
            // Do not click things that change state or download: sign-out, attachments, static assets (tested
            // separately).
            if page.contains("logout") || page.contains("/media/") || page.contains("/static/") {
                continue;
            }
            if seen.insert(page.clone()) {
                queue.push_back(page);
            }
        }
    }
    visited
}

#[tokio::test]
async fn everything_is_served_and_linked_under_the_prefix() {
    let f = Fixture::new().await;
    let first = f
        .thread("First thread under the sub-path", "Body text")
        .await;
    let site = format!("{TEST_SITE_URL}{PREFIX}");
    let body = format!(
        "[Rules](/rules) and [already prefixed](/bbs/thread-{first}), external <a href=\"//x.example/y\">out</a>\n\n{site}/thread-{first}\n"
    );
    let second = f.thread("Second thread under the sub-path", &body).await;
    // The command-line rescue uses the same function: no administrator check, no second confirmation, and the audit
    // records the source as cli.
    let (before, saved) = runtime::set_site_url(&f.f.db, &f.f.global, &format!("{site}/"))
        .await
        .unwrap();
    assert_eq!(before, TEST_SITE_URL);
    assert_eq!(
        saved.site.url, site,
        "the trailing slash is normalized away"
    );
    let source: String = db::query_scalar(
        "SELECT source FROM gonof_common_log WHERE type='site.url.cli' AND uid IS NULL ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(f.f.db.executor())
    .await
    .unwrap();
    assert_eq!(source, "cli");
    runtime::update(&f.f.db, &f.f.global, f.actor(), "site.settings.save", |s| {
        s.seo.indexing = true;
    })
    .await
    .unwrap();

    // Entry: 404 outside the prefix; the home page without the trailing slash gets 301. The health check answers
    // under the prefix and at the root, where forum.install.sh checks it without knowing the forum address.
    for outside in [
        "/",
        "/healthz/",
        "/thread-1",
        "/bbsx/",
        "/static/js/site.js",
    ] {
        assert_eq!(
            f.get(outside, false).await.status,
            StatusCode::NOT_FOUND,
            "{outside}"
        );
    }
    assert_eq!(f.get("/bbs/healthz", false).await.status, StatusCode::OK);
    assert_eq!(f.get("/healthz", false).await.status, StatusCode::OK);
    for (from, to) in [
        ("/bbs", "/bbs/"),
        ("/bbs?sort=created", "/bbs/?sort=created"),
    ] {
        let reply = f.get(from, false).await;
        assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY, "{from}");
        assert_eq!(reply.headers[header::LOCATION], to);
    }

    // Home page: the prefix the page frame gives scripts, canonical and share URLs, styles and scripts, header links.
    let home = f.get("/bbs/", false).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("data-base=\"/bbs\""));
    assert!(
        home.body
            .contains(&format!("<link rel=\"canonical\" href=\"{site}/\">"))
    );
    assert!(
        home.body
            .contains(&format!("<meta property=\"og:url\" content=\"{site}/\">"))
    );
    assert!(home.body.contains("href=\"/bbs/site.webmanifest\""));
    assert!(home.body.contains("src=\"/bbs/static/js/site.js?v="));
    assert!(home.body.contains("href=\"/bbs/daily\""));
    // Session Cookies keep `Path=/`: the sign-in page issues the anonymous CSRF Cookie.
    let login = f.get("/bbs/login", false).await;
    assert_eq!(login.status, StatusCode::OK);
    let cookies: Vec<_> = login
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("gonof_csrf_bbs__port58833=") && c.contains("Path=/;")),
        "{cookies:?}"
    );

    // Body: root-relative gets the prefix, already prefixed is not repeated, protocol-relative is untouched; in-site
    // thread cards recognize absolute URLs with the prefix.
    let thread = f.get(&format!("/bbs/thread-{second}"), false).await;
    assert_eq!(thread.status, StatusCode::OK);
    assert!(thread.body.contains("href=\"/bbs/rules\""));
    assert!(thread.body.contains(&format!(
        "href=\"/bbs/thread-{first}\" rel=\"nofollow ugc noreferrer\">already prefixed"
    )));
    assert!(thread.body.contains("href=\"//x.example/y\""));
    assert!(
        thread.body.contains(&format!(
            "<a class=\"topic-embed__title\" href=\"/bbs/thread-{first}\">"
        )),
        "in-site thread card"
    );
    assert!(
        thread
            .body
            .contains(&format!("\"url\":\"{site}/thread-{second}\"")),
        "thread page JSON-LD"
    );

    // Static assets: fonts use relative URLs and work as usual under the prefix. The fixture root is a temporary
    // directory, so first put a real stylesheet into it.
    let css = f.f.root.join("apps/web/static/css");
    std::fs::create_dir_all(&css).unwrap();
    std::fs::copy(
        gono_forum_web::project_root().join("apps/web/static/css/fonts.css"),
        css.join("fonts.css"),
    )
    .unwrap();
    let fonts = f.get("/bbs/static/css/fonts.css", false).await;
    assert_eq!(fonts.status, StatusCode::OK);
    assert!(fonts.body.contains("url(../fonts/") && !fonts.body.contains("url(/static"));

    // Redirects: the 303 to the sign-in page and the HX-Redirect when an attachment is boosted both carry the prefix.
    let my = f.get("/bbs/my", false).await;
    assert!(my.status.is_redirection());
    assert!(
        my.headers[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/bbs/login"),
        "{:?}",
        my.headers
    );
    let boosted = f
        .send(
            Request::builder()
                .uri("/bbs/media/12345")
                .header("HX-Request", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(boosted.headers["hx-redirect"], "/bbs/media/12345");

    // Cookies sent only to the display page carry the prefix, redirecting back to the display page.
    let issued = f.post("/bbs/my/feed/enable", &[]).await;
    assert_eq!(issued.status, StatusCode::SEE_OTHER, "{}", issued.body);
    assert_eq!(issued.headers[header::LOCATION], "/bbs/my/feed");
    let reveal: Vec<_> = issued
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    assert!(
        reveal
            .iter()
            .any(|c| c.starts_with("gonof_feed_reveal_bbs__port58833=")
                && c.contains("Path=/bbs/my/feed;")),
        "{reveal:?}"
    );

    // robots, sitemap, feeds, manifest.
    let robots = f.get("/bbs/robots.txt", false).await.body;
    assert!(robots.contains("Disallow: /bbs/manage\n"), "{robots}");
    assert!(
        robots.contains(&format!("Sitemap: {site}/sitemap.xml\n")),
        "{robots}"
    );
    assert!(
        !robots
            .lines()
            .any(|line| line.starts_with("Disallow: /") && !line.starts_with("Disallow: /bbs/")),
        "{robots}"
    );
    let index = f.get("/bbs/sitemap.xml", false).await.body;
    assert!(
        index.contains(&format!("<loc>{site}/sitemap.xml?page=1</loc>")),
        "{index}"
    );
    let sitemap = f.get("/bbs/sitemap.xml?page=1", false).await.body;
    assert!(
        sitemap.contains(&format!("<loc>{site}/thread-{first}</loc>")),
        "{sitemap}"
    );
    assert!(
        !sitemap.contains(&format!("<loc>{TEST_SITE_URL}/thread")),
        "{sitemap}"
    );
    let feed = f.get("/bbs/feed.xml", false).await.body;
    assert!(
        feed.contains(&format!("{site}/thread-{first}"))
            && !feed.contains(&format!("{TEST_SITE_URL}/thread")),
        "{feed}"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&f.get("/bbs/site.webmanifest", false).await.body).unwrap();
    assert_eq!(manifest["start_url"], "/bbs/");
    assert_eq!(manifest["scope"], "/bbs/");
    assert_eq!(manifest["icons"][0]["src"], "/bbs/static/img/app-192.png");
    // The admin SEO settings give the robots to merge into the domain root.
    let search = f.get("/bbs/manage/settings/search", true).await.body;
    assert!(
        search.contains("data-robots-merge") && search.contains("Disallow: /bbs/manage"),
        "{search}"
    );

    // Walk the in-site links: anonymously from the home page, as administrator from the home page and the admin area.
    assert!(crawl(&f, &["/bbs/", &format!("/bbs/thread-{second}")], false, 60).await > 20);
    assert!(crawl(&f, &["/bbs/", "/bbs/manage", "/bbs/my"], true, 150).await > 60);

    // Mail: the base URL carries the prefix, with links appended after it.
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=?")
        .bind(MEMBER)
        .execute(f.f.db.executor())
        .await
        .unwrap();
    f.identity.resend_verification(MEMBER).await.unwrap();
    let mailbox = f.f.root.join("subpath-mail");
    let mailer = gono_forum_web::mail::Mailer::new(
        f.f.db.clone(),
        gono_forum_web::secrets::CommonConfig::default(),
        mailbox.clone(),
        f.f.global.clone(),
    );
    // Enabling the feed URL earlier may have queued an extra alert email, so find the verification email by content.
    assert!(
        mailer
            .deliver_pending(identity::now())
            .await
            .unwrap()
            .delivered
            >= 1
    );
    let texts: Vec<String> = std::fs::read_dir(&mailbox)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    assert!(
        texts
            .iter()
            .any(|text| text.contains(&format!("{site}/verify-email?token="))),
        "{texts:?}"
    );
    let _ = std::fs::remove_dir_all(&mailbox);
    f.f.cleanup().await.unwrap();
}

#[tokio::test]
async fn changing_the_path_needs_a_typed_confirmation() {
    let f = Fixture::new().await;
    let site = format!("{TEST_SITE_URL}{PREFIX}");
    runtime::set_site_url(&f.f.db, &f.f.global, &site)
        .await
        .unwrap();
    // An ordinary save cannot change the path; changing only the site name works as usual.
    let refused = runtime::update(&f.f.db, &f.f.global, f.actor(), "site.settings.save", |s| {
        s.site.url = format!("{TEST_SITE_URL}/forum");
    })
    .await
    .unwrap_err();
    assert_eq!(
        refused.to_string(),
        Lang::EN.config_error_prefix_needs_confirm()
    );
    let form = |url: &str| {
        vec![
            ("site_name", "Sub-path Site".to_owned()),
            ("site_url", url.to_owned()),
        ]
    };
    let post = |fields: Vec<(&'static str, String)>| {
        let f = &f;
        async move {
            let borrowed: Vec<(&str, &str)> =
                fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
            f.post("/bbs/manage/settings", &borrowed).await
        }
    };
    let same = post(form(&site)).await;
    assert_eq!(
        same.status,
        StatusCode::SEE_OTHER,
        "an unchanged path needs no confirmation"
    );
    assert_eq!(
        same.headers[header::LOCATION],
        "/bbs/manage/settings?saved=1"
    );

    // The path is about to change: show the confirmation page first, with the database unchanged.
    let target = format!("{TEST_SITE_URL}/forum");
    let first = post(form(&target)).await;
    assert_eq!(first.status, StatusCode::OK);
    assert!(first.body.contains("data-prefix-confirm"), "{}", first.body);
    assert!(first.body.contains("name=\"prefix_from\" value=\"/bbs\""));
    assert!(
        first.body.contains(&format!("<code>{site}/manage</code>"))
            && first
                .body
                .contains(&format!("<code>{target}/manage</code>"))
    );
    assert!(
        first.body.contains("location /forum {")
            && first
                .body
                .contains("rewrite ^/bbs(?:/(.*))?$ /forum/$1 permanent;")
    );
    assert!(
        first
            .body
            .contains(&format!("gono-forum-cli set-site-url {site}"))
    );
    assert_eq!(f.site_url().await, site);

    // Not saved without typing confirm correctly.
    let mut wrong = form(&target);
    wrong.push(("prefix_from", "/bbs".into()));
    wrong.push(("confirm_prefix", "Confirm!".into()));
    let wrong = post(wrong).await;
    assert!(
        wrong
            .body
            .contains("Type confirm (in lowercase) in the confirmation box")
    );
    assert_eq!(f.site_url().await, site);

    // The current value seen at confirmation has been changed: confirm again against the new current value.
    let mut stale = form(&target);
    stale.push(("prefix_from", "/old".into()));
    stale.push(("confirm_prefix", "confirm".into()));
    let stale = post(stale).await;
    // The message has an apostrophe, which the page escapes; compare the part after it.
    assert!(
        stale
            .body
            .contains("path was just changed; please confirm again with the current value"),
        "{}",
        stale.body
    );
    assert_eq!(f.site_url().await, site);

    // Typing confirm: saves and redirects under the new prefix; the old prefix immediately 404s and the new prefix
    // works.
    let mut confirmed = form(&target);
    confirmed.push(("prefix_from", "/bbs".into()));
    confirmed.push(("confirm_prefix", " confirm ".into()));
    let saved = post(confirmed).await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        saved.headers[header::LOCATION],
        "/forum/manage/settings?saved=1"
    );
    assert_eq!(f.site_url().await, target);
    assert_eq!(
        f.get("/bbs/manage/settings", true).await.status,
        StatusCode::NOT_FOUND
    );
    let page = f.get("/forum/manage/settings?saved=1", true).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("action=\"/forum/manage/settings\""));

    // Changing back to the root path also requires confirmation; the command-line rescue does not.
    let back = f
        .post(
            "/forum/manage/settings",
            &[("site_name", "Sub-path Site"), ("site_url", TEST_SITE_URL)],
        )
        .await;
    assert!(back.body.contains("data-prefix-confirm") && back.body.contains("the root path /"));
    runtime::set_site_url(&f.f.db, &f.f.global, "")
        .await
        .unwrap();
    assert_eq!(f.site_url().await, "");
    assert_eq!(f.get("/healthz", false).await.status, StatusCode::OK);
    f.f.cleanup().await.unwrap();
}
