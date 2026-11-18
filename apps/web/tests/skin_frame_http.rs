//! The frame a skin chooses (docs/skins.md, "Frame"): side navigation with the blocks of `side.nav` on every front
//! page, the header's boards left out beside it, and the top navigation the admin area and pages without side
//! navigation blocks keep.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::{project_root, test_support};
use serde_json::json;
use site::Site;

/// Writes the instance skin `id` (the Ruled fixture's palette, the manifest's `extra` sections) and makes it the
/// site's skin.
async fn use_skin(site: &Site, id: &str, extra: &str) {
    let dir = site.services.root.join(format!("skins/{id}"));
    std::fs::create_dir_all(dir.join("palettes")).unwrap();
    std::fs::copy(
        project_root().join("tests/fixtures/skins/ruled/palettes/main.json"),
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

/// The page data block (docs/skins.md, "Page data").
fn page_data(html: &str) -> serde_json::Value {
    let open = "<script type=\"application/json\" id=\"gonof-page\">";
    let start = html.find(open).expect("the page data block") + open.len();
    serde_json::from_str(&html[start..start + html[start..].find("</script>").unwrap()]).unwrap()
}

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

#[tokio::test]
async fn a_banner_page_head_opens_the_main_region() {
    let site = Site::new(true).await;
    use_skin(
        &site,
        "band",
        "[frame]\npage_head = \"banner\"\n[layout]\n\"page.head.home\" = [{ id = \"hello\", source = \"markdown\", body = { en = \"Hello **there**\" } }]\n",
    )
    .await;
    let banner = "<main class=\"site-main site-main--hero\" id=\"main\" tabindex=\"-1\">\n<div class=\"page-hero\"><div class=\"page-hero__inner container\"><div class=\"page-head page-head--banner\">";
    let home = site.get("/", None).await;
    assert!(home.body.contains(banner), "{}", home.body);
    let hero = &home.body[home.body.find(banner).unwrap()..];
    assert!(hero.contains("data-block=\"hello\""));
    assert!(hero.contains("<strong>there</strong>"));
    // The rest of the page keeps the page width, and the main column no longer opens with the head.
    assert!(hero.contains("</div></div></div>\n<div class=\"container\">"));
    let column = &home.body[home.body.find("layout__main").unwrap()..];
    assert!(!column.contains("class=\"page-head"));
    // A board page's head, the board head by default, goes into the banner with its heading.
    let board = site.get("/showcase", None).await;
    let hero = &board.body[board.body.find(banner).unwrap()..];
    assert!(hero.find("data-block=\"board-head\"").unwrap() < hero.find("layout__main").unwrap());
    assert_eq!(board.body.matches("<h1").count(), 1);
    // A page without head blocks has no banner.
    let thread = site.get("/thread-103", None).await;
    assert!(
        thread
            .body
            .contains("<main class=\"container site-main\" id=\"main\" tabindex=\"-1\">")
    );
    assert!(!thread.body.contains("page-hero"));
    // Beside side navigation the banner spans the main column.
    use_skin(
        &site,
        "bandside",
        &SIDE.replace(
            "header_boards = false",
            "header_boards = false\npage_head = \"banner\"",
        ),
    )
    .await;
    let board = site.get("/showcase", None).await;
    assert!(board.body.contains("<main class=\"site-main site-frame__main\" id=\"main\" tabindex=\"-1\">\n<div class=\"page-hero\"><div class=\"page-hero__inner\"><div class=\"page-head page-head--banner\">"));
    assert!(!board.body.contains("<div class=\"container\">"));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_skin_may_open_the_home_page_with_the_board_directory() {
    let site = Site::new(true).await;
    use_skin(&site, "directory", "[frame]\nhome = \"boards\"\n").await;
    let home = site.get("/", None).await;
    assert!(home.body.contains("data-board-directory"));
    // Its rows carry data-board, so the board variables reach them.
    assert!(
        home.body
            .contains("<li class=\"board-dir__item\" data-board=\"")
    );
    // The page data block lists its boards in order with their latest threads, for the unread marks of site.js
    // (docs/skins.md, "Unread"): the thread, its last activity as the directory shows it, and its highest floor.
    let data = page_data(&home.body);
    let boards = data["directory"]
        .as_array()
        .expect("the directory's boards");
    let shown: Vec<i64> = home
        .body
        .split("<li class=\"board-dir__item\" data-board=\"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].parse().unwrap())
        .collect();
    let listed: Vec<i64> = boards
        .iter()
        .map(|board| board["fid"].as_i64().unwrap())
        .collect();
    assert_eq!(listed, shown);
    let mut latest = 0;
    for board in boards {
        let Some(post) = board["last_post"].as_object() else {
            continue;
        };
        let tid = post["tid"].as_i64().unwrap();
        let highest = site
            .scalar(&format!(
                "SELECT maxposition FROM gonof_forum_thread WHERE tid={tid}"
            ))
            .await;
        assert_eq!(post["max_position"], highest, "{board}");
        let at = post["at"].as_str().unwrap();
        assert!(
            home.body.contains(&format!("<time datetime=\"{at}\"")),
            "{at}"
        );
        latest += 1;
    }
    assert!(latest > 0, "{data}");
    // The thread list of page 2 has no directory.
    assert_eq!(
        page_data(&site.get("/page-2", None).await.body)["directory"],
        serde_json::Value::Null
    );
    // A site's own choice wins over the skin's.
    test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &json!({"v": 1, "skin": "directory", "home": "threads"}).to_string(),
    )
    .await
    .unwrap();
    site.changed();
    assert!(
        !site
            .get("/", None)
            .await
            .body
            .contains("data-board-directory")
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn behavior_modules_load_only_where_the_skin_and_the_page_allow() {
    let site = Site::new(true).await;
    // The default skin loads none.
    let thread = site.get("/thread-103", None).await;
    assert!(!thread.body.contains("data-behaviors"));
    assert!(!thread.body.contains("/static/js/behaviors/"));
    use_skin(
        &site,
        "lively",
        "[layout]\n\"thread.aside\" = [{ id = \"outline\", source = \"thread.outline\", view = \"timeline\", params = { tid = \"current\" }, title = { key = \"block-title-outline\" } }]\n[behaviors]\nenabled = [\"timeline\", \"progress\", \"autohide-header\", \"docked-composer\"]\n",
    )
    .await;
    let thread = site.get("/thread-103", None).await;
    assert!(
        thread
            .body
            .contains(" data-behaviors=\"timeline progress autohide-header docked-composer\">")
    );
    for name in ["timeline", "progress", "autohide-header", "docked-composer"] {
        assert!(
            thread
                .body
                .contains(&format!("/static/js/behaviors/{name}.js?v=")),
            "{name}"
        );
    }
    // Without the module the timeline block is the list of pages, each with the date it begins.
    let start = thread.body.find("data-block=\"outline\"").unwrap();
    let block = &thread.body[start..start + thread.body[start..].find("</section>").unwrap()];
    assert!(block.contains("<ol class=\"block-outline__pages\">"));
    assert!(block.contains("href=\"/thread-103?view=flat\""));
    assert!(block.contains("<time class=\"block-outline__date\" datetime=\""));
    // The page data block carries the outline the module draws.
    let data_start = thread.body.find("id=\"gonof-page\">").unwrap();
    let data =
        &thread.body[data_start..data_start + thread.body[data_start..].find("</script>").unwrap()];
    assert!(data.contains("\"source\":\"thread.outline\""), "{data}");
    // Not in the admin area, on sign-in or account security pages.
    for (path, session) in [
        ("/manage", Some(&site.admin)),
        ("/login", None),
        ("/my/account", Some(&site.member)),
    ] {
        let page = site.get(path, session).await;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        assert!(!page.body.contains("/static/js/behaviors/"), "{path}");
        assert!(!page.body.contains("data-behaviors"), "{path}");
    }
    // Other front pages load them too.
    let home = site.get("/", None).await;
    assert!(home.body.contains("/static/js/behaviors/timeline.js"));
    site.services.cleanup().await.unwrap();
}
