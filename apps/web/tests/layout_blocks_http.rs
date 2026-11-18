//! Layouts other than the core default (docs/skins.md, "Places and blocks"): blocks from the site's layout document
//! and from a skin's `[layout]`, their hooks and views, the page data block's records, who sees a block, the limit of
//! blocks a page, and blocks that no longer pass their checks being skipped instead of failing the page.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::{project_root, test_support};
use serde_json::{Value, json};
use site::Site;

/// The text of the page data block.
fn page_data(html: &str) -> Value {
    let open = "<script type=\"application/json\" id=\"gonof-page\">";
    let start = html.find(open).unwrap() + open.len();
    let end = start + html[start..].find("</script>").unwrap();
    serde_json::from_str(&html[start..end].replace("\\u003c", "<")).unwrap()
}

async fn layout(site: &Site, places: Value) {
    let body = json!({"v": 1, "places": places}).to_string();
    test_support::set_setting(site.db(), "settings.layout.body", &body)
        .await
        .unwrap();
    site.changed();
}

fn aside(html: &str) -> &str {
    let start = html.find("<aside class=\"layout__aside\"").unwrap();
    &html[start..start + html[start..].find("</aside>").unwrap()]
}

#[tokio::test]
async fn a_site_layout_shows_its_blocks_and_their_records() {
    let site = Site::new(true).await;
    let daily = site.fid("daily").await;
    layout(
        &site,
        json!({
            "home.aside": [
                {"id": "me", "source": "member-card"},
                {"id": "active", "source": "threads", "params": {"scope": "site", "sort": "latest", "limit": 3},
                 "view": "list", "title": {"key": "block-title-active"}},
                {"id": "boards", "source": "boards", "params": {"counts": true}, "view": "grid",
                 "title": {"key": "block-title-boards"}},
                {"id": "notice", "source": "markdown", "title": {"text": {"en": "Read this"}},
                 "body": {"en": "**Welcome** to the forum."}},
                {"id": "members-only", "source": "markdown", "show": "members", "title": {"key": "block-title-notice"},
                 "body": {"en": "Only for members."}},
                // A view this version does not have: skipped, the page still renders.
                {"id": "broken", "source": "threads", "view": "carousel", "title": {"key": "block-title-hot"}},
            ],
            "footer": [{"id": "stats", "source": "site.stats", "params": {"scope": "site"}, "view": "compact"}],
            "page.head.board": [],
            "thread.aside": [
                {"id": "outline", "source": "thread.outline", "view": "progress", "title": {"key": "block-title-outline"}},
            ],
        }),
    )
    .await;
    let home = site.get("/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    let side = aside(&home.body);
    assert!(side.contains("person-card block block--member-card block--card\" data-block=\"me\""));
    assert!(
        side.contains("block block--threads block--list\" data-block=\"active\""),
        "{side}"
    );
    assert_eq!(side.matches("class=\"block-list__item\"").count(), 3);
    assert!(side.contains("block block--boards block--grid\" data-block=\"boards\""));
    assert!(side.contains(&format!(
        "class=\"block-boards__tile\" data-board=\"{daily}\""
    )));
    assert!(
        side.contains("<strong>Welcome</strong> to the forum."),
        "{side}"
    );
    assert!(
        !side.contains("Only for members."),
        "show: members leaves out visitors"
    );
    assert!(!side.contains("data-block=\"broken\""));
    // The old sidebar cards are gone with the default layout.
    assert!(!side.contains("data-recent-threads"));
    let footer = &home.body[home.body.find("<footer class=\"colophon\">").unwrap()..];
    assert!(footer.contains("<div class=\"colophon__blocks\">"));
    assert!(footer.contains(
        "<p class=\"block-stats block-stats--compact block block--site-stats block--compact\" data-block=\"stats\">"
    ));
    assert!(
        footer.contains("Built on <a class=\"colophon__brand\""),
        "the core line stays"
    );
    let data = page_data(&home.body);
    let ids: Vec<&str> = data["blocks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        ids,
        ["active", "boards", "stats"],
        "data sources only: {data}"
    );
    assert_eq!(
        data["blocks"]["active"]["items"].as_array().unwrap().len(),
        3
    );
    assert_eq!(data["blocks"]["boards"]["params"]["counts"], true);
    let member = site.get("/", Some(&site.member)).await;
    assert!(aside(&member.body).contains("Only for members."));

    // Without a board head the board page keeps its heading for screen readers.
    let board = site.get("/daily", None).await;
    assert!(!board.body.contains("class=\"page-head\""));
    assert!(!board.body.contains("board-head"));
    let name = site
        .text(&format!(
            "SELECT name FROM gonof_forum_forum WHERE fid={daily}"
        ))
        .await;
    assert!(
        board
            .body
            .contains(&format!("<h1 class=\"sr-only\">{name}</h1>"))
    );
    // board.aside is not in the document: the core default's blocks.
    assert!(board.body.contains("data-block=\"hot-board\""));

    let thread = site.get("/thread-103", None).await;
    assert!(
        thread
            .body
            .contains("block block--thread-outline block--progress\" data-block=\"outline\"")
    );
    assert!(
        thread.body.contains("href=\"/thread-103?view=flat\""),
        "page 1 of the outline"
    );
    let data = page_data(&thread.body);
    assert_eq!(
        data["blocks"]["outline"]["params"]["tid"], 103,
        "current resolves to the thread"
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_page_shows_at_most_ten_blocks() {
    let site = Site::new(true).await;
    // Eight in the aside and four in the footer: a saved document could not hold this, a page skips the last two.
    let notes = |prefix: &str, count: usize| -> Vec<Value> {
        (0..count)
            .map(|index| {
                json!({"id": format!("{prefix}{index}"), "source": "markdown", "title": {"key": "block-title-notice"},
                       "body": {"en": format!("{prefix} note {index}")}})
            })
            .collect()
    };
    layout(
        &site,
        json!({"home.aside": notes("a", 8), "footer": notes("f", 4)}),
    )
    .await;
    let home = site.get("/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    for index in 0..8 {
        assert!(home.body.contains(&format!("a note {index}")));
    }
    assert!(home.body.contains("f note 0") && home.body.contains("f note 1"));
    assert!(!home.body.contains("f note 2") && !home.body.contains("f note 3"));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_skin_layout_fills_the_places_the_site_leaves_alone() {
    let site = Site::prepared(true, |_| async {}).await;
    // An instance skin whose manifest has a layout for the home aside.
    let dir = site.services.root.join("skins/stacked");
    std::fs::create_dir_all(dir.join("palettes")).unwrap();
    let fixture = project_root().join("tests/fixtures/skins/ruled");
    std::fs::copy(
        fixture.join("palettes/main.json"),
        dir.join("palettes/main.json"),
    )
    .unwrap();
    let manifest = std::fs::read_to_string(fixture.join("skin.toml"))
        .unwrap()
        .replace("id = \"ruled\"", "id = \"stacked\"")
        .replace("name = { en = \"Ruled\" }", "name = { en = \"Stacked\" }");
    std::fs::write(
        dir.join("skin.toml"),
        format!(
            "{manifest}\n[layout]\n\"home.aside\" = [{{ id = \"pinned\", source = \"threads\", view = \"mini-list\", params = {{ sort = \"pinned\" }}, title = {{ key = \"block-title-pinned\" }} }}]\n"
        ),
    )
    .unwrap();
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        r#"{"v":1,"skin":"stacked"}"#,
    )
    .await
    .unwrap();
    site.services.global.refresh_all().await.unwrap();
    let home = site.get("/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(
        home.body.contains("data-skin=\"stacked\""),
        "the skin is in use"
    );
    let side = aside(&home.body);
    assert!(side.contains("data-block=\"pinned\""), "{side}");
    assert!(
        !side.contains("person-card"),
        "the skin's aside replaces the core default's"
    );
    // The site's own document wins over the skin's layout.
    layout(
        &site,
        json!({"home.aside": [{"id": "me", "source": "member-card"}]}),
    )
    .await;
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &json!({"v": 1, "skin": "stacked", "places": {"home.aside": [{"id": "me", "source": "member-card"}]}}).to_string(),
    )
    .await
    .unwrap();
    site.changed();
    let home = site.get("/", None).await;
    assert!(aside(&home.body).contains("data-block=\"me\""));
    assert!(!aside(&home.body).contains("data-block=\"pinned\""));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_debug_panel_lists_every_block_with_its_time() {
    let site = Site::new(true).await;
    layout(
        &site,
        json!({"home.aside": [
            {"id": "me", "source": "member-card"},
            {"id": "guests-only", "source": "markdown", "show": "guests", "title": {"key": "block-title-notice"},
             "body": {"en": "Sign up!"}},
            {"id": "latest", "source": "threads", "view": "mini-list", "title": {"key": "block-title-latest"},
             "params": {"sort": "created", "limit": 3}},
        ]}),
    )
    .await;
    test_support::set_setting(site.db(), "settings.debug.panel", "true")
        .await
        .unwrap();
    site.changed();
    let home = site.get("/", Some(&site.admin)).await;
    assert!(
        home.body.contains("data-debug-layout"),
        "the panel has the section"
    );
    for id in ["me", "guests-only", "latest"] {
        assert!(
            home.body
                .contains(&format!("data-debug-layout-block=\"{id}\"")),
            "{id} is listed"
        );
    }
    let skipped = home
        .body
        .find("data-debug-layout-block=\"guests-only\"")
        .unwrap();
    assert!(
        home.body[skipped..skipped + 80].contains("data-debug-layout-skipped"),
        "a block not for this visitor is listed as skipped"
    );
    site.services.cleanup().await.unwrap();
}
