//! The built-in Ledger skin (docs/skins.md, "Built-in skins") through the real application: chosen for the site, its
//! frame, layout and behavior modules reach the pages, every one of its blocks renders, its palettes are choices of
//! their own, and its name comes from the translation files.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::test_support;
use serde_json::json;
use site::Site;

/// Makes Ledger the site's skin, with `palette` or its default one.
async fn use_ledger(site: &Site, palette: Option<&str>) {
    let mut body = json!({"v": 1, "skin": "ledger"});
    if let Some(palette) = palette {
        body["palette"] = json!(palette);
    }
    test_support::set_setting(site.db(), "settings.layout.body", &body.to_string())
        .await
        .unwrap();
    site.services.global.refresh_all().await.unwrap();
}

/// The page's `<html>` start tag.
fn html_tag(html: &str) -> &str {
    let start = html.find("<html").expect("an html element");
    &html[start..start + html[start..].find('>').unwrap() + 1]
}

/// The part of the page from `open` to the next `close`.
fn part<'a>(html: &'a str, open: &str, close: &str) -> &'a str {
    let start = html
        .find(open)
        .unwrap_or_else(|| panic!("no {open}: {html}"));
    &html[start..start + html[start..].find(close).unwrap()]
}

fn has_blocks(html: &str, ids: &[&str]) {
    for id in ids {
        assert!(
            html.contains(&format!("data-block=\"{id}\"")),
            "{id} is missing: {html}"
        );
    }
}

#[tokio::test]
async fn ledger_brings_its_frame_layout_and_behavior_modules() {
    let site = Site::new(true).await;
    use_ledger(&site, None).await;

    let home = site.get("/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    let tag = html_tag(&home.body);
    assert!(
        tag.contains("data-skin=\"ledger\" data-skin-source=\"builtin\" data-palette=\"blue\""),
        "{tag}"
    );
    assert!(
        tag.contains("data-nav=\"side\" data-header-boards=\"false\""),
        "{tag}"
    );
    assert!(tag.contains("data-avatar-shape=\"circle\""), "{tag}");
    // The side column with the board tree; the header leaves its boards out. Without a navigation document the
    // header's entries are the same boards, so they are not listed a second time.
    let side = part(
        &home.body,
        "<div class=\"side-nav\" data-side-nav>",
        "<main ",
    );
    has_blocks(side, &["side-boards"]);
    assert!(!side.contains("data-block=\"side-links\""), "{side}");
    assert_eq!(side.matches("href=\"/showcase\"").count(), 1, "{side}");
    assert!(!home.body.contains("class=\"masthead__boards"));
    has_blocks(
        part(&home.body, "<aside class=\"layout__aside\"", "</aside>"),
        &["me", "hot", "stats"],
    );
    // The thread list opens the home page; the board directory stays a choice of the layout.
    assert!(!home.body.contains("data-board-directory"));

    let board = site.get("/showcase", None).await;
    assert_eq!(board.status, StatusCode::OK);
    has_blocks(
        part(&board.body, "<aside class=\"layout__aside\"", "</aside>"),
        &["me-board", "hot-board", "stats-board"],
    );
    // The board head stays in the main column (no banner).
    assert!(board.body.contains("<div class=\"page-head\">"));
    assert!(!board.body.contains("page-hero"));

    let thread = site.get("/thread-103", None).await;
    assert_eq!(thread.status, StatusCode::OK);
    assert!(
        html_tag(&thread.body)
            .contains(" data-behaviors=\"timeline progress autohide-header docked-composer\">"),
        "{}",
        html_tag(&thread.body)
    );
    let aside = part(&thread.body, "<aside class=\"layout__aside\"", "</aside>");
    has_blocks(aside, &["me-thread", "outline"]);
    assert!(
        aside.contains("block--thread-outline block--timeline"),
        "{aside}"
    );
    for name in ["timeline", "progress", "autohide-header", "docked-composer"] {
        assert!(
            thread
                .body
                .contains(&format!("/static/js/behaviors/{name}.js?v=")),
            "{name}"
        );
    }

    // The stylesheet: the four palettes and the skin's own rules, in the skin layer.
    let start = home
        .body
        .find("/skin/ledger.css?v=")
        .expect("the skin's stylesheet");
    let href = &home.body[start..start + home.body[start..].find('"').unwrap()];
    let css = site.get(href, None).await;
    assert_eq!(css.status, StatusCode::OK);
    for palette in ["blue", "teal", "indigo", "contrast"] {
        assert!(
            css.body
                .contains(&format!(":root[data-palette=\"{palette}\"]")),
            "{palette}"
        );
    }
    assert!(css.body.contains(".rows--topics > .rows__item {"));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn ledger_lists_the_navigation_document_under_the_board_tree() {
    let site = Site::new(true).await;
    use_ledger(&site, None).await;
    let showcase = site.fid("showcase").await;
    test_support::set_setting(
        site.db(),
        "settings.navigation.body",
        &format!("Projects | forum:{showcase}\nRules | /rules"),
    )
    .await
    .unwrap();
    site.services.global.refresh_all().await.unwrap();
    let board = site.get("/showcase", None).await;
    assert_eq!(board.status, StatusCode::OK);
    let side = part(
        &board.body,
        "<div class=\"side-nav\" data-side-nav>",
        "<main ",
    );
    has_blocks(side, &["side-boards", "side-links"]);
    let links = part(side, "data-block=\"side-links\"", "</nav>");
    assert!(
        links.contains("href=\"/showcase\" aria-current=\"page\">Projects</a>"),
        "{links}"
    );
    assert!(links.contains("href=\"/rules\">Rules</a>"), "{links}");
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn ledger_palettes_are_choices_of_their_own() {
    let site = Site::new(true).await;
    use_ledger(&site, Some("contrast")).await;
    assert!(html_tag(&site.get("/", None).await.body).contains("data-palette=\"contrast\""));
    // A palette the skin does not have falls back to its default.
    use_ledger(&site, Some("night")).await;
    assert!(html_tag(&site.get("/", None).await.body).contains("data-palette=\"blue\""));

    let page = site.get("/manage/skins", Some(&site.admin)).await;
    assert_eq!(page.status, StatusCode::OK);
    for palette in ["blue", "teal", "indigo", "contrast"] {
        assert!(
            page.body
                .contains(&format!("<option value=\"ledger:{palette}\"")),
            "{palette}"
        );
    }
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn ledger_is_named_in_the_page_language() {
    let site = Site::new(true).await;
    test_support::set_setting(site.db(), "settings.site.language", "zh-CN")
        .await
        .unwrap();
    site.services.global.clear_local();
    let zh = gono_forum_web::i18n::Lang::parse("zh-CN").unwrap();
    let page = site.get("/manage/skins", Some(&site.admin)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(&format!(
        "<option value=\"ledger:blue\">{} · blue</option>",
        zh.skin_builtin_ledger_name()
    )));
    assert!(page.body.contains(&format!(
        "<tr data-skin-row=\"ledger\" data-skin-usable=\"true\"><th scope=\"row\"><span translate=\"no\">{}</span>",
        zh.skin_builtin_ledger_name()
    )));
    site.services.cleanup().await.unwrap();
}
