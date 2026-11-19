//! The public markup hooks skins build on (docs/skins.md): the fixed class names and data attributes of list rows,
//! counts split into number and word, `<time datetime>`, the optional elements hidden in the default look, the drawn
//! breadcrumb separator, and the page data block with its fields. Rendered on the home page, a board page, a child
//! board, a thread page, a member's space, search and the forum directory.
#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content,
    db::{self, Database},
    identity::Identity,
    project_root, router,
    test_support::{self, TestFixture},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// The fixture threads the cases use (tests/fixtures/forum.json).
const PINNED: i64 = 101;
const FEATURED: i64 = 103;
/// Made a locked poll in the `ai` board, which is moved under `showcase`.
const LOCKED_POLL: i64 = 104;
/// An ordinary thread: no tags, no states.
const PLAIN: i64 = 105;
/// Made a site-wide (level-3) pin in `daily`, so it shows in the cross-forum pins of other boards.
const SITE_PIN: i64 = 106;

struct Site {
    app: Router,
    services: TestFixture,
    showcase: i64,
    ai: i64,
    daily: i64,
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn fid(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
    )
    .await
}

/// The development fixture, with `ai` under `showcase`, a locked poll and a site-wide pin. `site_name` replaces the
/// site name when given (the escaping case). The router is built last, so its caches see these changes.
async fn site(site_name: Option<&str>) -> Site {
    let services = TestFixture::migrated().await.unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    content::seed_fixture(&db).await.unwrap();
    let showcase = fid(&db, "showcase").await;
    let ai = fid(&db, "ai").await;
    let daily = fid(&db, "daily").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    test_support::sync_forum_copies(&db, showcase)
        .await
        .unwrap();
    execute(&db, &format!("UPDATE gonof_forum_thread SET category=4, closed=1, cache_revision=cache_revision+1 WHERE tid={LOCKED_POLL}")).await;
    execute(&db, &format!("UPDATE gonof_forum_thread SET displayorder=3, cache_revision=cache_revision+1 WHERE tid={SITE_PIN}")).await;
    test_support::refresh_listing(&db).await.unwrap();
    test_support::set_setting(&db, "settings.forum.directory_statistics", "true")
        .await
        .unwrap();
    if let Some(name) = site_name {
        test_support::set_setting(&db, "settings.site.name", name)
            .await
            .unwrap();
    }
    let app = router::build(services.config.clone(), &project_root(), db);
    Site {
        app,
        services,
        showcase,
        ai,
        daily,
    }
}

async fn page(app: &Router, path: &str, session: Option<&str>) -> String {
    let mut request = Request::builder().uri(path);
    if let Some(token) = session {
        request = request.header(header::COOKIE, format!("gonof_session__port58833={token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

/// The raw text of the page data block.
fn block_text(html: &str) -> &str {
    let open = "<script type=\"application/json\" id=\"gonof-page\">";
    let start = html.find(open).expect("the page data block") + open.len();
    let end = start + html[start..].find("</script>").unwrap();
    assert_eq!(html.matches(open).count(), 1, "one page data block");
    &html[start..end]
}

fn block(html: &str) -> Value {
    serde_json::from_str(block_text(html)).unwrap()
}

/// The page data block without the page's `now` and each block's `generated_at`, the seconds the page and its blocks
/// were built (docs/data.md, "Common shapes"): two requests for the same page can fall on either side of a second, so
/// only these fields may differ between them. The page and every block must still carry them.
fn timeless_block(html: &str) -> Value {
    let mut data = block(html);
    let now = data.as_object_mut().unwrap().remove("now");
    assert!(
        now.as_ref()
            .and_then(Value::as_str)
            .is_some_and(|now| now.ends_with('Z')),
        "now: {now:?}"
    );
    for (id, envelope) in data["blocks"].as_object_mut().unwrap() {
        let at = envelope.as_object_mut().unwrap().remove("generated_at");
        assert!(
            at.as_ref()
                .and_then(Value::as_str)
                .is_some_and(|at| at.ends_with('Z')),
            "{id}: {at:?}"
        );
    }
    data
}

/// The `<li>` of a thread in the first list on the page that has it.
fn row(html: &str, tid: i64) -> &str {
    let marker = format!("data-thread-id=\"{tid}\"");
    let at = html
        .find(&marker)
        .unwrap_or_else(|| panic!("thread {tid} is listed"));
    let start = html[..at].rfind("<li").unwrap();
    let end = at + html[at..].find("</li>").unwrap();
    &html[start..end]
}

/// The cross-forum pins block of a board page.
fn cross_pins(html: &str) -> &str {
    let at = html.find("data-cross-pins").expect("the cross-forum pins");
    &html[at..at + html[at..].find("</ul>").unwrap()]
}

/// The `<li>` opening tag of a row, to read its data attributes.
fn opening_tag(row: &str) -> &str {
    &row[..=row.find('>').unwrap()]
}

/// Every `datetime` on the page is an instant in UTC to the second, `2026-09-08T00:02:00Z`.
fn assert_iso_times(html: &str) {
    let mut seen = 0;
    for part in html.split("datetime=\"").skip(1) {
        let value = &part[..part.find('"').unwrap()];
        let bytes = value.as_bytes();
        assert!(
            value.len() == 20
                && bytes[4] == b'-'
                && bytes[10] == b'T'
                && bytes[13] == b':'
                && value.ends_with('Z'),
            "datetime {value}"
        );
        seen += 1;
    }
    assert!(seen > 0, "the page has <time> elements");
}

fn assert_list_row_hooks(row: &str) {
    for hook in [
        "class=\"row-meta__author\"",
        "class=\"row-meta__replies\"",
        "<span class=\"row-meta__views\" aria-label=\"",
        "<span class=\"stat\"><span class=\"stat__value\">",
        "<time class=\"row-meta__time\" datetime=\"",
        // The optional board badge, after the row body.
        "<span class=\"row-end\" data-optional><a class=\"board-chip\" href=\"/",
    ] {
        assert!(row.contains(hook), "{hook} in {row}");
    }
}

#[tokio::test]
async fn list_rows_carry_their_classes_states_and_optional_elements() {
    let Site {
        app,
        services,
        showcase,
        ai,
        daily,
    } = site(None).await;
    let home = page(&app, "/", None).await;
    // A board page's head carries the board, so the board variables reach it.
    let board = page(&app, "/daily", None).await;
    assert!(board.contains(&format!(
        "data-block=\"board-head\" data-board=\"{daily}\">"
    )));

    for tid in [PINNED, FEATURED, LOCKED_POLL, PLAIN, SITE_PIN] {
        assert_list_row_hooks(row(&home, tid));
    }
    // Rows with replies name the last replier.
    assert!(row(&home, FEATURED).contains("<span class=\"row-meta__last\" translate=\"no\">"));
    // The title time keeps its hover text.
    assert!(row(&home, FEATURED).contains("\" title=\""));

    let pinned = opening_tag(row(&home, PINNED));
    assert!(pinned.contains("data-state=\"pinned\"") && !pinned.contains("data-kind"));
    assert!(row(&home, PINNED).contains("<span class=\"row-labels\">"));
    assert!(
        row(&home, PINNED).contains("<span class=\"row-icon row-icon--pinned\" data-optional>")
    );
    assert!(row(&home, PINNED).contains("<span class=\"sr-only\">Pinned</span>"));

    let featured = opening_tag(row(&home, FEATURED));
    assert!(featured.contains(&format!("data-board=\"{showcase}\"")));
    assert!(featured.contains("data-state=\"featured\""));

    let poll = opening_tag(row(&home, LOCKED_POLL));
    assert!(poll.contains(&format!("data-board=\"{ai}\"")));
    assert!(poll.contains("data-kind=\"poll\"") && poll.contains("data-state=\"closed\""));
    assert!(row(&home, LOCKED_POLL).contains("<span class=\"tag tag--poll\">"));
    assert!(
        row(&home, LOCKED_POLL)
            .contains("<span class=\"row-icon row-icon--locked\" data-optional>")
    );
    assert!(row(&home, LOCKED_POLL).contains("<span class=\"sr-only\">Locked</span>"));

    // The highest floor, for site.js to compare with what the reader saw (docs/skins.md, "Unread").
    for tid in [PINNED, FEATURED, LOCKED_POLL, PLAIN, SITE_PIN] {
        let highest = scalar(
            &services.db,
            &format!("SELECT maxposition FROM gonof_forum_thread WHERE tid={tid}"),
        )
        .await;
        assert!(
            opening_tag(row(&home, tid)).contains(&format!(
                "data-thread-id=\"{tid}\" data-maxposition=\"{highest}\" "
            )),
            "{tid}"
        );
    }
    assert!(opening_tag(row(&home, FEATURED)).contains("data-maxposition=\"79\""));
    // Nothing is unread on the server: site.js adds the marks.
    assert!(!home.contains("is-unread") && !home.contains("row-unread"));
    // Whose reading state the browser keeps: 0 for visitors.
    assert!(home.contains("<meta name=\"gonof-reader\" content=\"0\">"));

    let plain = row(&home, PLAIN);
    assert!(
        !opening_tag(plain).contains("data-state") && !opening_tag(plain).contains("data-kind")
    );
    assert!(!plain.contains("row-labels") && !plain.contains("row-icon"));
    assert_iso_times(&home);

    // A parent board names a child board's thread, and page 1 shows the other families' site-wide pins.
    let board = page(&app, "/showcase", None).await;
    assert!(row(&board, LOCKED_POLL).contains("<span class=\"row-meta__board\" data-child-forum>"));
    let pins = cross_pins(&board);
    let pin = row(pins, SITE_PIN);
    assert!(opening_tag(pin).contains(&format!("data-board=\"{daily}\"")));
    assert!(opening_tag(pin).contains("data-state=\"pinned\""));
    assert_list_row_hooks(pin);
    assert!(opening_tag(pin).contains(&format!(
        "data-thread-id=\"{SITE_PIN}\" data-maxposition=\"7\" "
    )));
    assert!(pin.contains("<span class=\"row-meta__board\"><a href=\"/daily\">"));
    // The count's number and word in their own elements, reading as before: "6 replies".
    let replies = scalar(
        &services.db,
        &format!("SELECT replies FROM gonof_forum_thread WHERE tid={SITE_PIN}"),
    )
    .await;
    assert_eq!(replies, 6);
    assert!(pin.contains(
        "<span class=\"stat\"><span class=\"stat__value\">6</span> <span class=\"stat__label\">replies</span></span>"
    ), "{pin}");
    // The view count likewise, number and word.
    let views = scalar(
        &services.db,
        &format!("SELECT views FROM gonof_forum_thread WHERE tid={SITE_PIN}"),
    )
    .await;
    assert!((2..1000).contains(&views), "{views}");
    assert!(pin.contains(&format!(
        "<span class=\"row-meta__views\" aria-label=\"{views} views\"><span class=\"stat\"><span class=\"stat__value\">{views}</span> <span class=\"stat__label\">views</span></span></span>"
    )), "{pin}");
    assert!(pin.contains("<span class=\"row-icon row-icon--pinned\" data-optional>"));

    // Search results use the same rows.
    let search = page(&app, "/search?q=RSS", None).await;
    assert_list_row_hooks(row(&search, FEATURED));
    assert!(opening_tag(row(&search, FEATURED)).contains("data-maxposition=\"79\""));
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn thread_profile_and_directory_hooks_are_present() {
    let Site {
        app,
        services,
        showcase,
        ai,
        daily,
    } = site(None).await;

    // The breadcrumb separators are empty spans the stylesheet draws.
    let child = page(&app, "/ai", None).await;
    assert!(child.contains(
        "<a href=\"/\">Home</a><span class=\"crumbs__sep\" aria-hidden=\"true\"></span><a href=\"/showcase\">Showcase</a>"
    ));
    assert!(!child.contains("\u{203a}"));

    let thread = page(&app, &format!("/thread-{FEATURED}"), None).await;
    assert!(thread.contains("<span class=\"crumbs__sep\" aria-hidden=\"true\"></span>"));
    assert!(thread.contains("<time class=\"row-time\" datetime=\""));
    assert!(thread.contains(
        "<a class=\"button button--small thread-head__reply\" href=\"#reply-editor\" data-optional>Reply</a>"
    ));
    assert!(thread.contains("id=\"reply-editor\""));
    assert!(thread.contains("<span class=\"thread-stats__views\">"));
    // The thread's own board on the article, for the board variables.
    assert!(thread.contains(&format!(
        "<article class=\"thread\" data-board=\"{showcase}\" aria-labelledby=\"thread-title\">"
    )));
    // The action icons are <use> references to symbols drawn once per page.
    assert_eq!(
        thread
            .matches("class=\"action-icon-sprite\" hidden")
            .count(),
        1
    );
    for action in ["reply", "quote"] {
        assert!(thread.contains(&format!("<symbol id=\"action-icon-{action}\"")));
        let class = format!("<a class=\"floor__action floor__action--{action}\" href=\"");
        let at = thread.find(&class).unwrap_or_else(|| panic!("{class}"));
        let link = &thread[at..at + thread[at..].find("</a>").unwrap()];
        let icon = format!(
            "<svg class=\"floor__action-icon\" data-optional aria-hidden=\"true\" focusable=\"false\"><use href=\"#action-icon-{action}\"></use></svg>"
        );
        assert!(link.contains(&icon), "{link}");
        assert!(
            link.contains("<span class=\"floor__action-label\">"),
            "{link}"
        );
    }
    assert_iso_times(&thread);
    // The category tag carries the same class as in lists.
    let poll = page(&app, &format!("/thread-{LOCKED_POLL}"), None).await;
    assert!(poll.contains("<span class=\"tag tag--poll\">Poll</span>"));

    let author = scalar(
        &services.db,
        &format!("SELECT authorid FROM gonof_forum_thread WHERE tid={FEATURED}"),
    )
    .await;
    let profile = page(&app, &format!("/space/{author}/threads"), None).await;
    for hook in [
        "class=\"profile__meta-board row-meta__board\"",
        "<time class=\"row-meta__time\" datetime=\"",
        "<span class=\"row-meta__replies\"><span class=\"stat\"><span class=\"stat__value\">",
        "<span class=\"stat__label\">",
        "Joined <time datetime=\"",
    ] {
        assert!(profile.contains(hook), "{hook}");
    }
    assert_iso_times(&profile);

    let directory = page(&app, "/forums", None).await;
    // Every row of the directory, child boards included, carries its board.
    for fid in [daily, showcase, ai] {
        assert!(
            directory.contains(&format!("data-board=\"{fid}\">")),
            "{fid}"
        );
    }
    for hook in [
        "<span class=\"row-meta__threads\"><span class=\"stat\"><span class=\"stat__value\">",
        "<span class=\"row-meta__today\"><span class=\"stat\"><span class=\"stat__label\">Today</span> <span class=\"stat__value\">",
    ] {
        assert!(directory.contains(hook), "{hook}");
    }
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_page_data_block_describes_the_page_and_the_site() {
    let Site {
        app,
        services,
        showcase,
        ..
    } = site(None).await;
    let started = gono_forum_web::identity::now();
    let fields = [
        "v",
        "type",
        "tid",
        "fid",
        "uid",
        "page",
        "thread",
        "site",
        "blocks",
        "directory",
        "now",
    ];
    let site_part = serde_json::json!({
        "name": "Sample Community",
        "description": "A sample community built with Gono Forum, for running tests.",
        "language": "en",
        "dir": "ltr",
        "base": "",
        "credit_name": "Coins",
        "index_name": "Popularity Index",
        "date_format": "ymd-dash",
    });
    let check = |data: &Value, kind: &str| {
        let keys: Vec<&str> = data
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), fields.len(), "{data}");
        for field in fields {
            assert!(keys.contains(&field), "{field} in {data}");
        }
        assert_eq!(data["v"], 1);
        assert_eq!(data["type"], kind);
        assert_eq!(data["site"], site_part);
        // The server's time as it wrote the page, to the second, for scripts that must not trust the device's clock.
        let now = data["now"]
            .as_str()
            .unwrap_or_else(|| panic!("now in {data}"));
        assert!(now.ends_with('Z') && !now.contains('.'), "{now}");
        let now = chrono::DateTime::parse_from_rfc3339(now)
            .unwrap_or_else(|error| panic!("{now}: {error}"))
            .timestamp();
        assert!(
            (started..=gono_forum_web::identity::now()).contains(&now),
            "{now}"
        );
    };

    let home = block(&page(&app, "/page-2", None).await);
    check(&home, "home");
    // The records of the page's data-source blocks, by block id (docs/data.md); core cards are not data sources.
    let blocks: Vec<&str> = home["blocks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(blocks, ["hot", "latest"], "{home}");
    assert_eq!(home["blocks"]["hot"]["source"], "threads");
    assert_eq!(home["blocks"]["hot"]["params"]["scope"], "site");
    assert_eq!(home["blocks"]["latest"]["params"]["sort"], "created");
    assert_eq!(
        (home["fid"].clone(), home["page"].clone()),
        (Value::Null, 2.into())
    );
    assert_eq!(home["thread"], Value::Null);
    // Only a home page that opens with the board directory lists it (tests/skin_frame_http.rs).
    assert_eq!(home["directory"], Value::Null);

    let board = block(&page(&app, "/showcase", None).await);
    check(&board, "board");
    assert_eq!(board["fid"], showcase);
    assert_eq!(board["page"], 1);

    let search = block(&page(&app, "/search?q=RSS", None).await);
    check(&search, "search");
    assert_eq!(search["fid"], Value::Null);

    let directory = block(&page(&app, "/forums", None).await);
    check(&directory, "other");
    assert_eq!(directory["page"], Value::Null);

    let author = scalar(
        &services.db,
        &format!("SELECT authorid FROM gonof_forum_thread WHERE tid={FEATURED}"),
    )
    .await;
    let member = block(&page(&app, &format!("/space/{author}"), None).await);
    check(&member, "member");
    assert_eq!(member["uid"], author);
    assert_eq!(member["page"], 1);

    // Thread 103 has 78 replies: page 2 of the flat view shows the opening post, then floors 52 to 79.
    let html = page(&app, &format!("/thread-{FEATURED}-2?view=flat"), None).await;
    let thread = block(&html);
    check(&thread, "thread");
    assert_eq!(thread["tid"], FEATURED);
    assert_eq!(thread["page"], 2);
    assert_eq!(thread["fid"], showcase);
    assert_eq!(thread["thread"]["replies"], 78);
    assert_eq!(thread["thread"]["pages"], 2);
    let floors = thread["thread"]["floors"].as_array().unwrap();
    let positions: Vec<i64> = floors
        .iter()
        .map(|floor| floor["position"].as_i64().unwrap())
        .collect();
    let expected: Vec<i64> = std::iter::once(1).chain(52..=79).collect();
    assert_eq!(positions, expected);
    // Each floor is on the page with the same time.
    for floor in floors {
        let id = format!("id=\"floor-{}\"", floor["position"]);
        let at = html.find(&id).unwrap_or_else(|| panic!("{id}"));
        let time = format!("datetime=\"{}\"", floor["at"].as_str().unwrap());
        assert!(html[at..].contains(&time), "{time}");
        assert!(floor["pid"].as_i64().unwrap() > 0);
    }
    let first_pid = scalar(
        &services.db,
        &format!("SELECT pid FROM gonof_forum_post WHERE first_tid={FEATURED}"),
    )
    .await;
    assert_eq!(floors[0]["pid"], first_pid);
    // The nested view lists each reply's nested replies after it, in page order.
    let nested = block(&page(&app, &format!("/thread-{FEATURED}"), None).await);
    assert_eq!(nested["thread"]["floors"][0]["position"], 1);
    services.cleanup().await.unwrap();
}

/// The floor a thread page counts as read up to (docs/skins.md, "Unread"): every floor up to it is on this page or an
/// earlier one. Thread 103 has 78 replies, 50 a page: floors 2 to 55 are top-level replies and floors 56 to 79 are
/// nested under floor 2, so the nested view shows some of them on page 1, beside top-level replies up to 51.
#[tokio::test]
async fn a_thread_page_counts_as_read_up_to_its_last_whole_floor() {
    let Site { app, services, .. } = site(None).await;
    for (path, expected) in [
        // Flat: consecutive floors, the page's highest; the last page counts as the whole thread.
        (format!("/thread-{FEATURED}?view=flat"), 51),
        (format!("/thread-{FEATURED}-2?view=flat"), 79),
        // Nested: the page's last top-level reply, although floor 2's nested replies on it go further.
        (format!("/thread-{FEATURED}"), 51),
        (format!("/thread-{FEATURED}-2"), 79),
    ] {
        let html = page(&app, &path, None).await;
        let data = block(&html);
        assert_eq!(data["thread"]["read_through"], expected, "{path}");
        // The replies section carries it too, for the fragment a posted reply swaps in, with the thread's highest floor
        // for the timeline and progress modules (docs/skins.md, "Behavior modules").
        assert!(
            html.contains(&format!(
                "id=\"topic-replies\" aria-labelledby=\"replies-heading\" data-read-through=\"{expected}\" data-last-position=\"79\">"
            )),
            "{path}"
        );
        if path == format!("/thread-{FEATURED}") {
            let shown = data["thread"]["floors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|floor| floor["position"].as_i64().unwrap())
                .max()
                .unwrap();
            assert!(
                shown > 55,
                "nested replies of floor 2 are on page 1: {shown}"
            );
        }
    }
    services.cleanup().await.unwrap();
}

/// Nothing personal: no `me.*` source and nothing about the viewer's account. The blocks' records are what the page
/// shows this viewer, so with nothing members-only or blocked a member gets the same block as an anonymous visitor.
#[tokio::test]
async fn the_page_data_block_has_nothing_personal() {
    let Site { app, services, .. } = site(None).await;
    let identity = Identity::new(
        services.db.clone(),
        services.config.identity.clone(),
        services.root.clone(),
        services.global.clone(),
    );
    let author = scalar(
        &services.db,
        &format!("SELECT authorid FROM gonof_forum_thread WHERE tid={FEATURED}"),
    )
    .await;
    let grant = http_auth::login(&identity, &services.db, author).await;
    for path in [
        "/".to_owned(),
        "/showcase".to_owned(),
        format!("/thread-{FEATURED}"),
        format!("/space/{author}"),
    ] {
        let anonymous = page(&app, &path, None).await;
        let member = page(&app, &path, Some(&grant.session_token)).await;
        assert!(
            member.contains(&format!("data-recent-owner=\"{author}\""))
                || member.contains("class=\"account-nav"),
            "{path} is rendered for the member"
        );
        // The reader's uid is in the head, outside the data block, for the unread state site.js keeps.
        assert!(
            member.contains(&format!(
                "<meta name=\"gonof-reader\" content=\"{author}\">"
            )),
            "{path}"
        );
        assert!(
            anonymous.contains("<meta name=\"gonof-reader\" content=\"0\">"),
            "{path}"
        );
        assert_eq!(
            timeless_block(&anonymous),
            timeless_block(&member),
            "{path}"
        );
        let data = block(&member);
        for (_, envelope) in data["blocks"].as_object().unwrap() {
            assert!(
                !envelope["source"].as_str().unwrap().starts_with("me."),
                "{path}"
            );
        }
        assert!(!block_text(&member).contains("\"csrf"), "{path}");
    }
    services.cleanup().await.unwrap();
}

/// No value can end the data block early: `<`, `>` and `&` are JSON escapes, and the value reads back unchanged.
#[tokio::test]
async fn the_page_data_block_escapes_markup() {
    let name = "Ends </script><b>here</b> & \"there\"";
    let Site { app, services, .. } = site(Some(name)).await;
    let html = page(&app, "/", None).await;
    let text = block_text(&html);
    assert!(
        !text.contains('<') && !text.contains('>') && !text.contains('&'),
        "{text}"
    );
    assert!(text.contains("\\u003c/script\\u003e"));
    assert_eq!(block(&html)["site"]["name"], name);
    services.cleanup().await.unwrap();
}
