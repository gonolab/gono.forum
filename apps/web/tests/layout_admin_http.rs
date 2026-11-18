//! Appearance › Layout (docs/skins.md, "Editing a layout"): changes go to a draft that visitors do not see, the
//! administrator previews it alone, publishing writes the document and records `layout.save`; a domain changes single
//! places; changing the skin of a changed layout asks first; only administrators get in.
#[path = "support/site.rs"]
mod site;

use axum::http::{StatusCode, header};
use site::Site;

const PLACE: &str = "home.aside";

fn aside(html: &str) -> &str {
    let start = html.find("<aside class=\"layout__aside\"").unwrap();
    &html[start..start + html[start..].find("</aside>").unwrap()]
}

/// The blocks a place lists in the editor, by their `data-block`-less labels: here simply how many rows.
fn rows(html: &str, place: &str) -> usize {
    let at = html
        .find(&format!("data-layout-place=\"{place}\""))
        .unwrap();
    let section = &html[at..at + html[at..].find("</section>").unwrap()];
    section.matches("class=\"layout-blocks__item\"").count()
}

async fn draft_body(site: &Site, target: &str) -> Option<String> {
    gono_forum_web::db::query_scalar::<String>(
        "SELECT svalue FROM gonof_common_settings WHERE skey=?",
    )
    .bind(format!("settings.layout_draft.{target}.body"))
    .fetch_optional(site.db().executor())
    .await
    .unwrap()
}

fn cookie(answer: &site::Answer) -> String {
    answer
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .find(|value| value.starts_with("gonof_skin_preview__port58833="))
        .map(|value| value[..value.find(';').unwrap_or(value.len())].to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn edits_go_to_a_draft_that_is_previewed_and_published() {
    let site = Site::new(true).await;
    let editor = site.get("/manage/layout", Some(&site.admin)).await;
    assert_eq!(editor.status, StatusCode::OK, "{}", editor.body);
    assert_eq!(editor.body.matches("data-layout-place=").count(), 8);
    assert_eq!(
        rows(&editor.body, PLACE),
        6,
        "the core default's blocks are listed"
    );
    assert!(editor.body.contains("data-layout-draft=\"false\""));

    // Taking the place in starts a draft; visitors still get the published layout.
    let own = site
        .post(
            "/manage/layout/own",
            &site.admin,
            &[("domain", "0"), ("place", PLACE)],
        )
        .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    assert!(draft_body(&site, "site").await.is_some());
    // The add form is generated from the parameter lists.
    let form = site
        .get(
            "/manage/layout/add?domain=0&place=home.aside&source=threads",
            Some(&site.admin),
        )
        .await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    for field in [
        "name=\"view\"",
        "name=\"scope\"",
        "name=\"sort\"",
        "name=\"days\"",
        "name=\"limit\"",
        "name=\"title_key\"",
    ] {
        assert!(form.body.contains(field), "{field}");
    }
    let added = site
        .post(
            "/manage/layout/add",
            &site.admin,
            &[
                ("domain", "0"),
                ("place", PLACE),
                ("source", "threads"),
                ("view", "list"),
                ("scope", "site"),
                ("sort", "latest"),
                ("days", "7"),
                ("limit", "4"),
                ("title_key", "block-title-active"),
                ("show", "all"),
            ],
        )
        .await;
    assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
    // A Markdown block without its text is refused with the field named.
    let refused = site
        .post(
            "/manage/layout/add",
            &site.admin,
            &[
                ("domain", "0"),
                ("place", PLACE),
                ("source", "markdown"),
                ("title_key", "block-title-notice"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused.body.contains("data-layout-error"),
        "{}",
        refused.body
    );
    // Move the new block to the top, remove the member card that is now second.
    let editor = site.get("/manage/layout", Some(&site.admin)).await;
    assert_eq!(rows(&editor.body, PLACE), 7);
    for index in (1..=6).rev() {
        let moved = site
            .post(
                "/manage/layout/move",
                &site.admin,
                &[
                    ("domain", "0"),
                    ("place", PLACE),
                    ("index", &index.to_string()),
                    ("direction", "up"),
                ],
            )
            .await;
        assert_eq!(moved.status, StatusCode::SEE_OTHER);
    }
    let removed = site
        .post(
            "/manage/layout/remove",
            &site.admin,
            &[("domain", "0"), ("place", PLACE), ("index", "1")],
        )
        .await;
    assert_eq!(removed.status, StatusCode::SEE_OTHER);
    let draft = draft_body(&site, "site").await.unwrap();
    let draft: serde_json::Value = serde_json::from_str(&draft).unwrap();
    let ids: Vec<&str> = draft["places"][PLACE]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "threads-1",
            "hot",
            "latest",
            "recent-local",
            "site-overview",
            "friend-links"
        ]
    );
    let home = site.get("/", None).await;
    assert!(
        !home.body.contains("data-block=\"threads-1\""),
        "the draft is not live"
    );

    // The preview shows the draft to this administrator only.
    let preview = site
        .post("/manage/layout/preview", &site.admin, &[("domain", "0")])
        .await;
    assert_eq!(preview.status, StatusCode::SEE_OTHER);
    let sealed = cookie(&preview);
    assert!(!sealed.is_empty());
    let previewed = site
        .get_with(
            "/",
            None,
            &[(
                "cookie",
                &format!("gonof_session__port58833={}; {sealed}", site.admin.0),
            )],
        )
        .await;
    assert!(
        aside(&previewed.body).contains("data-block=\"threads-1\""),
        "the administrator sees the draft"
    );
    assert!(previewed.body.contains("data-skin-preview"));
    assert!(
        previewed
            .body
            .contains(gono_forum_web::i18n::Lang::EN.layout_preview_bar())
    );
    let other = site
        .get_with(
            "/",
            None,
            &[(
                "cookie",
                &format!("gonof_session__port58833={}; {sealed}", site.member.0),
            )],
        )
        .await;
    assert!(
        !other.body.contains("data-block=\"threads-1\""),
        "someone else's cookie shows nothing"
    );

    // Publishing makes it everyone's, ends the draft and the preview, and records layout.save.
    let published = site
        .post("/manage/layout/publish", &site.admin, &[("domain", "0")])
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );
    assert!(
        published
            .header(header::LOCATION)
            .ends_with("saved=published")
    );
    assert!(
        cookie(&published).ends_with("gonof_skin_preview__port58833="),
        "the preview ends"
    );
    assert!(draft_body(&site, "site").await.is_none());
    let home = site.get("/", None).await;
    let side = aside(&home.body);
    assert!(side.contains("data-block=\"threads-1\""));
    assert!(!side.contains("person-card"));
    assert_eq!(side.matches("class=\"block-list__item\"").count(), 4);
    let logged = site
        .scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='layout.save'")
        .await;
    assert_eq!(logged, 1);
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_domain_changes_single_places_and_a_skin_change_asks_first() {
    let site = Site::prepared(true, |db| async move {
        gono_forum_web::db::query(
            "INSERT INTO gonof_common_site_domain(domain_id,site_url,dateline) VALUES(7,'https://other.example.test',1)",
        )
        .execute(db.executor())
        .await
        .unwrap();
    })
    .await;
    // A domain's draft only holds the places it changes.
    let own = site
        .post(
            "/manage/layout/own",
            &site.admin,
            &[("domain", "7"), ("place", "footer")],
        )
        .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    let added = site
        .post(
            "/manage/layout/add",
            &site.admin,
            &[
                ("domain", "7"),
                ("place", "footer"),
                ("source", "markdown"),
                ("body_en", "Hello from the other domain"),
            ],
        )
        .await;
    assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(&site, "7").await.unwrap()).unwrap();
    assert_eq!(
        draft["places"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["footer"]
    );
    let published = site
        .post("/manage/layout/publish", &site.admin, &[("domain", "7")])
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );
    let document = site
        .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.domain_layout.7.body'")
        .await;
    assert!(
        document.contains("Hello from the other domain"),
        "{document}"
    );
    let editor = site.get("/manage/layout?domain=7", Some(&site.admin)).await;
    assert!(
        editor
            .body
            .contains("data-layout-place=\"footer\" data-layout-own=\"true\"")
    );
    assert!(
        editor
            .body
            .contains("data-layout-place=\"home.aside\" data-layout-own=\"false\"")
    );

    // The site's layout has its own aside: choosing a skin asks what to do with it.
    let own = site
        .post(
            "/manage/layout/own",
            &site.admin,
            &[("domain", "0"), ("place", PLACE)],
        )
        .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER);
    site.post("/manage/layout/publish", &site.admin, &[("domain", "0")])
        .await;
    let asked = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "default:default")],
        )
        .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert!(asked.body.contains("data-layout-switch"));
    let kept = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "default:default"), ("layout", "keep")],
        )
        .await;
    assert_eq!(kept.status, StatusCode::SEE_OTHER);
    let body = site
        .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
        .await;
    assert!(body.contains("\"home.aside\""), "{body}");
    let reset = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "default:default"), ("layout", "skin")],
        )
        .await;
    assert_eq!(reset.status, StatusCode::SEE_OTHER);
    let body = site
        .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
        .await;
    assert!(body.contains("\"places\":null"), "{body}");
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn only_administrators_edit_layouts() {
    let site = Site::new(false).await;
    for session in [&site.moderator, &site.member] {
        let page = site.get("/manage/layout", Some(session)).await;
        assert_ne!(page.status, StatusCode::OK);
        let post = site
            .post(
                "/manage/layout/own",
                session,
                &[("domain", "0"), ("place", PLACE)],
            )
            .await;
        assert_ne!(post.status, StatusCode::SEE_OTHER);
    }
    assert!(draft_body(&site, "site").await.is_none());
    site.services.cleanup().await.unwrap();
}

/// The board directory of the home page (docs/skins.md, "Board directory home"), or `None`.
fn directory(html: &str) -> Option<&str> {
    let start = html.find("<section class=\"panel board-dir\"")?;
    Some(&html[start..start + html[start..].find("</section>").unwrap()])
}

#[tokio::test]
async fn the_home_page_choice_is_drafted_previewed_and_published() {
    let site = Site::new(true).await;
    // One child board, to see child boards listed under their parent.
    let showcase = site.fid("showcase").await;
    site.execute(&format!(
        "UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE slug='ai'"
    ))
    .await;
    site.changed();
    let editor = site.get("/manage/layout", Some(&site.admin)).await;
    assert!(editor.body.contains("data-layout-home=\"inherit\""));
    assert!(directory(&site.get("/", None).await.body).is_none());

    // Choosing the board directory goes into the draft.
    let chosen = site
        .post(
            "/manage/layout/home",
            &site.admin,
            &[("domain", "0"), ("home", "boards")],
        )
        .await;
    assert_eq!(chosen.status, StatusCode::SEE_OTHER, "{}", chosen.body);
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(&site, "site").await.unwrap()).unwrap();
    assert_eq!(draft["home"], "boards");
    let editor = site.get("/manage/layout", Some(&site.admin)).await;
    assert!(editor.body.contains("data-layout-home=\"boards\""));
    assert!(
        directory(&site.get("/", None).await.body).is_none(),
        "not live yet"
    );
    // A later change to a place keeps the choice in the draft.
    let own = site
        .post(
            "/manage/layout/own",
            &site.admin,
            &[("domain", "0"), ("place", PLACE)],
        )
        .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER);
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(&site, "site").await.unwrap()).unwrap();
    assert_eq!(draft["home"], "boards");
    let refused = site
        .post(
            "/manage/layout/home",
            &site.admin,
            &[("domain", "0"), ("home", "gallery")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);

    // The administrator previews it alone.
    let preview = site
        .post("/manage/layout/preview", &site.admin, &[("domain", "0")])
        .await;
    let sealed = cookie(&preview);
    let previewed = site
        .get_with(
            "/",
            None,
            &[(
                "cookie",
                &format!("gonof_session__port58833={}; {sealed}", site.admin.0),
            )],
        )
        .await;
    assert!(directory(&previewed.body).is_some());

    // Published, every visitor's home page opens with the directory: the top-level boards with their counts and
    // latest threads, above the thread list; later pages, other sorts and board pages do not.
    let published = site
        .post("/manage/layout/publish", &site.admin, &[("domain", "0")])
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );
    let home = site.get("/", None).await;
    let shown = directory(&home.body).expect("the directory");
    let boards = site
        .scalar("SELECT COUNT(*) FROM gonof_forum_forum WHERE parent_fid IS NULL AND status=1 AND visibility='public'")
        .await;
    assert_eq!(
        shown.matches("class=\"board-dir__item\"").count() as i64,
        boards
    );
    assert!(shown.contains("class=\"row-meta__threads\""));
    assert!(shown.contains("class=\"board-dir__last-title\""));
    assert!(shown.contains("<time datetime=\""));
    assert!(shown.contains("class=\"board-dir__children\""), "{shown}");
    assert!(
        home.body.find("data-board-directory").unwrap()
            < home.body.find("data-scroll-list").unwrap(),
        "the thread list follows"
    );
    for path in ["/page-2", "/?sort=created", "/showcase", "/search?q=a"] {
        let page = site.get(path, None).await;
        assert!(directory(&page.body).is_none(), "{path}");
    }
    let logged = site
        .text("SELECT data FROM gonof_common_log WHERE type='layout.save'")
        .await;
    let logged: serde_json::Value = serde_json::from_str(&logged).unwrap();
    assert_eq!(logged["home"], "boards");
    assert_eq!(logged["previous_home"], serde_json::Value::Null);

    // A skin's default gives way to the site's choice.
    let body = site
        .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
        .await;
    let mut layout: serde_json::Value = serde_json::from_str(&body).unwrap();
    layout["home"] = serde_json::json!("threads");
    gono_forum_web::test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &layout.to_string(),
    )
    .await
    .unwrap();
    site.changed();
    assert!(directory(&site.get("/", None).await.body).is_none());
    site.services.cleanup().await.unwrap();
}
