//! Multi-domain sites: the switch and the main domain row, splitting sites by Host and path,
//! lists and feeds within scope, redirects to the canonical domain outside scope, canonical and sitemaps by the
//! canonical domain, main-site-only URLs back to the main domain, structural validation and path-change confirmation in
//! the admin area, linkage between the forum URL and forums, and account mail by the originating domain.
#[path = "support/http_auth.rs"]
mod http_auth;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content,
    content::writes::{ContentWriter, NewThread, WriteConfig},
    db,
    domains::DomainMap,
    identity::{self, Actor},
    organization, runtime,
    test_support::{TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9971;
const MEMBER: i64 = 9972;
/// Host of the forum URL `TEST_SITE_URL`.
const MAIN: &str = "127.0.0.1:58833";
const DEMO2: &str = "demo2.test";
const DEMO2_URL: &str = "http://demo2.test";

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

impl Reply {
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .map_or("", |value| value.to_str().unwrap())
    }
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        for (uid, role, verified) in [(ADMIN, 1, 1), (MEMBER, 0, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,?,?,1)")
                .bind(uid)
                .bind(format!("domains-{uid}"))
                .bind(format!("domains-{uid}@example.test"))
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

    /// The administrator's session cookie as the forum on `host` names it: with the port when the host has one
    /// (docs/install.md, "Reverse proxy").
    fn session_cookie(&self, host: &str) -> String {
        let port = host
            .rsplit_once(':')
            .map_or_else(String::new, |(_, port)| format!("__port{port}"));
        format!("gonof_session{port}={}", self.admin.session_token)
    }

    async fn get(&self, host: &str, path: &str, signed_in: bool) -> Reply {
        let mut request = Request::builder().uri(path).header(header::HOST, host);
        if signed_in {
            request = request.header(header::COOKIE, self.session_cookie(host));
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, host: &str, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, host)
                .header(header::COOKIE, self.session_cookie(host))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body.finish()))
                .unwrap(),
        )
        .await
    }

    async fn fid(&self, slug: &str) -> i64 {
        db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug=?")
            .bind(slug)
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }

    async fn thread(&self, slug: &str, title: &str) -> i64 {
        let fid = self.fid(slug).await;
        ContentWriter::new(self.f.db.clone(), WriteConfig::default())
            .publish_at(
                ADMIN,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: "Body text".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                identity::now() - 60,
            )
            .await
            .unwrap()
            .tid
    }

    async fn map(&self) -> DomainMap {
        DomainMap::load(&self.f.db).await.unwrap()
    }

    /// Turns the switch on (through the admin form) and returns the id of the main domain row.
    async fn enable(&self) -> i64 {
        let reply = self
            .post(MAIN, "/manage/domains/switch", &[("enabled", "on")])
            .await;
        assert!(reply.status.is_redirection(), "{}", reply.body);
        self.map().await.main(TEST_SITE_URL).unwrap().domain_id
    }

    /// Adds a row and returns the page after saving (a redirect on success).
    async fn add(&self, url: &str, canonical: &str, forums: &[&str]) -> Reply {
        let mut fields: Vec<(String, String)> = vec![
            ("domain_id".into(), "0".into()),
            ("version".into(), "0".into()),
            ("site_url".into(), url.into()),
            ("site_name".into(), "Customer Feedback Center".into()),
            ("canonical".into(), canonical.into()),
            ("enabled".into(), "1".into()),
        ];
        for slug in forums {
            fields.push((format!("forum_{}", self.fid(slug).await), "1".into()));
        }
        let borrowed: Vec<(&str, &str)> = fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        self.post(MAIN, "/manage/domains", &borrowed).await
    }

    async fn indexing(&self) {
        runtime::update(&self.f.db, &self.f.global, self.actor(), "test", |s| {
            s.seo.indexing = true;
        })
        .await
        .unwrap();
    }
}

fn canonical(body: &str) -> &str {
    let at = body
        .find("<link rel=\"canonical\" href=\"")
        .expect("the page has a canonical")
        + 28;
    &body[at..at + body[at..].find('"').unwrap()]
}

#[tokio::test]
async fn domain_load_failure_stops_scoped_routes_and_recovers_after_repair() {
    let f = Fixture::new().await;
    f.thread("daily", "Listed on the main site only").await;
    f.enable().await;
    assert!(
        f.add(DEMO2_URL, "self", &["meta"])
            .await
            .status
            .is_redirection()
    );
    let home = f.get(DEMO2, "/", false).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(!home.body.contains("Listed on the main site only"));
    assert_eq!(
        f.get(DEMO2, "/links", false).await.status,
        StatusCode::MOVED_PERMANENTLY
    );

    let set_canonical = |value| {
        db::query("UPDATE gonof_common_site_domain SET canonical=? WHERE site_url=?")
            .bind(value)
            .bind(DEMO2_URL)
            .execute(f.f.db.executor())
    };
    set_canonical("bad").await.unwrap();
    f.f.global.clear_local();
    for path in ["/", "/links", "/daily/feed.xml"] {
        let reply = f.get(DEMO2, path, false).await;
        assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
        assert!(
            reply
                .body
                .contains("The canonical value in the domain table is not valid"),
            "{path}"
        );
        assert!(!reply.body.contains("Listed on the main site only"));
    }
    let api = f.get(DEMO2, "/api/v1/forums", false).await;
    assert_eq!(api.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&api.body).unwrap()["code"],
        "internal"
    );
    assert_eq!(f.get(DEMO2, "/healthz", false).await.status, StatusCode::OK);
    let static_dir = f.f.root.join("apps/web/static");
    std::fs::create_dir_all(&static_dir).unwrap();
    std::fs::write(static_dir.join("probe.txt"), "asset").unwrap();
    assert_eq!(f.get(DEMO2, "/static/probe.txt", false).await.body, "asset");

    set_canonical("self").await.unwrap();
    let home = f.get(DEMO2, "/", false).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(!home.body.contains("Listed on the main site only"));
    assert_eq!(
        f.get(DEMO2, "/links", false).await.status,
        StatusCode::MOVED_PERMANENTLY
    );
    f.f.cleanup().await.unwrap();
}

#[tokio::test]
async fn domains_scope_listings_redirect_and_point_canonicals_at_the_home_domain() {
    let f = Fixture::new().await;
    let daily = f.thread("daily", "A thread in Daily").await;
    let meta = f.thread("meta", "Feedback in Meta").await;
    // When off: other Hosts show all forums like the main site, without reading the domain table.
    let off = f.get(DEMO2, "/", false).await;
    assert_eq!(off.status, StatusCode::OK);
    assert!(off.body.contains("A thread in Daily") && off.body.contains("Feedback in Meta"));

    // Turn on: creates the main domain row with all 6 top-level forums checked.
    let main_id = f.enable().await;
    assert_eq!(f.map().await.get(main_id).unwrap().forums.len(), 6);
    // demo2 shows only the meta and releases forums, with itself as canonical.
    let saved = f.add(DEMO2_URL, "self", &["meta", "releases"]).await;
    assert!(saved.status.is_redirection(), "{}", saved.body);
    // The same forum cannot have a second other domain that is canonical for itself; one whose canonical points to
    // the main domain is allowed.
    let twice = f.add("http://demo3.test", "self", &["meta"]).await;
    assert_eq!(twice.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        twice
            .body
            .contains("a forum can have only one canonical domain"),
        "{}",
        twice.body
    );
    let mirror = f.add("http://demo3.test", "main", &["meta"]).await;
    assert!(mirror.status.is_redirection(), "{}", mirror.body);
    // A URL equal to the forum URL is the main domain and cannot be registered again; differing only in scheme still
    // counts as the same location.
    assert_eq!(
        f.add(TEST_SITE_URL, "self", &[]).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        f.add("https://demo2.test", "self", &[]).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // demo2's home page lists only threads within scope, and the header and title use its site name.
    let home = f.get(DEMO2, "/", false).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Feedback in Meta"), "{}", home.body);
    assert!(!home.body.contains("A thread in Daily"), "{}", home.body);
    assert!(
        home.body.contains("Customer Feedback Center</a>"),
        "header text"
    );
    assert!(
        home.body
            .contains("<title>Customer Feedback Center</title>"),
        "{}",
        home.body
    );
    assert_eq!(canonical(&home.body), format!("{DEMO2_URL}/"));
    // The main domain shows everything as usual, and the header still shows the main site's brand text.
    let main_home = f.get(MAIN, "/", false).await;
    assert!(
        main_home.body.contains("A thread in Daily") && main_home.body.contains("Feedback in Meta")
    );
    assert!(main_home.body.contains("Sample Forum</a>"));

    // Threads and forums outside scope redirect to the same URL on the canonical domain; ones within scope as usual.
    let away = f.get(DEMO2, &format!("/thread-{daily}"), false).await;
    assert_eq!(away.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(away.location(), format!("{TEST_SITE_URL}/thread-{daily}"));
    let forum = f.get(DEMO2, "/daily?sort=created", false).await;
    assert_eq!(
        forum.location(),
        format!("{TEST_SITE_URL}/daily?sort=created")
    );
    assert_eq!(
        f.get(DEMO2, "/new/daily", true).await.location(),
        format!("{TEST_SITE_URL}/new/daily")
    );
    assert_eq!(
        f.get(DEMO2, "/daily/feed.xml", false).await.location(),
        format!("{TEST_SITE_URL}/daily/feed.xml")
    );
    // The canonical domain of meta is demo2: opening it on either side points to demo2.
    let there = f.get(DEMO2, &format!("/thread-{meta}"), false).await;
    assert_eq!(there.status, StatusCode::OK);
    assert_eq!(canonical(&there.body), format!("{DEMO2_URL}/thread-{meta}"));
    let here = f.get(MAIN, &format!("/thread-{meta}"), false).await;
    assert_eq!(here.status, StatusCode::OK);
    assert_eq!(canonical(&here.body), format!("{DEMO2_URL}/thread-{meta}"));
    assert_eq!(
        canonical(&f.get(MAIN, "/meta", false).await.body),
        format!("{DEMO2_URL}/meta")
    );
    assert_eq!(
        canonical(&f.get(MAIN, &format!("/thread-{daily}"), false).await.body),
        format!("{TEST_SITE_URL}/thread-{daily}")
    );
    // demo3, whose canonical points to the main domain: every page points to the same URL on the main domain.
    let mirrored = f.get("demo3.test", &format!("/thread-{meta}"), false).await;
    assert_eq!(
        canonical(&mirrored.body),
        format!("{TEST_SITE_URL}/thread-{meta}")
    );
    assert_eq!(
        canonical(&f.get("demo3.test", "/", false).await.body),
        format!("{TEST_SITE_URL}/")
    );

    // Unregistered Hosts behave like the main domain: all forums, with the site URL derived from this request.
    let other = f.get("other.test", "/", false).await;
    assert!(other.body.contains("A thread in Daily"));
    assert_eq!(canonical(&other.body), "http://other.test/");
    assert_eq!(
        canonical(
            &f.get("other.test", &format!("/thread-{meta}"), false)
                .await
                .body
        ),
        format!("{DEMO2_URL}/thread-{meta}")
    );

    // A request without a Host header cannot derive a URL and is treated as the main domain.
    let hostless = f
        .send(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await;
    assert_eq!(hostless.status, StatusCode::OK);
    assert_eq!(canonical(&hostless.body), format!("{TEST_SITE_URL}/"));

    // Main-site-only URLs: on other domains GET redirects to the main domain and form submissions get 404;
    // unregistered Hosts as usual.
    let manage = f.get(DEMO2, "/manage?x=1", true).await;
    assert_eq!(manage.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(manage.location(), format!("{TEST_SITE_URL}/manage?x=1"));
    assert_eq!(
        f.get(DEMO2, "/ranks", false).await.location(),
        format!("{TEST_SITE_URL}/ranks")
    );
    assert_eq!(
        f.post(DEMO2, "/manage/domains/switch", &[("enabled", "off")])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get("other.test", "/links", false).await.status,
        StatusCode::OK
    );

    // The forum lists of feeds and the member API include only forums within scope.
    let feed = f.get(DEMO2, "/feed.xml", false).await.body;
    assert!(
        feed.contains(&format!("{DEMO2_URL}/thread-{meta}")),
        "{feed}"
    );
    assert!(!feed.contains(&format!("thread-{daily}")), "{feed}");
    assert!(
        feed.contains("<title>Customer Feedback Center</title>"),
        "{feed}"
    );

    // The sitemap lists only content for which this site is canonical; a domain whose canonical points to the main
    // domain has no sitemap, and robots does not mention one either.
    f.indexing().await;
    let main_map = f.get(MAIN, "/sitemap.xml?page=1", false).await.body;
    assert!(
        main_map.contains(&format!("{TEST_SITE_URL}/thread-{daily}</loc>")),
        "{main_map}"
    );
    assert!(!main_map.contains(&format!("thread-{meta}<")), "{main_map}");
    assert!(main_map.contains(&format!("{TEST_SITE_URL}/links</loc>")));
    let demo_map = f.get(DEMO2, "/sitemap.xml?page=1", false).await.body;
    assert!(
        demo_map.contains(&format!("{DEMO2_URL}/thread-{meta}</loc>")),
        "{demo_map}"
    );
    assert!(
        demo_map.contains(&format!("{DEMO2_URL}/meta</loc>")),
        "{demo_map}"
    );
    assert!(
        !demo_map.contains(&format!("thread-{daily}<")),
        "{demo_map}"
    );
    assert!(
        !demo_map.contains("/links</loc>"),
        "verified members and friend links are on the main domain only"
    );
    assert_eq!(
        f.get("demo3.test", "/sitemap.xml", false).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        !f.get("demo3.test", "/robots.txt", false)
            .await
            .body
            .contains("Sitemap:")
    );
    assert!(
        f.get(DEMO2, "/robots.txt", false)
            .await
            .body
            .contains(&format!("Sitemap: {DEMO2_URL}/sitemap.xml"))
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&f.get(DEMO2, "/site.webmanifest", false).await.body).unwrap();
    assert_eq!(manifest["name"], "Customer Feedback Center");

    // Admin list: the main domain comes first; a domain without its own brand images uses the main site's, and a wrong
    // address is 404.
    let list = f.get(MAIN, "/manage/domains", true).await.body;
    assert!(
        list.contains("data-domain-row") && list.contains(DEMO2_URL),
        "{list}"
    );
    assert_eq!(
        f.get(MAIN, "/brand/domain/1/logo/999/source", false)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Turning the switch off: rows in the table are kept, and all Hosts go back to only the main site.
    let off = f
        .post(MAIN, "/manage/domains/switch", &[("enabled", "off")])
        .await;
    assert!(off.status.is_redirection());
    assert_eq!(f.map().await.rows().len(), 3);
    assert!(
        f.get(DEMO2, "/", false)
            .await
            .body
            .contains("A thread in Daily")
    );
    f.f.cleanup().await.unwrap();
}

#[tokio::test]
async fn same_host_sub_paths_settings_and_forums_follow_the_domain_table() {
    let f = Fixture::new().await;
    let meta = f.thread("meta", "Meta under a sub-path").await;
    let main_id = f.enable().await;
    // Root path plus sub-path: the first segment of the sub-path must avoid forum URLs and system URLs.
    for clash in ["/daily", "/manage", "/thread-1"] {
        let refused = f
            .add(&format!("{TEST_SITE_URL}{clash}"), "self", &["meta"])
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{clash}");
    }
    let saved = f
        .add(&format!("{TEST_SITE_URL}/help"), "self", &["meta"])
        .await;
    assert!(saved.status.is_redirection(), "{}", saved.body);
    // Sub-paths cannot be nested.
    assert_eq!(
        f.add(&format!("{TEST_SITE_URL}/help/vip"), "main", &[])
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let help = f.map().await;
    let help = help
        .rows()
        .iter()
        .find(|row| row.site_url.ends_with("/help"))
        .unwrap()
        .clone();

    // On the same host, matching is by path segment: under /help is that scope, and on-site URLs carry its prefix;
    // everything else falls to the main domain at the root path.
    assert_eq!(f.get(MAIN, "/help", false).await.location(), "/help/");
    let page = f.get(MAIN, "/help/", false).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Meta under a sub-path"));
    assert!(page.body.contains("href=\"/help/rules\""), "{}", page.body);
    assert_eq!(
        f.get(MAIN, "/helpx", false).await.status,
        StatusCode::NOT_FOUND,
        "/helpx falls to the root path and is neither a forum nor a system URL"
    );
    assert_eq!(
        f.get(MAIN, &format!("/help/thread-{meta}"), false)
            .await
            .status,
        StatusCode::OK
    );
    // The robots of the root path row merges the sub-path's rules and sitemap.
    f.indexing().await;
    let robots = f.get(MAIN, "/robots.txt", false).await.body;
    assert!(
        robots.contains(&format!("Sitemap: {TEST_SITE_URL}/sitemap.xml"))
            && robots.contains(&format!("Sitemap: {TEST_SITE_URL}/help/sitemap.xml")),
        "{robots}"
    );

    // Creating a forum must avoid the first segments of registered sub-paths.
    let input = |slug: &str| organization::ForumInput {
        fid: 0,
        parent_fid: 0,
        icon_aid: 0,
        name: "New forum".into(),
        slug: slug.into(),
        description: String::new(),
        guidance: String::new(),
        displayorder: 9,
        status: 1,
        visibility: "public".into(),
        version: 0,
        ..Default::default()
    };
    assert!(
        organization::save_forum(&f.f.db, f.actor(), &input("help"))
            .await
            .is_err()
    );
    // A newly created top-level forum joins only the main domain; after becoming a child forum it leaves every
    // domain; deleting a forum deletes its link rows first.
    let fresh = organization::save_forum(&f.f.db, f.actor(), &input("fresh"))
        .await
        .unwrap();
    let map = f.map().await;
    assert!(map.get(main_id).unwrap().shows(fresh));
    assert!(!map.get(help.domain_id).unwrap().shows(fresh));
    let version: i64 = db::query_scalar("SELECT version FROM gonof_forum_forum WHERE fid=?")
        .bind(fresh)
        .fetch_one(f.f.db.executor())
        .await
        .unwrap();
    organization::delete_forum(&f.f.db, f.actor(), fresh, version, "test", 86400)
        .await
        .unwrap();
    assert!(!f.map().await.get(main_id).unwrap().shows(fresh));

    // Changing the path of an existing domain must pass confirmation first: the first attempt does not save, a wrong
    // entry does not save, only the correct entry saves.
    let meta_fid = f.fid("meta").await;
    let fields = |url: &str| -> Vec<(String, String)> {
        vec![
            ("domain_id".into(), help.domain_id.to_string()),
            ("version".into(), help.version.to_string()),
            ("site_url".into(), url.into()),
            ("site_name".into(), "Help".into()),
            ("canonical".into(), "self".into()),
            ("enabled".into(), "1".into()),
            (format!("forum_{meta_fid}"), "1".into()),
        ]
    };
    let mut first = fields(&format!("{TEST_SITE_URL}/support"));
    let as_refs = |fields: &Vec<(String, String)>| -> Vec<(String, String)> { fields.clone() };
    let send = |fields: Vec<(String, String)>| {
        let f = &f;
        async move {
            let borrowed: Vec<(&str, &str)> = fields
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            f.post(MAIN, "/manage/domains", &borrowed).await
        }
    };
    let asked = send(as_refs(&first)).await;
    assert_eq!(asked.status, StatusCode::OK);
    assert!(asked.body.contains("data-domain-confirm"), "{}", asked.body);
    first.push(("confirmed_from".into(), "/help".into()));
    first.push(("confirmed_url".into(), help.site_url.clone()));
    first.push(("confirm".into(), "yes".into()));
    let wrong = send(as_refs(&first)).await.body;
    assert!(
        wrong.contains("Type confirm (in lowercase) in the confirmation box"),
        "{wrong}"
    );
    // After a wrong entry, the redrawn confirmation area still carries the old path, and entering it correctly saves.
    assert!(
        wrong.contains("name=\"confirmed_from\" value=\"/help\""),
        "{wrong}"
    );
    first.pop();
    first.push(("confirm".into(), "confirm".into()));
    assert!(send(first).await.status.is_redirection());
    assert_eq!(f.get(MAIN, "/support/", false).await.status, StatusCode::OK);
    assert_eq!(
        f.get(MAIN, "/help/", false).await.status,
        StatusCode::NOT_FOUND
    );

    // Changing the forum URL: the main domain row changes with it; it cannot be cleared while the switch is on; a new
    // URL already registered as another row is rejected.
    let moved = format!("{TEST_SITE_URL}/bbs");
    runtime::set_site_url(&f.f.db, &f.f.global, &moved)
        .await
        .unwrap();
    assert_eq!(f.map().await.get(main_id).unwrap().site_url, moved);
    assert!(
        runtime::set_site_url(&f.f.db, &f.f.global, "")
            .await
            .is_err()
    );
    assert!(
        runtime::set_site_url(&f.f.db, &f.f.global, &format!("{TEST_SITE_URL}/support"))
            .await
            .is_err()
    );
    // The main domain is under /bbs, so the host no longer has a root path: the root path is 404, /support as before.
    assert_eq!(f.get(MAIN, "/", false).await.status, StatusCode::NOT_FOUND);
    assert_eq!(f.get(MAIN, "/bbs/", false).await.status, StatusCode::OK);
    assert_eq!(f.get(MAIN, "/support/", false).await.status, StatusCode::OK);
    f.f.cleanup().await.unwrap();
}

#[tokio::test]
async fn account_mail_links_use_the_domain_they_were_requested_on() {
    let f = Fixture::new().await;
    f.enable().await;
    assert!(
        f.add(DEMO2_URL, "self", &["meta"])
            .await
            .status
            .is_redirection()
    );
    let map = Arc::new(f.map().await);
    let demo2 = map
        .rows()
        .iter()
        .find(|row| row.site_url == DEMO2_URL)
        .unwrap()
        .domain_id;
    // Originated on demo2: the queue records the domain id and delivery links use demo2; mail originated on an
    // unregistered Host uses the forum URL.
    let on = |current: Option<i64>| gono_forum_web::site::Site {
        prefix: "".into(),
        url: current.map(|_| DEMO2_URL.into()),
        derived: current.is_none(),
        domains: Some(Arc::new(gono_forum_web::site::Domains {
            map: map.clone(),
            forum_url: TEST_SITE_URL.into(),
            current,
        })),
        participation: Default::default(),
        language: Default::default(),
    };
    gono_forum_web::site::scope(on(Some(demo2)), f.identity.resend_verification(MEMBER))
        .await
        .unwrap();
    let payload: String = db::query_scalar(
        "SELECT payload FROM gonof_common_outbox WHERE event_type='email_verification' ORDER BY eventid DESC LIMIT 1",
    )
    .fetch_one(f.f.db.executor())
    .await
    .unwrap();
    assert!(
        payload.contains(&format!("\"domain_id\":{demo2}")),
        "{payload}"
    );
    let mailbox = f.f.root.join("domains-mail");
    let mailer = gono_forum_web::mail::Mailer::new(
        f.f.db.clone(),
        gono_forum_web::secrets::CommonConfig::default(),
        mailbox.clone(),
        f.f.global.clone(),
    );
    let read = || -> Vec<String> {
        std::fs::read_dir(&mailbox)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
                    .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(
        mailer
            .deliver_pending(identity::now())
            .await
            .unwrap()
            .delivered
            >= 1
    );
    assert!(
        read()
            .iter()
            .any(|text| text.contains(&format!("{DEMO2_URL}/verify-email?token="))),
        "{:?}",
        read()
    );
    let _ = std::fs::remove_dir_all(&mailbox);
    // Unregistered Host: no id is recorded, and links use the forum URL (not derived from the request's Host).
    db::query("UPDATE gonof_common_outbox SET status='sent' WHERE event_type='email_verification'")
        .execute(f.f.db.executor())
        .await
        .unwrap();
    gono_forum_web::site::scope(on(None), f.identity.resend_verification(MEMBER))
        .await
        .unwrap();
    let payload: String = db::query_scalar(
        "SELECT payload FROM gonof_common_outbox WHERE event_type='email_verification' ORDER BY eventid DESC LIMIT 1",
    )
    .fetch_one(f.f.db.executor())
    .await
    .unwrap();
    assert!(!payload.contains("domain_id"), "{payload}");
    assert!(
        mailer
            .deliver_pending(identity::now())
            .await
            .unwrap()
            .delivered
            >= 1
    );
    assert!(
        read()
            .iter()
            .any(|text| text.contains(&format!("{TEST_SITE_URL}/verify-email?token="))),
        "{:?}",
        read()
    );
    let _ = std::fs::remove_dir_all(&mailbox);
    f.f.cleanup().await.unwrap();
}

/// The appeal link asked for on another domain is queued with that domain, like the other account mail, so it is
/// written for that domain and links back to it instead of to the main site.
#[tokio::test]
async fn appeal_links_use_the_domain_they_were_requested_on() {
    let f = Fixture::new().await;
    f.enable().await;
    assert!(
        f.add(DEMO2_URL, "self", &["meta"])
            .await
            .status
            .is_redirection()
    );
    let map = Arc::new(f.map().await);
    let demo2 = map
        .rows()
        .iter()
        .find(|row| row.site_url == DEMO2_URL)
        .unwrap()
        .domain_id;
    let now = identity::now();
    let version: i64 =
        db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=?")
            .bind(MEMBER)
            .fetch_one(f.f.db.executor())
            .await
            .unwrap();
    gono_forum_web::moderation::issue_sanction(
        &f.f.db,
        f.actor(),
        &gono_forum_web::moderation::SanctionRequest {
            uid: MEMBER,
            kind: "warning".into(),
            hours: 0,
            reason: "Broke the test community rules".into(),
            expected_version: version,
            request_key: identity::random_token(),
        },
        now,
    )
    .await
    .unwrap();
    let site = gono_forum_web::site::Site {
        prefix: "".into(),
        url: Some(DEMO2_URL.into()),
        derived: false,
        domains: Some(Arc::new(gono_forum_web::site::Domains {
            map: map.clone(),
            forum_url: TEST_SITE_URL.into(),
            current: Some(demo2),
        })),
        participation: Default::default(),
        language: Default::default(),
    };
    gono_forum_web::site::scope(
        site,
        gono_forum_web::moderation::request_appeal_access(
            &f.f.db,
            &format!("domains-{MEMBER}@example.test"),
            now + 1,
        ),
    )
    .await
    .unwrap();
    let (eventid, payload): (i64, String) = {
        let row = db::query(
            "SELECT eventid,payload FROM gonof_common_outbox WHERE event_type='appeal_access'",
        )
        .fetch_one(f.f.db.executor())
        .await
        .unwrap();
        (
            row.try_get("eventid").unwrap(),
            db::text(&row, "payload").unwrap(),
        )
    };
    assert!(
        payload.contains(&format!("\"domain_id\":{demo2}")),
        "{payload}"
    );
    let mailbox = f.f.root.join("domains-appeal-mail");
    gono_forum_web::mail::Mailer::new(
        f.f.db.clone(),
        gono_forum_web::secrets::CommonConfig::default(),
        mailbox.clone(),
        f.f.global.clone(),
    )
    .deliver_pending(now + 2)
    .await
    .unwrap();
    let message = std::fs::read_to_string(mailbox.join(format!("{eventid}.eml"))).unwrap();
    assert!(
        message.contains(&format!("{DEMO2_URL}/appeals/access?token=")),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(&mailbox);
    f.f.cleanup().await.unwrap();
}
