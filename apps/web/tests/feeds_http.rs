//! Public RSS/Atom feeds and share metadata on thread pages.
#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    identity::{Actor, Identity},
    runtime,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: Actor = Actor {
    uid: 9701,
    credentials_version: 1,
    api_key: None,
};
const MEMBER: i64 = 9702;
/// "\u{957f}" is the Chinese for "long": one character but three bytes in UTF-8, so the summary length below is
/// checked to be counted in characters, not bytes.
const LONG: &str = "\u{957f}";

async fn fixture() -> (TestFixture, Router) {
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    content::seed_fixture(&f.db).await.unwrap();
    for (uid, adminid) in [(ADMIN.uid, 1), (MEMBER, 0)] {
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
            .bind(uid).bind(format!("feeds-{uid}")).bind(format!("feeds-{uid}@example.test")).bind(adminid)
            .execute(f.db.executor()).await.unwrap();
    }
    let app = gono_forum_web::router::build_with_global(
        f.config.clone(),
        &f.root,
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    );
    (f, app)
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
            .map(|v| v.to_str().unwrap())
            .unwrap_or("")
    }
}

async fn get(app: &Router, path: &str, extra: &[(header::HeaderName, &str)]) -> Reply {
    let mut request = Request::builder().uri(path);
    for (name, value) in extra {
        request = request.header(name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// Change forums, threads or posts with raw SQL, then sync the thread copies and invalidate caches as the admin
/// area and moderation entry points do (the copies are maintained only by code).
async fn exec(f: &TestFixture, sql: &str) {
    db::query(sql).execute(f.db.executor()).await.unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
}

/// A minimal DOM: element name, attributes, direct text and child elements. A parse failure (unmatched tags,
/// invalid entities) fails the assertion.
#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> &Node {
        self.children
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("missing <{name}>"))
    }
    fn all(&self, name: &str) -> Vec<&Node> {
        self.children.iter().filter(|c| c.name == name).collect()
    }
    fn attr(&self, name: &str) -> &str {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("<{}> is missing the attribute {name}", self.name))
    }
}

fn parse(xml: &str) -> Node {
    use quick_xml::{
        XmlVersion,
        escape::resolve_predefined_entity,
        events::{BytesStart, Event},
    };
    fn element(e: &BytesStart) -> Node {
        Node {
            name: e.name().as_ref().to_owned(),
            attrs: e
                .attributes()
                .map(|a| {
                    let a = a.unwrap();
                    (
                        a.key.as_ref().to_owned(),
                        a.normalized_value(XmlVersion::Implicit1_0)
                            .unwrap()
                            .into_owned(),
                    )
                })
                .collect(),
            ..Node::default()
        }
    }
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut stack = vec![Node::default()];
    loop {
        match reader
            .read_event()
            .expect("the feed must be well-formed XML")
        {
            Event::Start(e) => stack.push(element(&e)),
            Event::Empty(e) => {
                let node = element(&e);
                stack.last_mut().unwrap().children.push(node);
            }
            Event::End(_) => {
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(t) => stack.last_mut().unwrap().text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => {
                let text = if r.is_char_ref() {
                    r.resolve_char_ref().unwrap().unwrap().to_string()
                } else {
                    resolve_predefined_entity(&r)
                        .unwrap_or_else(|| panic!("undefined entity &{};", &*r))
                        .to_owned()
                };
                stack.last_mut().unwrap().text.push_str(&text);
            }
            Event::CData(c) => stack.last_mut().unwrap().text.push_str(&c.xml10_content()),
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(stack.len(), 1, "not every element is closed");
    let mut root = stack.pop().unwrap();
    assert_eq!(
        root.children.len(),
        1,
        "there must be exactly one root element"
    );
    root.children.pop().unwrap()
}

/// Thread ids of RSS items, in order of appearance.
fn rss_tids(xml: &str) -> Vec<i64> {
    let rss = parse(xml);
    assert_eq!(rss.name, "rss");
    rss.child("channel")
        .all("item")
        .into_iter()
        .map(|item| tid_of(&item.child("link").text))
        .collect()
}

/// Thread ids of Atom entries, in order of appearance.
fn atom_tids(xml: &str) -> Vec<i64> {
    let feed = parse(xml);
    assert_eq!(feed.name, "feed");
    feed.all("entry")
        .into_iter()
        .map(|entry| tid_of(entry.child("link").attr("href")))
        .collect()
}

fn tid_of(link: &str) -> i64 {
    link.rsplit_once("/thread-")
        .and_then(|(_, tid)| tid.parse().ok())
        .unwrap_or_else(|| panic!("the item link is not a thread address: {link}"))
}

/// The contents of `<script type="application/ld+json">`; None when absent.
fn json_ld(html: &str) -> Option<serde_json::Value> {
    let start = html.find("<script type=\"application/ld+json\">")?;
    let rest = &html[start + "<script type=\"application/ld+json\">".len()..];
    let end = rest
        .find("</script>")
        .expect("the structured data is not closed");
    Some(serde_json::from_str(&rest[..end]).expect("the structured data must be valid JSON"))
}

fn meta_content<'a>(html: &'a str, attr: &str) -> &'a str {
    let marker = format!("{attr} content=\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {attr}"))
        + marker.len();
    &html[start..start + html[start..].find('"').unwrap()]
}

#[tokio::test]
async fn feeds_list_public_threads_by_publication_time() {
    let (f, app) = fixture().await;

    let rss = get(&app, "/feed.xml", &[]).await;
    assert_eq!(rss.status, StatusCode::OK);
    assert_eq!(
        rss.header(header::CONTENT_TYPE),
        "application/rss+xml; charset=utf-8"
    );
    assert_eq!(rss.header(header::CACHE_CONTROL), "public, no-cache");
    assert!(
        rss.headers.get(header::SET_COOKIE).is_none(),
        "feeds read no session and set no CSRF cookie"
    );
    // All 24 fixture threads are public. By publish time descending: the two pinned threads (101, 102) were published
    // earliest and come last; being pinned does not move them forward.
    let expected: Vec<i64> = (101..=124).rev().collect();
    assert_eq!(rss_tids(&rss.body), expected);
    let channel = parse(&rss.body);
    let channel = channel.child("channel");
    assert_eq!(channel.child("title").text, "Sample Community");
    // The language of the site the feed was requested from (an English site here).
    assert_eq!(channel.child("language").text, "en");
    assert!(channel.child("link").text.ends_with('/'));
    let item = channel.all("item")[0];
    assert_eq!(item.child("guid").attr("isPermaLink"), "true");
    assert_eq!(
        item.child("pubDate").text,
        "Tue, 08 Sep 2026 00:23:00 +0000"
    );
    // The fixture importer stores author names in lower case: 124 is by "Camellia".
    assert_eq!(item.child("dc:creator").text, "camellia");
    assert!(!item.child("description").text.is_empty());

    let atom = get(&app, "/atom.xml", &[]).await;
    assert_eq!(atom.status, StatusCode::OK);
    assert_eq!(
        atom.header(header::CONTENT_TYPE),
        "application/atom+xml; charset=utf-8"
    );
    assert_eq!(atom_tids(&atom.body), expected);
    let feed = parse(&atom.body);
    assert_eq!(feed.attr("xml:lang"), "en");
    assert!(feed.child("id").text.ends_with("/atom.xml"));
    let entry = feed.all("entry")[0];
    assert_eq!(entry.child("published").text, "2026-09-08T00:23:00Z");
    assert_eq!(entry.child("author").child("name").text, "camellia");

    // A forum feed only includes its own forum.
    let daily = [124, 120, 115, 111, 106];
    assert_eq!(
        rss_tids(&get(&app, "/daily/feed.xml", &[]).await.body),
        daily
    );
    let daily_atom = get(&app, "/daily/atom.xml", &[]).await;
    assert_eq!(atom_tids(&daily_atom.body), daily);
    assert!(
        parse(&daily_atom.body)
            .child("title")
            .text
            .ends_with(" - Sample Community")
    );
    for missing in ["/nowhere/feed.xml", "/nowhere/atom.xml"] {
        assert_eq!(get(&app, missing, &[]).await.status, StatusCode::NOT_FOUND);
    }

    // When the content is unchanged, revalidating with the ETag gets a 304.
    let etag = rss.header(header::ETAG).to_owned();
    assert!(etag.starts_with('"'));
    let again = get(&app, "/feed.xml", &[(header::IF_NONE_MATCH, &etag)]).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert!(again.body.is_empty());

    // With indexing off (the default) it declares noindex consistently with robots.txt; turned on, that goes away.
    assert_eq!(
        rss.header("x-robots-tag".parse().unwrap()),
        "noindex, nofollow"
    );
    runtime::update(&f.db, &f.global, ADMIN, "site.settings.save", |s| {
        s.seo.indexing = true;
    })
    .await
    .unwrap();
    assert!(
        get(&app, "/feed.xml", &[])
            .await
            .headers
            .get("x-robots-tag")
            .is_none()
    );

    // Autodiscovery: the home page gives the two site-wide feeds, a forum page the forum's two, the search page none.
    let home = get(&app, "/", &[]).await.body;
    assert!(home.contains("<link rel=\"alternate\" type=\"application/rss+xml\" title=\"Sample Community RSS\" href=\"http://127.0.0.1:58833/feed.xml\">"), "{home}");
    assert!(home.contains("<link rel=\"alternate\" type=\"application/atom+xml\" title=\"Sample Community Atom\" href=\"http://127.0.0.1:58833/atom.xml\">"));
    let forum = get(&app, "/daily", &[]).await.body;
    assert!(forum.contains("type=\"application/rss+xml\" title=\"Daily - Sample Community RSS\" href=\"http://127.0.0.1:58833/daily/feed.xml\""), "{forum}");
    assert!(forum.contains("href=\"http://127.0.0.1:58833/daily/atom.xml\""));
    assert!(
        !get(&app, "/search?q=RSS", &[])
            .await
            .body
            .contains("rel=\"alternate\"")
    );

    // The feeds page lists the site-wide and every public forum's feeds, with an entry in the footer.
    let page = get(&app, "/feeds", &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    for href in ["/feed.xml", "/atom.xml", "/daily/feed.xml", "/ai/atom.xml"] {
        assert!(
            page.body
                .contains(&format!("href=\"http://127.0.0.1:58833{href}\"")),
            "{href}"
        );
    }
    assert!(home.contains("<a href=\"/feeds\">Feeds</a>"));

    // The Redis-disabled deployment mode falls back to the source as usual.
    let mut config = f.config.clone();
    config.redis.enabled = false;
    let direct = gono_forum_web::router::build(config, &f.root, f.db.clone());
    assert_eq!(
        rss_tids(&get(&direct, "/feed.xml", &[]).await.body),
        expected
    );
    f.cleanup().await.unwrap();
}

/// A thread kept from a deleted account carries the stored marker as its author copy; feeds show it as "Deleted user"
/// in the feed's language, like the pages.
#[tokio::test]
async fn feeds_name_deleted_authors_in_their_language() {
    let (f, app) = fixture().await;
    db::query("UPDATE gonof_forum_thread SET author=? WHERE tid=124")
        .bind(content::copies::DELETED_AUTHOR)
        .execute(f.db.executor())
        .await
        .unwrap();
    let deleted = gono_forum_web::i18n::Lang::EN.common_deleted_user();
    let rss = get(&app, "/feed.xml", &[]).await;
    let item = parse(&rss.body);
    assert_eq!(
        item.child("channel").all("item")[0]
            .child("dc:creator")
            .text,
        deleted
    );
    let atom = get(&app, "/atom.xml", &[]).await;
    assert_eq!(
        parse(&atom.body).all("entry")[0]
            .child("author")
            .child("name")
            .text,
        deleted
    );
    f.cleanup().await.unwrap();
}

/// Threads that are hidden, withdrawn, erased, members-only, unpublished, read-restricted, in private forums, in
/// members-only forums, in child forums inheriting the members-only scope, or in disabled forums never enter feeds;
/// feeds of invisible forums are 404.
#[tokio::test]
async fn feeds_leave_out_everything_anonymous_cannot_read() {
    let (f, app) = fixture().await;
    // Fill the cache first: every later change advances the content version, and the next read must see the new
    // result.
    assert_eq!(rss_tids(&get(&app, "/feed.xml", &[]).await.body).len(), 24);
    for slug in ["hosting", "ai", "releases", "meta"] {
        assert_eq!(
            get(&app, &format!("/{slug}/atom.xml"), &[]).await.status,
            StatusCode::OK
        );
    }

    // The daily and showcase forums stay public; threads are tightened one by one.
    for sql in [
        // First post hidden by moderation.
        "UPDATE gonof_forum_post SET invisible=-1 WHERE tid=124 AND first=1",
        // Thread withdrawn and first post withdrawn.
        "UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid=120",
        "UPDATE gonof_forum_post SET deleted_at=1 WHERE tid=115 AND first=1",
        // Body erased (necessarily together with withdrawal).
        "UPDATE gonof_forum_post SET message='',deleted_at=1,erased_at=1 WHERE tid=106 AND first=1",
        // The author made it members-only.
        "UPDATE gonof_forum_thread SET visibility='members' WHERE tid=121",
        // Unpublished, read-restricted, thread hidden by moderation.
        "UPDATE gonof_forum_thread SET publication_state='draft' WHERE tid=116",
        "UPDATE gonof_forum_thread SET readperm=1 WHERE tid=112",
        // Negative pin levels reserved for moderation (-1 and -2 are derived from first-post hiding and reserved
        // states; -3 is kept as is).
        "UPDATE gonof_forum_thread SET displayorder=-3 WHERE tid=108",
        // Whole forums: private, members-only, disabled.
        "UPDATE gonof_forum_forum SET visibility='private' WHERE slug='hosting'",
        "UPDATE gonof_forum_forum SET visibility='members' WHERE slug='ai'",
        "UPDATE gonof_forum_forum SET status=0 WHERE slug='meta'",
    ] {
        exec(&f, sql).await;
    }
    // A child forum that is public itself but whose parent is members-only counts by the inherited scope.
    let parent = db::query("INSERT INTO gonof_forum_forum(name,slug,visibility,description,guidance) VALUES('Members parent','feeds-members-parent','members','','')")
        .execute(f.db.executor()).await.unwrap().last_insert_id();
    db::query("UPDATE gonof_forum_forum SET parent_fid=? WHERE slug='releases'")
        .bind(i64::try_from(parent).unwrap())
        .execute(f.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    // Feeds judge forum readability from the process forum cache: after changing the database directly, clear this
    // process's layer (saving in the admin area clears it itself).
    f.global.clear_local();

    assert_eq!(
        rss_tids(&get(&app, "/feed.xml", &[]).await.body),
        [111, 103]
    );
    assert_eq!(
        atom_tids(&get(&app, "/atom.xml", &[]).await.body),
        [111, 103]
    );
    assert_eq!(
        rss_tids(&get(&app, "/daily/feed.xml", &[]).await.body),
        [111]
    );
    assert_eq!(
        atom_tids(&get(&app, "/showcase/atom.xml", &[]).await.body),
        [103]
    );
    for slug in ["hosting", "ai", "releases", "meta", "feeds-members-parent"] {
        for file in ["feed.xml", "atom.xml"] {
            let reply = get(&app, &format!("/{slug}/{file}"), &[]).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{slug}/{file}");
        }
    }

    // Signed-in members viewing a members-only forum: the page is normal, but no autodiscovery links (the feed itself
    // is 404 for anonymous visitors).
    let identity = Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let member = http_auth::login(&identity, &f.db, MEMBER).await;
    let cookie = format!("gonof_session={}", member.session_token);
    let ai = get(&app, "/ai", &[(header::COOKIE, &cookie)]).await;
    assert_eq!(ai.status, StatusCode::OK);
    assert!(!ai.body.contains("rel=\"alternate\""));
    let feeds = get(&app, "/feeds", &[(header::COOKIE, &cookie)]).await.body;
    assert!(!feeds.contains("/ai/feed.xml") && feeds.contains("/daily/feed.xml"));
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn feed_text_is_escaped_and_summaries_hide_markup() {
    let (f, app) = fixture().await;
    // "\u{20bb7}" is a Chinese character outside the Basic Multilingual Plane (four bytes in UTF-8), like the emoji.
    let title = "A<B & C \"quote\" \u{20bb7}😀 </title>";
    db::query("UPDATE gonof_forum_thread SET subject=? WHERE tid=124")
        .bind(title)
        .execute(f.db.executor())
        .await
        .unwrap();
    let body = format!(
        "Opening words <spoiler>Secret ending</spoiler>\n\n![picture](attachment:999) [quote](post:1) [@someone](member:{MEMBER}) code `a<b&c` {}",
        LONG.repeat(400)
    );
    db::query("UPDATE gonof_forum_post SET message=? WHERE tid=124 AND first=1")
        .bind(&body)
        .execute(f.db.executor())
        .await
        .unwrap();

    let rss = get(&app, "/feed.xml", &[]).await.body;
    assert!(
        rss.contains("<title>A&lt;B &amp; C &quot;quote&quot; \u{20bb7}😀 &lt;/title&gt;</title>")
    );
    let channel = parse(&rss);
    let item = channel.child("channel").all("item")[0];
    assert_eq!(item.child("title").text, title);
    let summary = &item.child("description").text;
    let atom = get(&app, "/atom.xml", &[]).await.body;
    let feed = parse(&atom);
    let entry = feed.all("entry")[0];
    assert_eq!(entry.child("title").text, title);
    assert_eq!(&entry.child("summary").text, summary);
    assert!(summary.starts_with("Opening words"), "{summary}");
    assert!(
        summary.contains("a<b&c"),
        "code stays as plain text: {summary}"
    );
    assert!(
        summary.contains("feeds-9702"),
        "mentions show the current username: {summary}"
    );
    for hidden in [
        "Secret ending",
        "spoiler",
        "attachment:",
        "post:",
        "member:",
        "picture",
        "<",
    ] {
        if hidden == "<" {
            assert_eq!(summary.matches('<').count(), 1, "{summary}");
        } else {
            assert!(!summary.contains(hidden), "{hidden}: {summary}");
        }
    }
    assert_eq!(summary.chars().count(), 160);
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn topic_pages_share_article_metadata_and_structured_data() {
    let (f, app) = fixture().await;
    let replies: i64 = db::query_scalar("SELECT replies FROM gonof_forum_thread WHERE tid=103")
        .fetch_one(f.db.executor())
        .await
        .unwrap();

    let html = get(&app, "/thread-103", &[]).await.body;
    assert_eq!(meta_content(&html, "property=\"og:type\""), "article");
    assert_eq!(meta_content(&html, "name=\"twitter:card\""), "summary");
    assert_eq!(
        meta_content(&html, "name=\"twitter:title\""),
        meta_content(&html, "property=\"og:title\"")
    );
    let description = meta_content(&html, "property=\"og:description\"");
    assert_eq!(
        meta_content(&html, "name=\"twitter:description\""),
        description
    );
    assert!(!description.is_empty());
    let data = json_ld(&html).expect("a public thread outputs structured data");
    assert_eq!(data["@context"], "https://schema.org");
    assert_eq!(data["@type"], "DiscussionForumPosting");
    assert_eq!(
        data["headline"],
        "I built a lightweight RSS reader: notes on how development went"
    );
    assert_eq!(data["url"], "http://127.0.0.1:58833/thread-103");
    assert_eq!(data["datePublished"], "2026-09-08T00:02:00Z");
    assert_eq!(data["author"]["name"], "southwind");
    assert_eq!(
        data["interactionStatistic"]["userInteractionCount"],
        replies
    );
    // Structured data adds `text`, the same excerpt already public in the page
    // description; `isPartOf` points to the forum; a first post that was never edited has no `dateModified`.
    assert_eq!(data["text"], description);
    assert!(data.get("articleBody").is_none());
    assert_eq!(data["isPartOf"]["@type"], "WebPage");
    let slug: String = db::query_scalar(
        "SELECT f.slug FROM gonof_forum_thread t JOIN gonof_forum_forum f ON f.fid=t.fid WHERE t.tid=103",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(!data["isPartOf"]["name"].as_str().unwrap().is_empty());
    assert_eq!(
        data["isPartOf"]["url"],
        format!("http://127.0.0.1:58833/{slug}")
    );

    // Other pages stay website and have no structured data.
    let home = get(&app, "/", &[]).await.body;
    assert_eq!(meta_content(&home, "property=\"og:type\""), "website");
    assert!(json_ld(&home).is_none());

    // A </script> in the title must not end the data block early.
    let hostile = "</script><script>alert(1)</script> & <!--";
    db::query("UPDATE gonof_forum_thread SET subject=? WHERE tid=103")
        .bind(hostile)
        .execute(f.db.executor())
        .await
        .unwrap();
    let html = get(&app, "/thread-103", &[]).await.body;
    assert_eq!(json_ld(&html).unwrap()["headline"], hostile);
    assert!(!html.contains("<script>alert(1)"));
    assert_eq!(
        html.matches("</script>").count(),
        html.matches("<script").count()
    );

    // A withdrawn thread is only a tombstone and outputs no structured data.
    exec(
        &f,
        "UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid=103",
    )
    .await;
    assert!(json_ld(&get(&app, "/thread-103", &[]).await.body).is_none());

    // Members-only threads and threads in members-only forums: 404 for anonymous visitors; signed-in members see the
    // share description replaced by the site description, with no excerpt or structured data.
    exec(
        &f,
        "UPDATE gonof_forum_thread SET visibility='members' WHERE tid=121",
    )
    .await;
    exec(
        &f,
        "UPDATE gonof_forum_forum SET visibility='members' WHERE slug='ai'",
    )
    .await;
    // The thread page's readability check uses the process forum cache; clear it after changing the
    // database directly.
    f.global.clear_local();
    let identity = Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let member = http_auth::login(&identity, &f.db, MEMBER).await;
    let cookie = format!("gonof_session={}", member.session_token);
    let site_description = runtime::read(&f.db).await.unwrap().site.description;
    for tid in [121, 104] {
        let path = format!("/thread-{tid}");
        assert_eq!(get(&app, &path, &[]).await.status, StatusCode::NOT_FOUND);
        let html = get(&app, &path, &[(header::COOKIE, &cookie)]).await.body;
        assert_eq!(meta_content(&html, "property=\"og:type\""), "article");
        assert_eq!(
            meta_content(&html, "property=\"og:description\""),
            site_description,
            "{path}"
        );
        assert_eq!(
            meta_content(&html, "name=\"twitter:description\""),
            site_description
        );
        assert!(json_ld(&html).is_none(), "{path}");
    }
    // The same member viewing a public thread still gets the excerpt and structured data.
    let html = get(&app, "/thread-122", &[(header::COOKIE, &cookie)])
        .await
        .body;
    assert!(json_ld(&html).is_none(), "the ai forum is members-only now");
    let html = get(&app, "/thread-111", &[(header::COOKIE, &cookie)])
        .await
        .body;
    assert!(json_ld(&html).is_some());
    assert_ne!(
        meta_content(&html, "property=\"og:description\""),
        site_description
    );

    // With a share image set, the large image card is used, with the image at the same URL as og:image. The brand
    // image must point to a real attachment, so foreign key checks are briefly disabled on the same connection here,
    // only so the page frame can read this row.
    let mut connection = f.db.pool.acquire().await.unwrap();
    for query in [
        gono_forum_web::db::query("SET FOREIGN_KEY_CHECKS=0").sqlite("PRAGMA foreign_keys=OFF"),
        gono_forum_web::db::query(
            "INSERT INTO gonof_common_site_asset(slot,aid,version,dateline) VALUES('share',424242,1,1)",
        ),
        gono_forum_web::db::query("SET FOREIGN_KEY_CHECKS=1").sqlite("PRAGMA foreign_keys=ON"),
    ] {
        query.execute(&mut *connection).await.unwrap();
    }
    drop(connection);
    // Brand images live in the process-wide global cache: after changing the database directly, clear this process's
    // cache (the save function clears it itself).
    f.global.clear_local();
    let html = get(&app, "/thread-111", &[]).await.body;
    assert_eq!(
        meta_content(&html, "name=\"twitter:card\""),
        "summary_large_image"
    );
    assert_eq!(
        meta_content(&html, "name=\"twitter:image\""),
        meta_content(&html, "property=\"og:image\"")
    );
    assert!(meta_content(&html, "property=\"og:image\"").ends_with("/brand/share/424242/source"));
    f.cleanup().await.unwrap();
}
