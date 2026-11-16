//! Forum navigation: the page header's default navigation lists only top-level forums, and child forum pages
//! and threads in child forums highlight the parent forum; the mobile drawer and mobile forum bar follow the
//! navigation document; breadcrumbs, child forum tabs (without child forums the member blocked) and `BreadcrumbList`;
//! the page header's "All forums" entry; the posting guide and the tiered drop-down on the posting page; readable
//! forums go into the process cache (docs/settings-and-caching.md, "The process cache"), which public and personal
//! feeds read too.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::forum_filter::{self, FilterMode},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Forums-batch-three-password-2026!";
const ADMIN: i64 = 9300;
const READER: i64 = 9301;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn member(db: &Database, uid: i64, role: i64) {
    let salt = SaltString::encode_b64(b"forums-batch-three-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, 1, ?, ?)")
        .bind(uid)
        .bind(format!("crumb-{uid}"))
        .bind(format!("crumb-{uid}@example.test"))
        .bind(hash)
        .bind(role)
        .bind(identity::now() - 30 * 86_400)
        .execute(db.executor())
        .await
        .unwrap();
}

async fn forum(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
    )
    .await
}

async fn version(db: &Database, fid: i64) -> i64 {
    scalar(
        db,
        &format!("SELECT version FROM gonof_forum_forum WHERE fid={fid}"),
    )
    .await
}

/// Fixture: of the six forums, ai and hosting are moved under showcase ("Showcase"), leaving "Daily", "Showcase", "Releases" and
/// "Meta" at the top level.
async fn family(f: &TestFixture) -> (i64, i64, i64) {
    content::seed_forums(&f.db).await.unwrap();
    set_setting(&f.db, "settings.posting.daily_threads", "1000")
        .await
        .unwrap();
    let showcase = forum(&f.db, "showcase").await;
    let ai = forum(&f.db, "ai").await;
    let hosting = forum(&f.db, "hosting").await;
    execute(
        &f.db,
        &format!(
            "UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid IN ({ai},{hosting})"
        ),
    )
    .await;
    member(&f.db, ADMIN, 1).await;
    member(&f.db, READER, 0).await;
    (showcase, ai, hosting)
}

struct Http {
    f: TestFixture,
    app: Router,
    identity: Identity,
}

impl Http {
    fn new(f: TestFixture) -> Self {
        let identity = Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, identity }
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        self.identity
            .login(&format!("crumb-{uid}"), PASSWORD)
            .await
            .unwrap()
    }

    async fn set_filter(&self, reader: &SessionGrant, fid: i64, mode: FilterMode) {
        forum_filter::set(
            &self.f.db,
            &self.f.cache,
            reader.member.actor(),
            fid,
            Some(mode),
            &identity::random_token(),
        )
        .await
        .unwrap();
    }

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

    async fn form(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &grant.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("HX-Request", "false")
            .header(header::ORIGIN, TEST_SITE_URL)
            .header(header::HOST, host_of(TEST_SITE_URL))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            )
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }
}

/// The part of the page from `start` up to the next `end`; an empty string when `start` is absent.
fn section<'a>(page: &'a str, start: &str, end: &str) -> &'a str {
    match page.find(start) {
        Some(at) => &page[at..at + page[at..].find(end).unwrap()],
        None => "",
    }
}

fn header_nav(page: &str) -> &str {
    section(page, "<nav class=\"masthead__boards\"", "</nav>")
}

fn strip(page: &str) -> &str {
    section(page, "<nav class=\"board-strip\"", "</nav>")
}

fn drawer(page: &str) -> &str {
    section(page, "<nav class=\"drawer\"", "</nav>")
}

fn tabs(page: &str) -> &str {
    section(page, "aria-label=\"Subforums\"", "</nav>")
}

fn breadcrumb(page: &str) -> &str {
    section(page, "<nav class=\"crumbs\"", "</nav>")
}

/// All `<script type="application/ld+json">` data blocks in the page.
fn json_ld(page: &str) -> Vec<serde_json::Value> {
    const OPEN: &str = "<script type=\"application/ld+json\">";
    page.match_indices(OPEN)
        .map(|(at, _)| {
            let rest = &page[at + OPEN.len()..];
            serde_json::from_str(&rest[..rest.find("</script>").unwrap()]).unwrap()
        })
        .collect()
}

fn breadcrumb_list(page: &str) -> Option<serde_json::Value> {
    json_ld(page)
        .into_iter()
        .find(|block| block["@type"] == "BreadcrumbList")
}

/// The `(name, URL)` items of `BreadcrumbList`; the last item (this page) has no URL.
fn crumbs(list: &serde_json::Value) -> Vec<(String, Option<String>)> {
    list["itemListElement"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            assert_eq!(item["position"], index + 1);
            (
                item["name"].as_str().unwrap().to_owned(),
                item["item"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn link(href: &str, current: bool) -> String {
    if current {
        format!("href=\"{href}\" aria-current=\"page\"")
    } else {
        format!("href=\"{href}\"")
    }
}

/// The page header, mobile drawer and mobile forum bar share one source and list only top-level forums; child forum
/// pages and threads in child forums highlight the parent forum, while a child forum named in the navigation document
/// takes precedence itself; breadcrumbs, child forum tabs (without blocked child forums) and `BreadcrumbList`; the
/// "All forums" entry.
#[tokio::test]
async fn header_navigation_follows_the_family_and_breadcrumbs_match_structured_data() {
    let f = TestFixture::migrated().await.unwrap();
    let (showcase, ai, hosting) = family(&f).await;
    let site = runtime::read(&f.db).await.unwrap().site.name;
    let tid = ContentWriter::new(f.db.clone(), WriteConfig::default())
        .with_site_url(TEST_SITE_URL)
        .publish(
            READER,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid: ai,
                title: "Thread in a child forum".into(),
                body: "Breadcrumbs and highlighting".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .tid;
    let h = Http::new(f);

    // The default navigation lists only top-level forums, the same in the page header, mobile drawer and
    // mobile forum bar; with four top-level forums "All forums" always stays in the page header.
    let (status, home) = h.page("/", None).await;
    assert_eq!(status, StatusCode::OK);
    for part in [header_nav(&home), strip(&home), drawer(&home)] {
        for slug in ["daily", "showcase", "releases", "meta"] {
            assert!(
                part.contains(&format!("href=\"/{slug}\"")),
                "{slug}: {part}"
            );
        }
        for slug in ["ai", "hosting"] {
            assert!(
                !part.contains(&format!("href=\"/{slug}\"")),
                "{slug}: {part}"
            );
        }
    }
    assert!(
        header_nav(&home)
            .contains("<a class=\"masthead__board\" href=\"/forums\" data-directory-link>")
    );
    assert!(!header_nav(&home).contains("forum-more"));
    assert!(drawer(&home).contains("href=\"/forums\" data-directory-link>All forums</a>"));
    assert!(
        !strip(&home).contains("/forums"),
        "the forum bar follows only the navigation document"
    );
    assert!(strip(&home).contains(&link("/", true)));
    assert!(breadcrumb(&home).is_empty() && json_ld(&home).is_empty());
    let (_, directory) = h.page("/forums", None).await;
    assert!(
        header_nav(&directory)
            .contains("href=\"/forums\" data-directory-link aria-current=\"page\"")
    );

    // Child forum page: highlights the parent forum; breadcrumb "Home › Showcase"; the first tab "All" points to the parent
    // forum, and the current item is this forum.
    let (status, child) = h.page("/ai", None).await;
    assert_eq!(status, StatusCode::OK);
    for part in [header_nav(&child), strip(&child)] {
        assert!(part.contains(&link("/showcase", true)), "{part}");
        assert!(!part.contains("href=\"/daily\" aria-current"));
    }
    assert!(!strip(&child).contains(&link("/", true)));
    let crumb = breadcrumb(&child);
    assert!(crumb.contains(
        "<a href=\"/\">Home</a><span class=\"crumbs__sep\" aria-hidden=\"true\"></span><a href=\"/showcase\">Showcase</a>"
    ));
    assert!(
        !crumb.contains("/ai"),
        "the breadcrumb leaves out this page"
    );
    let row = tabs(&child);
    assert!(
        row.contains(&format!("{}>All</a>", link("/showcase", false))),
        "{row}"
    );
    assert!(row.contains(&format!("{}>AI</a>", link("/ai", true))));
    assert!(row.contains(&format!("{}>Hosting</a>", link("/hosting", false))));
    assert!(row.find(">All</a>").unwrap() < row.find(">AI</a>").unwrap());
    // BreadcrumbList: the page breadcrumb items plus this page (without a URL); the home item uses the site name.
    assert_eq!(
        crumbs(&breadcrumb_list(&child).unwrap()),
        [
            (site.clone(), Some(format!("{TEST_SITE_URL}/"))),
            (
                "Showcase".to_owned(),
                Some(format!("{TEST_SITE_URL}/showcase"))
            ),
            ("AI".to_owned(), None),
        ]
    );

    // Parent forum page: no breadcrumbs; the "All" tab is the current item; structured data has only home and this
    // forum.
    let (_, parent) = h.page("/showcase", None).await;
    assert!(breadcrumb(&parent).is_empty());
    assert!(header_nav(&parent).contains(&link("/showcase", true)));
    assert!(tabs(&parent).contains(&format!("{}>All</a>", link("/showcase", true))));
    assert_eq!(
        crumbs(&breadcrumb_list(&parent).unwrap()),
        [
            (site.clone(), Some(format!("{TEST_SITE_URL}/"))),
            ("Showcase".to_owned(), None),
        ]
    );
    // A top-level forum without child forums has no tabs, but still has structured data.
    let (_, daily) = h.page("/daily", None).await;
    assert!(tabs(&daily).is_empty() && breadcrumb(&daily).is_empty());
    assert!(header_nav(&daily).contains(&link("/daily", true)));
    assert_eq!(crumbs(&breadcrumb_list(&daily).unwrap()).len(), 2);

    // A thread in a child forum: the breadcrumb adds the parent; the page header highlights the parent forum;
    // structured data has the thread first, then the breadcrumb, whose last item is the thread title.
    let (status, topic) = h.page(&format!("/thread-{tid}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(breadcrumb(&topic).contains("<a href=\"/\">Home</a><span class=\"crumbs__sep\" aria-hidden=\"true\"></span><a href=\"/showcase\">Showcase</a><span class=\"crumbs__sep\" aria-hidden=\"true\"></span><a href=\"/ai\">AI</a>"));
    assert!(header_nav(&topic).contains(&link("/showcase", true)));
    let blocks = json_ld(&topic);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["@type"], "DiscussionForumPosting");
    assert_eq!(
        crumbs(&blocks[1]),
        [
            (site.clone(), Some(format!("{TEST_SITE_URL}/"))),
            (
                "Showcase".to_owned(),
                Some(format!("{TEST_SITE_URL}/showcase"))
            ),
            ("AI".to_owned(), Some(format!("{TEST_SITE_URL}/ai"))),
            ("Thread in a child forum".to_owned(), None),
        ]
    );

    // Child forums the member blocked do not appear in the tabs (hidden ones are still listed); the page header
    // navigation is not affected by the member's own filters.
    let reader = h.login(READER).await;
    h.set_filter(&reader, ai, FilterMode::Block).await;
    h.set_filter(&reader, hosting, FilterMode::Hide).await;
    let (_, filtered) = h.page("/showcase", Some(&reader)).await;
    assert!(!tabs(&filtered).contains("/ai"));
    assert!(tabs(&filtered).contains(&link("/hosting", false)));
    let (_, sibling) = h.page("/hosting", Some(&reader)).await;
    assert!(tabs(&sibling).contains(&format!("{}>Hosting</a>", link("/hosting", true))));
    assert!(!tabs(&sibling).contains("/ai"));
    // When only blocked child forums remain, the parent forum page has no tabs.
    h.set_filter(&reader, hosting, FilterMode::Block).await;
    let (_, lonely) = h.page("/showcase", Some(&reader)).await;
    assert!(tabs(&lonely).is_empty());
    assert!(header_nav(&lonely).contains(&link("/showcase", true)));

    // When the navigation document names a child forum, that forum itself matches and the parent forum is no longer
    // highlighted at the same time; the forum bar follows the navigation document (including on-site pages).
    let admin = h.login(ADMIN).await;
    let navigation = format!("Showcase | forum:{showcase}\nAI | forum:{ai}\nRules | /rules");
    let (status, _) = h
        .form(
            "/manage/navigation",
            &admin,
            &[("title", ""), ("body", &navigation)],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, named) = h.page("/ai", None).await;
    for part in [header_nav(&named), strip(&named)] {
        assert!(part.contains(&link("/ai", true)), "{part}");
        assert!(part.contains(&link("/showcase", false)));
        assert!(part.contains("href=\"/rules\""));
        assert!(!part.contains("href=\"/daily\""));
    }
    let (_, sibling) = h.page("/hosting", None).await;
    assert!(header_nav(&sibling).contains(&link("/showcase", true)));
    assert!(header_nav(&sibling).contains(&link("/ai", false)));

    // The parent forum is made members-only (saved through the admin area, which clears this process's forum cache
    // after commit): anonymous visitors cannot open the child forum; signed-in members can view it but get no
    // `BreadcrumbList`, and the thread page has only the breadcrumb, without structured data.
    let (status, _) = h
        .form(
            "/manage/forums",
            &admin,
            &[
                ("fid", &showcase.to_string()),
                ("parent_fid", "0"),
                ("icon_aid", "0"),
                ("name", "Showcase"),
                ("slug", "showcase"),
                ("description", ""),
                ("guidance", ""),
                ("displayorder", "2"),
                ("status", "1"),
                ("visibility", "members"),
                ("version", &version(&h.f.db, showcase).await.to_string()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(h.page("/ai", None).await.0, StatusCode::NOT_FOUND);
    let (status, members) = h.page("/ai", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(breadcrumb(&members).contains("<a href=\"/showcase\">Showcase</a>"));
    assert!(breadcrumb_list(&members).is_none());
    let (_, members_topic) = h.page(&format!("/thread-{tid}"), Some(&admin)).await;
    assert!(breadcrumb(&members_topic).contains("<a href=\"/ai\">AI</a>"));
    assert!(json_ld(&members_topic).is_empty());
    let (_, public_forum) = h.page("/daily", Some(&admin)).await;
    assert!(
        breadcrumb_list(&public_forum).is_some(),
        "a public forum still has it for signed-in members"
    );
    h.f.cleanup_ref().await.unwrap();
}

/// Posting page: the forum drop-down is tiered and the posting guide shows for the selected forum; readable forums
/// use the process cache, so after the admin area saves, sorts or deletes, this process sees it at once after commit,
/// while direct database changes wait for the TTL (or `clear_local`); with a TTL of 0 every read hits the database.
/// When there are enough forums to need "More", "All forums" goes at the end of "More".
#[tokio::test]
async fn posting_page_guidance_and_cached_forum_list() {
    let f = TestFixture::migrated().await.unwrap();
    let (showcase, ai, hosting) = family(&f).await;
    execute(
        &f.db,
        &format!(
            "UPDATE gonof_forum_forum SET guidance='First line <b>bold</b>\n\nThird line' WHERE fid={ai}"
        ),
    )
    .await;
    execute(
        &f.db,
        "UPDATE gonof_forum_forum SET guidance='' WHERE slug IN ('showcase','daily')",
    )
    .await;
    let h = Http::new(f);
    let reader = h.login(READER).await;

    // `/new`: a parent forum is directly followed by its child forums, which have an indent prefix; before a forum is
    // chosen all guides are collapsed and only a single hint is shown.
    let (status, page) = h.page("/new", Some(&reader)).await;
    assert_eq!(status, StatusCode::OK);
    let select = section(&page, "<select id=\"editor-fid\" name=\"fid\"", "</select>");
    let order: Vec<&str> = ["Daily", "Showcase", "└ AI", "└ Hosting", "Releases", "Meta"]
        .into_iter()
        .collect();
    let mut cursor = 0;
    for label in order {
        let at = select[cursor..]
            .find(&format!(">{label}</option>"))
            .unwrap_or_else(|| panic!("{label} is not where expected: {select}"));
        cursor += at;
    }
    assert!(page.contains(&format!("<section class=\"guide-box\" data-forum-guidance=\"{ai}\" hidden><h2>Posting guide</h2><p>First line &#60;b&#62;bold&#60;/b&#62;<br><br>Third line</p></section>")), "{page}");
    assert!(
        !page.contains(&format!("data-forum-guidance=\"{showcase}\"")),
        "forums without guidelines are left out"
    );
    assert!(page.contains(&format!("data-forum-guidance=\"{hosting}\" hidden")));
    assert!(
        page.contains(
            "<p class=\"form-note\" data-forum-guidance-hint>Choose a forum to see its posting guide</p>"
        )
    );

    // `/new/ai`: the selected forum's guide is shown directly and the hint is collapsed.
    let (_, chosen) = h.page("/new/ai", Some(&reader)).await;
    assert!(chosen.contains(&format!("<option value=\"{ai}\" selected>└ AI</option>")));
    assert!(chosen.contains(&format!(
        "data-forum-guidance=\"{ai}\"><h2>Posting guide</h2>"
    )));
    assert!(chosen.contains(&format!("data-forum-guidance=\"{hosting}\" hidden")));
    assert!(chosen.contains("data-forum-guidance-hint hidden>"));
    // Forums the member blocked are not in the drop-down, and their guides are left out.
    h.set_filter(&reader, hosting, FilterMode::Block).await;
    let (_, blocked) = h.page("/new", Some(&reader)).await;
    assert!(
        !blocked.contains("└ Hosting")
            && !blocked.contains(&format!("data-forum-guidance=\"{hosting}\""))
    );
    // Without any guide there is no guide area.
    execute(&h.f.db, "UPDATE gonof_forum_forum SET guidance=''").await;
    h.f.global.clear_local();
    let (_, plain) = h.page("/new", Some(&reader)).await;
    assert!(!plain.contains("guide-box") && !plain.contains("data-forum-guidance-hint"));

    // Process cache: the anonymous copy is loaded first; after a direct database change the old name still shows
    // within the TTL, and only clearing this process's layer shows the new one.
    h.page("/", None).await;
    execute(
        &h.f.db,
        "UPDATE gonof_forum_forum SET name='Daily renamed' WHERE slug='daily'",
    )
    .await;
    let (_, stale) = h.page("/", None).await;
    assert!(header_nav(&stale).contains(">Daily</a>") && !stale.contains("Daily renamed"));
    h.f.global.clear_local();
    let (_, fresh) = h.page("/", None).await;
    assert!(header_nav(&fresh).contains(">Daily renamed</a>"));

    // After a batch sort in the admin area is committed, this process changes the order at once (no waiting for the
    // TTL).
    let admin = h.login(ADMIN).await;
    let daily = forum(&h.f.db, "daily").await;
    let meta = forum(&h.f.db, "meta").await;
    let (status, _) = h
        .form(
            "/manage/forums/order",
            &admin,
            &[
                (&format!("order_{daily}"), "99"),
                (
                    &format!("version_{daily}"),
                    &version(&h.f.db, daily).await.to_string(),
                ),
                (&format!("order_{meta}"), "-1"),
                (
                    &format!("version_{meta}"),
                    &version(&h.f.db, meta).await.to_string(),
                ),
                ("reason", "Reorder"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, reordered) = h.page("/", None).await;
    let nav = header_nav(&reordered);
    assert!(nav.find("/meta").unwrap() < nav.find("/showcase").unwrap());
    assert!(nav.find("/showcase").unwrap() < nav.find("/daily").unwrap());

    // After deleting an empty forum is committed, this process stops listing it at once.
    let releases = forum(&h.f.db, "releases").await;
    let (status, _) = h
        .form(
            &format!("/manage/forums/{releases}/delete"),
            &admin,
            &[
                ("version", &version(&h.f.db, releases).await.to_string()),
                ("reason", "No longer needed"),
                ("target_fid", "0"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, deleted) = h.page("/", None).await;
    assert!(!header_nav(&deleted).contains("/releases"));

    // TTL of 0: every request reads the database, so direct database changes show at once.
    set_setting(&h.f.db, "settings.cache.ttl_seconds", "0")
        .await
        .unwrap();
    h.f.global.clear_local();
    execute(
        &h.f.db,
        "UPDATE gonof_forum_forum SET name='Meta renamed' WHERE slug='meta'",
    )
    .await;
    let (_, uncached) = h.page("/", None).await;
    assert!(header_nav(&uncached).contains(">Meta renamed</a>"));

    // Enough top-level forums to need "More": navigation items take the space first, and "All forums" does not stay
    // in the page header, only at the end of "More".
    for (name, slug) in [
        ("Language Models", "llm"),
        ("Prompt Engineering", "prompting"),
        ("Open Source", "opensource"),
        ("Domain Trading", "domains"),
        ("Servers", "servers"),
        ("Network Security", "security"),
    ] {
        execute(
            &h.f.db,
            &format!("INSERT INTO gonof_forum_forum(name,slug,description,guidance,displayorder) VALUES('{name}','{slug}','','',50)"),
        )
        .await;
    }
    let (_, crowded) = h.page("/", None).await;
    let nav = header_nav(&crowded);
    let more = section(nav, "<ul class=\"forum-more__menu\">", "</ul>");
    assert!(
        more.contains("href=\"/forums\" data-directory-link>All forums</a>"),
        "{more}"
    );
    assert!(!section(nav, "<ul class=\"masthead__board-list\">", "</ul>").contains("/forums"));
    assert!(more.trim_end().ends_with("All forums</a></li>"));
    h.f.cleanup_ref().await.unwrap();
}

/// Save a forum: copy every current value from the database except visibility.
async fn save_visibility(h: &Http, admin: &SessionGrant, fid: i64, visibility: &str) {
    let row = db::query(
        "SELECT name,slug,parent_fid,displayorder,status FROM gonof_forum_forum WHERE fid=?",
    )
    .bind(fid)
    .fetch_one(h.f.db.executor())
    .await
    .unwrap();
    let name = db::text(&row, "name").unwrap();
    let slug = db::text(&row, "slug").unwrap();
    let parent = row
        .try_get::<Option<i64>, _>("parent_fid")
        .unwrap()
        .unwrap_or(0)
        .to_string();
    let order = row.try_get::<i64, _>("displayorder").unwrap().to_string();
    let status = row.try_get::<i64, _>("status").unwrap().to_string();
    let version = version(&h.f.db, fid).await.to_string();
    let (code, body) = h
        .form(
            "/manage/forums",
            admin,
            &[
                ("fid", &fid.to_string()),
                ("parent_fid", &parent),
                ("icon_aid", "0"),
                ("name", &name),
                ("slug", &slug),
                ("description", ""),
                ("guidance", ""),
                ("displayorder", &order),
                ("status", &status),
                ("visibility", visibility),
                ("version", &version),
            ],
        )
        .await;
    assert_eq!(code, StatusCode::SEE_OTHER, "{body}");
}

/// The public feed `/{slug}/feed.xml` and personal feeds judge whether a forum is readable from the process cache
/// of readable forums: a direct database change still gives content within the TTL; after a save in the admin area
/// is committed this process refreshes at once and both feeds stop giving it immediately. Personal feeds use the
/// owner's view: the anonymous copy when they exclude members-only threads, the member copy when they include them.
#[tokio::test]
async fn feeds_follow_the_cached_forum_list() {
    let f = TestFixture::migrated().await.unwrap();
    let (_, ai, _) = family(&f).await;
    let title = "Child forum thread in the feeds";
    ContentWriter::new(f.db.clone(), WriteConfig::default())
        .with_site_url(TEST_SITE_URL)
        .publish(
            READER,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid: ai,
                title: title.into(),
                body: "Forums in the cache".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let owner = Actor {
        uid: READER,
        credentials_version: 1,
        api_key: None,
    };
    let token = content::feed_tokens::issue(&f.db, owner).await.unwrap();
    let h = Http::new(f);
    let admin = h.login(ADMIN).await;
    let public = "/ai/feed.xml";
    let private = format!("/feeds/private/{token}/ai/feed.xml");
    let private_site = format!("/feeds/private/{token}/feed.xml");
    let members =
        |include: bool| content::feed_tokens::set_include_members(&h.f.db, owner, include);

    // Starting point: the public and personal (excluding and including members-only threads) single-forum,
    // parent-forum and site-wide copies all have this thread.
    for path in [
        public,
        "/showcase/feed.xml",
        "/feed.xml",
        &private,
        &private_site,
    ] {
        let (status, body) = h.page(path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(title), "{path}");
    }
    members(true).await.unwrap();
    let (status, body) = h.page(&private, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(title));

    // Both caches are loaded: change the forum to private directly without clearing this process's layer, and within
    // the TTL the feeds still treat it as readable.
    execute(
        &h.f.db,
        &format!("UPDATE gonof_forum_forum SET visibility='private' WHERE fid={ai}"),
    )
    .await;
    assert_eq!(h.page(public, None).await.0, StatusCode::OK);
    assert_eq!(h.page(&private, None).await.0, StatusCode::OK);
    members(false).await.unwrap();
    assert_eq!(h.page(&private, None).await.0, StatusCode::OK);
    execute(
        &h.f.db,
        &format!("UPDATE gonof_forum_forum SET visibility='public' WHERE fid={ai}"),
    )
    .await;

    // Changed to members-only in the admin area: the public feed and the personal feed excluding members-only threads
    // are 404 at once, and the parent-forum and site-wide copies no longer have it; the personal feed including
    // members-only threads still gives it.
    save_visibility(&h, &admin, ai, "members").await;
    assert_eq!(h.page(public, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.page(&private, None).await.0, StatusCode::NOT_FOUND);
    for path in ["/showcase/feed.xml", "/feed.xml", &private_site] {
        let (status, body) = h.page(path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!body.contains(title), "{path}");
    }
    members(true).await.unwrap();
    let (status, body) = h.page(&private, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(title));

    // Then changed to private: the personal feed including members-only threads is 404 at once too, and the
    // site-wide copy no longer has it.
    save_visibility(&h, &admin, ai, "private").await;
    assert_eq!(h.page(&private, None).await.0, StatusCode::NOT_FOUND);
    let (status, body) = h.page(&private_site, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(title));
    h.f.cleanup_ref().await.unwrap();
}
