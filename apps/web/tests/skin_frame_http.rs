//! The frame a skin chooses (docs/skins.md, "Frame"): side navigation with the blocks of `side.nav` on every front
//! page, the header's boards left out beside it, and the top navigation the admin area and pages without side
//! navigation blocks keep.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::{project_root, test_support};
use serde_json::json;
use site::Site;

/// Writes the instance skin `id` (the Ledger fixture's palette, the manifest's `extra` sections) and makes it the
/// site's skin.
async fn use_skin(site: &Site, id: &str, extra: &str) {
    let dir = site.services.root.join(format!("skins/{id}"));
    std::fs::create_dir_all(dir.join("palettes")).unwrap();
    std::fs::copy(
        project_root().join("tests/fixtures/skins/ledger/palettes/main.json"),
        dir.join("palettes/main.json"),
    )
    .unwrap();
    std::fs::write(
        dir.join("skin.toml"),
        format!(
            "contract = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = {{ en = \"Frame test\" }}\n[palettes]\ndefault = \"main\"\n{extra}"
        ),
    )
    .unwrap();
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &json!({"v": 1, "skin": id}).to_string(),
    )
    .await
    .unwrap();
    site.services.global.refresh_all().await.unwrap();
}

const SIDE: &str = r#"
[frame]
nav = "side"
header_boards = false

[layout]
"side.nav" = [
  { id = "side-boards", source = "nav-boards" },
  { id = "side-links", source = "nav-links" },
  { id = "side-note", source = "markdown", body = { en = "Welcome to the **side**." } },
]
"#;

fn side_nav(html: &str) -> &str {
    let start = html
        .find("<div class=\"side-nav\" data-side-nav>")
        .unwrap_or_else(|| panic!("no side navigation: {html}"));
    &html[start..start + html[start..].find("<main ").unwrap()]
}

#[tokio::test]
async fn side_navigation_shows_its_blocks_on_every_front_page() {
    let site = Site::new(true).await;
    use_skin(&site, "rail", SIDE).await;
    for path in [
        "/",
        "/showcase",
        "/thread-103",
        "/forums",
        "/space/1",
        "/search?q=a",
    ] {
        let page = site.get(path, None).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        let body = &page.body;
        assert!(
            body.contains(" data-nav=\"side\" data-header-boards=\"false\""),
            "{path}"
        );
        let side = side_nav(body);
        for id in ["side-boards", "side-links", "side-note"] {
            assert!(
                side.contains(&format!("data-block=\"{id}\"")),
                "{path}: {id}"
            );
        }
        assert!(side.contains("<strong>side</strong>"), "{path}");
        assert!(
            body.contains(
                "<main class=\"site-main site-frame__main\" id=\"main\" tabindex=\"-1\">"
            ),
            "{path}"
        );
        // The header leaves its boards to the side navigation; the drawer still lists them.
        assert!(!body.contains("class=\"masthead__boards\""), "{path}");
        assert!(
            body.contains("class=\"drawer__link\" href=\"/showcase\""),
            "{path}"
        );
    }
    // The current board is marked in the side navigation's links and board tree, also on its threads' pages.
    let board = site.get("/showcase", None).await;
    let side = side_nav(&board.body);
    assert!(side.contains("href=\"/showcase\" aria-current=\"page\""));
    assert!(side.contains("class=\"block-boards__name\" href=\"/showcase\" aria-current=\"page\""));
    let thread = site.get("/thread-103", None).await;
    assert_eq!(
        side_nav(&thread.body)
            .matches("aria-current=\"page\"")
            .count(),
        2
    );
    // The admin area keeps the top navigation and its own module menu.
    let manage = site.get("/manage", Some(&site.admin)).await;
    assert_eq!(manage.status, StatusCode::OK);
    assert!(
        manage
            .body
            .contains(" data-nav=\"top\" data-header-boards=\"true\"")
    );
    assert!(!manage.body.contains("data-side-nav"));
    assert!(manage.body.contains("class=\"masthead__boards\""));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn without_side_navigation_blocks_a_page_keeps_the_top_navigation() {
    let site = Site::new(true).await;
    use_skin(
        &site,
        "rail",
        &SIDE.replace("header_boards = false", "header_boards = true"),
    )
    .await;
    // Beside the side navigation the header may keep its boards.
    let home = site.get("/", None).await;
    assert!(
        home.body
            .contains(" data-nav=\"side\" data-header-boards=\"true\"")
    );
    assert!(home.body.contains("class=\"masthead__boards\""));
    // A layout whose side.nav only has blocks for members shows visitors the top navigation.
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &json!({"v": 1, "skin": "rail", "places": {"side.nav": [
            {"id": "mine", "source": "nav-boards", "show": "members"},
        ]}})
        .to_string(),
    )
    .await
    .unwrap();
    site.changed();
    let guest = site.get("/", None).await;
    assert!(
        guest
            .body
            .contains(" data-nav=\"top\" data-header-boards=\"true\"")
    );
    assert!(!guest.body.contains("data-side-nav"));
    assert!(
        guest
            .body
            .contains("<main class=\"container site-main\" id=\"main\" tabindex=\"-1\">")
    );
    let member = site.get("/", Some(&site.member)).await;
    assert!(side_nav(&member.body).contains("data-block=\"mine\""));
    // An empty side.nav is the same as no side navigation.
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &json!({"v": 1, "skin": "rail", "places": {"side.nav": []}}).to_string(),
    )
    .await
    .unwrap();
    site.changed();
    let member = site.get("/", Some(&site.member)).await;
    assert!(member.body.contains(" data-nav=\"top\""));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_skin_without_side_navigation_blocks_is_unusable() {
    let site = Site::new(false).await;
    use_skin(&site, "bare", "[frame]\nnav = \"side\"\n").await;
    // The skin fails its checks, so pages fall back to the built-in default.
    let home = site.get("/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("data-skin=\"default\""));
    assert!(home.body.contains(" data-nav=\"top\""));
    site.services.cleanup().await.unwrap();
}
