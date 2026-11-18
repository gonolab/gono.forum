//! The core default layout (docs/skins.md, "Places and blocks") renders the asides, the page heads, the footer and
//! the start of the body exactly as the templates did before layouts existed, apart from the block hooks every block
//! carries (`block block--<source> block--<view>` on its outer element, `data-block`), the `data-board` of the board
//! head and the `div.page-head` wrapper around a page head that has blocks. The reference HTML in `tests/default_layout/` was recorded from the templates
//! before layouts; `GONOF_WRITE_DEFAULT_LAYOUT=1` records it again from the current templates.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::path::PathBuf;

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
use tower::ServiceExt;

/// The fixture thread whose author signs in for the member pages.
const FEATURED: i64 = 103;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
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

/// The text from `start` (included) to the first `end` after it (excluded, or included with `inclusive`).
fn region<'a>(html: &'a str, start: &str, end: &str, inclusive: bool) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("{start} is on the page"));
    let to = from
        + html[from..]
            .find(end)
            .unwrap_or_else(|| panic!("{end} follows {start}"));
    &html[from..if inclusive { to + end.len() } else { to }]
}

/// Replaces the value of every `_csrf` and `request_key` input: they differ per session and per request.
fn without_tokens(html: &str) -> String {
    let mut text = html.to_owned();
    for marker in ["name=\"_csrf\" value=\"", "name=\"request_key\" value=\""] {
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(marker) {
            let value = at + marker.len();
            out.push_str(&rest[..value]);
            out.push_str("TOKEN");
            rest = &rest[value + rest[value..].find('"').unwrap()..];
        }
        out.push_str(rest);
        text = out;
    }
    text
}

/// Removes the block hooks: ` block block--<source> block--<view>` inside a class attribute, ` data-block="…"` (with
/// the board head's ` data-board="…"` after it), and the `div.page-head` wrapper (its opening tag and the `</div>` that closes it, found by counting nested `div`s).
fn without_block_hooks(html: &str) -> String {
    let mut out = html.to_owned();
    while let Some(at) = out.find(" block block--") {
        // Three words: "block", "block--<source>", "block--<view>".
        let mut end = at + " block".len();
        for _ in 0..2 {
            end = end + 1 + out[end + 1..].find([' ', '"']).unwrap();
        }
        out.replace_range(at..end, "");
    }
    let attribute = " data-block=\"";
    while let Some(at) = out.find(attribute) {
        let mut close = at + attribute.len() + out[at + attribute.len()..].find('"').unwrap();
        let board = " data-board=\"";
        if out[close + 1..].starts_with(board) && out[at..close].contains("board-head") {
            close = close + 1 + board.len() + out[close + 1 + board.len()..].find('"').unwrap();
        }
        out.replace_range(at..=close, "");
    }
    let open = "<div class=\"page-head\">";
    while let Some(at) = out.find(open) {
        out.replace_range(at..at + open.len(), "");
        let mut depth = 1;
        let mut cursor = at;
        loop {
            let next_open = out[cursor..].find("<div").map(|i| i + cursor);
            let next_close = out[cursor..]
                .find("</div>")
                .map(|i| i + cursor)
                .expect("the page head wrapper is closed");
            match next_open {
                Some(open_at) if open_at < next_close => {
                    depth += 1;
                    cursor = open_at + 4;
                }
                _ => {
                    depth -= 1;
                    if depth == 0 {
                        out.replace_range(next_close..next_close + "</div>".len(), "");
                        break;
                    }
                    cursor = next_close + "</div>".len();
                }
            }
        }
    }
    out
}

/// Up to 200 bytes on each side of `at`, cut at character boundaries.
fn near(text: &str, at: usize) -> &str {
    let mut from = at.saturating_sub(200).min(text.len());
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (at + 200).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    &text[from..to]
}

/// The parts of a page that places fill.
fn parts(html: &str) -> String {
    let mut parts = vec![
        region(html, "<body", "<header class=\"masthead\">", false),
        region(html, "<aside class=\"layout__aside\"", "</aside>", true),
        region(html, "<footer class=\"colophon\">", "</footer>", true),
    ];
    if html.contains("<div class=\"layout__main listing__main\">") {
        parts.push(region(
            html,
            "<div class=\"layout__main listing__main\">",
            "<div class=\"pane\">",
            false,
        ));
    }
    if html.contains("<div class=\"layout__main topic__main\">") {
        parts.push(region(
            html,
            "<div class=\"layout__main topic__main\">",
            "<article class=\"thread\"",
            false,
        ));
    }
    without_tokens(&parts.join("\n----\n"))
}

fn reference(name: &str) -> PathBuf {
    project_root()
        .join("apps/web/tests/default_layout")
        .join(format!("{name}.html"))
}

#[tokio::test]
async fn the_core_default_layout_renders_the_pages_as_before() {
    let services = TestFixture::migrated().await.unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    content::seed_fixture(&db).await.unwrap();
    let showcase = scalar(
        &db,
        "SELECT fid FROM gonof_forum_forum WHERE slug='showcase'",
    )
    .await;
    let ai = scalar(&db, "SELECT fid FROM gonof_forum_forum WHERE slug='ai'").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    test_support::sync_forum_copies(&db, showcase)
        .await
        .unwrap();
    // Recent activity on a few threads, so the hot card lists them whatever day the test runs.
    let now = gono_forum_web::identity::now();
    execute(
        &db,
        &format!(
            "UPDATE gonof_forum_thread SET lastpost={now}, cache_revision=cache_revision+1 WHERE tid IN (101,103,105,106)"
        ),
    )
    .await;
    test_support::refresh_listing(&db).await.unwrap();
    // The footer timing changes from request to request.
    test_support::set_setting(&db, "settings.display.footer_metrics", "false")
        .await
        .unwrap();
    let app = router::build(services.config.clone(), &project_root(), db.clone());
    let identity = Identity::new(
        db.clone(),
        services.config.identity.clone(),
        services.root.clone(),
        services.global.clone(),
    );
    let author = scalar(
        &db,
        &format!("SELECT authorid FROM gonof_forum_thread WHERE tid={FEATURED}"),
    )
    .await;
    let grant = http_auth::login(&identity, &db, author).await;
    let member = Some(grant.session_token.as_str());
    let pages = [
        ("home", "/".to_owned(), None),
        ("home-member", "/".to_owned(), member),
        ("home-page-2", "/page-2".to_owned(), None),
        ("board", "/showcase".to_owned(), None),
        ("board-member", "/showcase".to_owned(), member),
        ("child-board", "/ai".to_owned(), None),
        ("board-plain", "/daily".to_owned(), member),
        ("thread", format!("/thread-{FEATURED}"), None),
        ("thread-member", format!("/thread-{FEATURED}"), member),
        ("search", "/search?q=notes".to_owned(), None),
    ];
    let write = std::env::var_os("GONOF_WRITE_DEFAULT_LAYOUT").is_some();
    let mut differences = Vec::new();
    for (name, path, session) in pages {
        let html = page(&app, &path, session).await;
        let raw = parts(&html);
        if !write {
            // The hooks are really there, so the comparison below is not vacuous.
            let hooks: &[&str] = match name {
                "home" | "home-member" | "home-page-2" | "search" => &[
                    " block block--member-card block--card\" data-block=\"member-card\"",
                    " block block--threads block--mini-list\" data-block=\"hot\"",
                    " block block--threads block--mini-list\" data-block=\"latest\"",
                    " block block--site-overview block--card\" data-block=\"site-overview\"",
                ],
                "board" | "board-member" | "child-board" | "board-plain" => &[
                    "<div class=\"page-head\">",
                    " block block--board-head block--head\" data-block=\"board-head\" data-board=\"",
                    " data-block=\"hot-board\"",
                ],
                _ => &[" block block--member-card block--card\" data-block=\"member-card-thread\""],
            };
            for hook in hooks {
                assert!(raw.contains(hook), "{name}: {hook}");
            }
        }
        let current = without_block_hooks(&raw);
        let file = reference(name);
        if write {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &current).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        if current != expected {
            let at = current
                .bytes()
                .zip(expected.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(current.len().min(expected.len()));
            differences.push(format!(
                "{name} ({path}) differs at byte {at}:\n  now:    {:?}\n  before: {:?}",
                near(&current, at),
                near(&expected, at)
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    services.cleanup().await.unwrap();
}
