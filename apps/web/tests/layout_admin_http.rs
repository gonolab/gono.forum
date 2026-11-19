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

/// The value of the first hidden input named `name`.
fn hidden<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html.find(&marker)? + marker.len();
    Some(&html[start..start + html[start..].find('"').unwrap()])
}

/// The revision the editor shows for `domain` (`"0"`: the site), which its forms carry.
async fn revision(site: &Site, domain: &str) -> String {
    let editor = site
        .get(
            &format!("/manage/layout?domain={domain}"),
            Some(&site.admin),
        )
        .await;
    assert_eq!(editor.status, StatusCode::OK, "{}", editor.body);
    hidden(&editor.body, "revision")
        .unwrap_or_default()
        .to_owned()
}

/// A form of the layout editor posted from a freshly opened editor page, with the revision it shows.
async fn edit(site: &Site, path: &str, fields: &[(&str, &str)]) -> site::Answer {
    let domain = fields
        .iter()
        .find(|(name, _)| *name == "domain")
        .map_or("0", |(_, value)| *value);
    let revision = revision(site, domain).await;
    let mut fields = fields.to_vec();
    fields.push(("revision", &revision));
    site.post(path, &site.admin, &fields).await
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
    let own = edit(
        &site,
        "/manage/layout/own",
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
    let added = edit(
        &site,
        "/manage/layout/add",
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
    let refused = edit(
        &site,
        "/manage/layout/add",
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
    for _ in 0..6 {
        let moved = edit(
            &site,
            "/manage/layout/move",
            &[
                ("domain", "0"),
                ("place", PLACE),
                ("block", "threads-1"),
                ("direction", "up"),
            ],
        )
        .await;
        assert_eq!(moved.status, StatusCode::SEE_OTHER);
    }
    let removed = edit(
        &site,
        "/manage/layout/remove",
        &[("domain", "0"), ("place", PLACE), ("block", "member-card")],
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
    let published = edit(&site, "/manage/layout/publish", &[("domain", "0")]).await;
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
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "7"), ("place", "footer")],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    let added = edit(
        &site,
        "/manage/layout/add",
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
    let published = edit(&site, "/manage/layout/publish", &[("domain", "7")]).await;
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
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "0"), ("place", PLACE)],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER);
    edit(&site, "/manage/layout/publish", &[("domain", "0")]).await;
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
    let chosen = edit(
        &site,
        "/manage/layout/home",
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
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "0"), ("place", PLACE)],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER);
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(&site, "site").await.unwrap()).unwrap();
    assert_eq!(draft["home"], "boards");
    let refused = edit(
        &site,
        "/manage/layout/home",
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
    let published = edit(&site, "/manage/layout/publish", &[("domain", "0")]).await;
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

/// The block ids of `PLACE` in the site's draft.
async fn draft_ids(site: &Site) -> Vec<String> {
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(site, "site").await.expect("a draft")).unwrap();
    draft["places"][PLACE]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_form_from_an_editor_page_left_open_changes_nothing_once_the_layout_changed() {
    let site = Site::new(true).await;
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "0"), ("place", PLACE)],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    // One tab shows the member card first and the hot threads second.
    let left_open = site.get("/manage/layout", Some(&site.admin)).await;
    let old = hidden(&left_open.body, "revision")
        .unwrap_or_default()
        .to_owned();
    assert_eq!(draft_ids(&site).await[..2], ["member-card", "hot"]);
    // Another tab swaps them.
    let swapped = edit(
        &site,
        "/manage/layout/move",
        &[
            ("domain", "0"),
            ("place", PLACE),
            ("index", "1"),
            ("block", "hot"),
            ("direction", "up"),
        ],
    )
    .await;
    assert_eq!(swapped.status, StatusCode::SEE_OTHER, "{}", swapped.body);
    assert_eq!(draft_ids(&site).await[..2], ["hot", "member-card"]);
    // Every form of the first tab is refused: removing or moving the member card (first on that page), following the
    // next layout again, choosing the home page, publishing and discarding the draft it showed.
    let changed = gono_forum_web::i18n::Lang::EN.layout_draft_changed();
    for (path, fields) in [
        (
            "/manage/layout/remove",
            vec![("place", PLACE), ("index", "0"), ("block", "member-card")],
        ),
        (
            "/manage/layout/move",
            vec![
                ("place", PLACE),
                ("index", "0"),
                ("block", "member-card"),
                ("direction", "down"),
            ],
        ),
        ("/manage/layout/own", vec![("place", PLACE)]),
        ("/manage/layout/inherit", vec![("place", PLACE)]),
        ("/manage/layout/home", vec![("home", "boards")]),
        ("/manage/layout/publish", vec![]),
        ("/manage/layout/discard", vec![]),
    ] {
        let mut fields = fields;
        fields.extend([("domain", "0"), ("revision", old.as_str())]);
        let answer = site.post(path, &site.admin, &fields).await;
        assert_eq!(
            answer.status,
            StatusCode::CONFLICT,
            "{path}: the draft is now {:?}",
            draft_body(&site, "site").await
        );
        assert!(answer.body.contains(changed), "{path}: {}", answer.body);
        assert!(
            answer.body.contains("data-layout-place=\"home.aside\""),
            "{path}: the editor shows the layout as it is now"
        );
    }
    assert_eq!(
        draft_ids(&site).await,
        [
            "hot",
            "member-card",
            "latest",
            "recent-local",
            "site-overview",
            "friend-links"
        ]
    );
    let draft: serde_json::Value =
        serde_json::from_str(&draft_body(&site, "site").await.unwrap()).unwrap();
    assert!(draft.get("home").is_none());
    assert_eq!(
        site.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='layout.save'")
            .await,
        0,
        "nothing was published"
    );
    // The add form carries the revision of the editor page it was opened from.
    let form = site
        .get(
            &format!("/manage/layout/add?domain=0&place={PLACE}&source=markdown&revision={old}"),
            Some(&site.admin),
        )
        .await;
    assert_eq!(hidden(&form.body, "revision"), Some(old.as_str()));
    // The page names blocks by id and carries its revision in the forms that change the layout.
    assert!(!old.is_empty());
    assert!(
        left_open
            .body
            .contains("name=\"block\" value=\"member-card\"")
    );
    assert!(
        left_open
            .body
            .contains(&format!("name=\"revision\" value=\"{old}\""))
    );
    let added = site
        .post(
            "/manage/layout/add",
            &site.admin,
            &[
                ("domain", "0"),
                ("place", PLACE),
                ("source", "markdown"),
                ("body_en", "Hello"),
                ("revision", &old),
            ],
        )
        .await;
    assert_eq!(added.status, StatusCode::CONFLICT, "{}", added.body);
    // The first block cannot move up: nothing changes.
    let before = draft_body(&site, "site").await;
    let first = edit(
        &site,
        "/manage/layout/move",
        &[
            ("domain", "0"),
            ("place", PLACE),
            ("block", "hot"),
            ("direction", "up"),
        ],
    )
    .await;
    assert_eq!(first.status, StatusCode::SEE_OTHER);
    assert_eq!(draft_body(&site, "site").await, before);
    // With the current revision a block id the place does not list is refused the same way.
    let missing = edit(
        &site,
        "/manage/layout/remove",
        &[
            ("domain", "0"),
            ("place", PLACE),
            ("block", "no-such-block"),
        ],
    )
    .await;
    assert_eq!(missing.status, StatusCode::CONFLICT);
    assert_eq!(draft_ids(&site).await.len(), 6);
    site.services.cleanup().await.unwrap();
}

/// A POST of the administrator with an extra cookie (the preview's).
async fn post_with_cookie(
    site: &Site,
    path: &str,
    cookie: &str,
    fields: &[(&str, &str)],
) -> site::Answer {
    use gono_forum_web::test_support::{TEST_SITE_URL, host_of};
    let mut all = vec![("_csrf", site.admin.1.as_str())];
    all.extend_from_slice(fields);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(all)
        .finish();
    site.send(
        axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header(header::HOST, host_of(TEST_SITE_URL))
            .header(header::ORIGIN, TEST_SITE_URL)
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}; {cookie}", site.admin.0),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body))
            .unwrap(),
    )
    .await
}

/// Sends `publish` while another administrator's save of the site's draft is under way: a transaction that holds the
/// document's rows (on SQLite the write lock) has written `newer` as the draft and commits shortly after the publish
/// started. Returns the publish's answer.
async fn publish_during_a_save(
    site: &Site,
    newer: &str,
    publish: impl std::future::Future<Output = site::Answer>,
) -> site::Answer {
    use gono_forum_web::db;
    let database = site.db();
    let mut save = database.begin_write().await.unwrap();
    db::query("SELECT skey FROM gonof_common_settings WHERE skey IN (?,?,?) FOR UPDATE")
        .bind("settings.layout.body")
        .bind("settings.layout.title")
        .bind("settings.layout.updated_at")
        .fetch_all(database.observe(&mut *save))
        .await
        .unwrap();
    db::query(
        "UPDATE gonof_common_settings SET svalue=? WHERE skey='settings.layout_draft.site.body'",
    )
    .bind(newer)
    .execute(database.observe(&mut *save))
    .await
    .unwrap();
    let (answer, ()) = tokio::join!(publish, async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        save.commit().await.unwrap();
    });
    answer
}

/// The site's draft with the first block of `PLACE` left out.
async fn newer_draft(site: &Site) -> String {
    let mut draft: serde_json::Value =
        serde_json::from_str(&draft_body(site, "site").await.unwrap()).unwrap();
    draft["places"][PLACE].as_array_mut().unwrap().remove(0);
    draft.to_string()
}

#[tokio::test]
async fn publishing_keeps_a_draft_saved_while_it_runs() {
    let site = Site::new(true).await;
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "0"), ("place", PLACE)],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    // The editor's Publish, from the page that showed the draft before the other save: refused, the newer draft stays
    // and nothing goes live.
    let shown = revision(&site, "0").await;
    let newer = newer_draft(&site).await;
    let published = publish_during_a_save(
        &site,
        &newer,
        site.post(
            "/manage/layout/publish",
            &site.admin,
            &[("domain", "0"), ("revision", &shown)],
        ),
    )
    .await;
    assert_eq!(published.status, StatusCode::CONFLICT, "{}", published.body);
    assert_eq!(draft_body(&site, "site").await.as_deref(), Some(&*newer));
    let document = site
        .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
        .await;
    assert!(!document.contains(PLACE), "{document}");
    assert_eq!(
        site.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='layout.save'")
            .await,
        0
    );

    // The preview bar's Publish takes the same path and publishes the draft as it is once the save is in: the newer
    // one, which goes live and is not left behind as a deleted draft.
    let preview = site
        .post("/manage/layout/preview", &site.admin, &[("domain", "0")])
        .await;
    let sealed = cookie(&preview);
    assert!(!sealed.is_empty());
    let newest = newer_draft(&site).await;
    let published = publish_during_a_save(
        &site,
        &newest,
        post_with_cookie(&site, "/manage/skins/preview/publish", &sealed, &[]),
    )
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
    assert!(draft_body(&site, "site").await.is_none());
    let document: serde_json::Value = serde_json::from_str(
        &site
            .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
            .await,
    )
    .unwrap();
    let draft: serde_json::Value = serde_json::from_str(&newest).unwrap();
    assert_eq!(
        document["places"], draft["places"],
        "the newest draft is live"
    );
    assert_eq!(
        site.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='layout.save'")
            .await,
        1
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn publishing_a_previewed_skin_asks_first_about_a_changed_layout() {
    let site = Site::new(true).await;
    let layout = || async {
        let body = site
            .text("SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'")
            .await;
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    // Atrium, with the site's own side navigation.
    let chosen = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "atrium:main")],
        )
        .await;
    assert_eq!(chosen.status, StatusCode::SEE_OTHER, "{}", chosen.body);
    let own = edit(
        &site,
        "/manage/layout/own",
        &[("domain", "0"), ("place", "side.nav")],
    )
    .await;
    assert_eq!(own.status, StatusCode::SEE_OTHER, "{}", own.body);
    let published = edit(&site, "/manage/layout/publish", &[("domain", "0")]).await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );
    let selects = "SELECT COUNT(*) FROM gonof_common_log WHERE type='skin.select'";
    assert_eq!(site.scalar(selects).await, 1);

    // Publishing a preview of the default skin from the preview bar asks, as choosing it directly does, and lists
    // the side navigation blocks the default skin would not show.
    let preview = site
        .post(
            "/manage/skins/preview",
            &site.admin,
            &[("choice", "default:")],
        )
        .await;
    let sealed = cookie(&preview);
    assert!(!sealed.is_empty());
    let asked = post_with_cookie(&site, "/manage/skins/preview/publish", &sealed, &[]).await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert!(asked.body.contains("data-layout-switch"), "{}", asked.body);
    assert!(
        asked.body.contains("<code>side-boards</code>"),
        "{}",
        asked.body
    );
    assert!(
        asked
            .body
            .contains("action=\"/manage/skins/preview/publish\"")
    );
    assert!(
        asked
            .body
            .contains("name=\"choice\" value=\"default:default\"")
    );
    assert_eq!(site.scalar(selects).await, 1, "nothing is published yet");
    assert_eq!(layout().await["skin"], "atrium");
    // An answer given about another skin than the one previewed is asked again.
    let other = post_with_cookie(
        &site,
        "/manage/skins/preview/publish",
        &sealed,
        &[("choice", "ledger:blue"), ("layout", "skin")],
    )
    .await;
    assert_eq!(other.status, StatusCode::OK, "{}", other.body);
    assert!(other.body.contains("data-layout-switch"));
    assert_eq!(layout().await["skin"], "atrium");

    // Keeping the layout publishes the skin with the site's places and ends the preview.
    let kept = post_with_cookie(
        &site,
        "/manage/skins/preview/publish",
        &sealed,
        &[("choice", "default:default"), ("layout", "keep")],
    )
    .await;
    assert_eq!(kept.status, StatusCode::SEE_OTHER, "{}", kept.body);
    assert_eq!(
        kept.header(header::LOCATION),
        "/manage/skins?saved=selected"
    );
    assert!(cookie(&kept).ends_with("gonof_skin_preview__port58833="));
    let kept = layout().await;
    assert_eq!(kept["skin"], "default");
    assert!(kept["places"]["side.nav"].is_array(), "{kept}");

    // Taking the new skin's layout clears the places after choosing the skin.
    let preview = site
        .post(
            "/manage/skins/preview",
            &site.admin,
            &[("choice", "atrium:")],
        )
        .await;
    let sealed = cookie(&preview);
    let reset = post_with_cookie(
        &site,
        "/manage/skins/preview/publish",
        &sealed,
        &[("choice", "atrium:main"), ("layout", "skin")],
    )
    .await;
    assert_eq!(reset.status, StatusCode::SEE_OTHER, "{}", reset.body);
    let reset = layout().await;
    assert_eq!(reset["skin"], "atrium");
    assert!(reset["places"].is_null(), "{reset}");
    assert_eq!(
        site.text("SELECT type FROM gonof_common_log WHERE type IN ('skin.select','layout.save') ORDER BY id DESC LIMIT 1")
            .await,
        "layout.save"
    );
    site.services.cleanup().await.unwrap();
}
