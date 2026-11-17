//! Board colors and glyphs (docs/skins.md, "Board colors and glyphs"): the forum form saves them, pages link the
//! board stylesheet only while a board has one, and `/boards.css` serves the variables without a session.
#[path = "support/site.rs"]
mod site;

use axum::http::{StatusCode, header};
use site::Site;

/// The forum form's fields for an existing board, with `extra` replacing or adding fields.
async fn form(
    site: &Site,
    fid: i64,
    extra: &[(&'static str, &str)],
) -> Vec<(&'static str, String)> {
    let row = gono_forum_web::db::query("SELECT * FROM gonof_forum_forum WHERE fid=?")
        .bind(fid)
        .fetch_one(site.db().executor())
        .await
        .unwrap();
    let text = |column: &str| gono_forum_web::db::text(&row, column).unwrap();
    let number = |column: &str| row.try_get::<i64, _>(column).unwrap().to_string();
    let mut fields = vec![
        ("fid", fid.to_string()),
        (
            "parent_fid",
            row.try_get::<Option<i64>, _>("parent_fid")
                .unwrap()
                .unwrap_or(0)
                .to_string(),
        ),
        ("icon_aid", "0".to_owned()),
        ("name", text("name")),
        ("slug", text("slug")),
        ("description", text("description")),
        ("guidance", text("guidance")),
        ("displayorder", number("displayorder")),
        ("status", number("status")),
        ("visibility", text("visibility")),
        ("version", number("version")),
    ];
    for (name, value) in extra {
        fields.retain(|(field, _)| field != name);
        fields.push((name, (*value).to_owned()));
    }
    fields
}

async fn save(site: &Site, fid: i64, extra: &[(&'static str, &str)]) -> site::Answer {
    let fields = form(site, fid, extra).await;
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    site.post("/manage/forums", &site.admin, &pairs).await
}

/// The board stylesheet address a page links, if any.
fn linked(html: &str) -> Option<String> {
    let at = html.find("data-board-style>")?;
    let tag = &html[html[..at].rfind("<link").unwrap()..at];
    let href = &tag[tag.find("href=\"").unwrap() + 6..];
    Some(href[..href.find('"').unwrap()].to_owned())
}

#[tokio::test]
async fn a_board_color_and_glyph_reach_the_board_stylesheet() {
    let site = Site::new(false).await;
    let daily = site.fid("daily").await;
    let home = site.get("/", None).await;
    assert_eq!(
        linked(&home.body),
        None,
        "no board has a color or glyph yet"
    );

    // The form stores the color in lower case and the glyph by name.
    let saved = save(
        &site,
        daily,
        &[("color_on", "1"), ("color", "#0F766E"), ("glyph", "chat")],
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        site.text(&format!(
            "SELECT color FROM gonof_forum_forum WHERE fid={daily}"
        ))
        .await,
        "#0f766e"
    );
    assert_eq!(
        site.text(&format!(
            "SELECT glyph FROM gonof_forum_forum WHERE fid={daily}"
        ))
        .await,
        "chat"
    );
    let edit = site
        .get(&format!("/manage/forums?fid={daily}"), Some(&site.admin))
        .await;
    assert!(
        edit.body.contains("name=\"color_on\" value=\"1\" checked"),
        "the box is ticked"
    );
    assert!(edit.body.contains("value=\"#0f766e\""));
    assert!(edit.body.contains("name=\"glyph\" value=\"chat\" checked"));
    assert!(edit.body.contains("<symbol id=\"board-glyph-chat\""));

    let home = site.get("/", None).await;
    let href = linked(&home.body).expect("pages link the board stylesheet");
    assert!(href.starts_with("/boards.css?v="), "{href}");
    let member_home = site.get("/", Some(&site.member)).await;
    assert_eq!(linked(&member_home.body).as_deref(), Some(href.as_str()));

    let style = site.get(&href, None).await;
    assert_eq!(style.status, StatusCode::OK);
    assert_eq!(
        style.header(header::CONTENT_TYPE),
        "text/css; charset=utf-8"
    );
    assert_eq!(
        style.header(header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(style.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert!(
        style.header(header::SET_COOKIE).is_empty(),
        "no session cookie"
    );
    let rule = format!("[data-board=\"{daily}\"] {{ --board-color: #");
    assert!(style.body.contains(&rule), "{}", style.body);
    assert!(style.body.contains("--board-on-color: #"));
    assert!(
        style
            .body
            .contains("--board-glyph: url(\"data:image/svg+xml,")
    );
    assert!(style.body.contains(&format!(
        ":root[data-theme=\"dark\"] [data-board=\"{daily}\"]"
    )));
    // Another version gets the current stylesheet without being kept.
    let old = site.get("/boards.css?v=000000000000", None).await;
    assert_eq!(old.header(header::CACHE_CONTROL), "no-cache");
    assert_eq!(old.body, style.body);

    // Unknown glyphs and colors that are not #rrggbb are refused and change nothing.
    for extra in [
        &[("color_on", "1"), ("color", "teal"), ("glyph", "chat")][..],
        &[("color_on", "1"), ("color", "#0f766e"), ("glyph", "dragon")][..],
    ] {
        let refused = save(&site, daily, extra).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{extra:?}"
        );
        assert!(
            refused
                .body
                .contains(gono_forum_web::i18n::Lang::EN.forum_error_look()),
            "{extra:?}"
        );
    }
    assert_eq!(
        site.text(&format!(
            "SELECT color FROM gonof_forum_forum WHERE fid={daily}"
        ))
        .await,
        "#0f766e"
    );

    // Unticking the box clears the color; with the glyph gone too, pages link nothing again.
    let cleared = save(&site, daily, &[("color", "#0f766e"), ("glyph", "")]).await;
    assert_eq!(cleared.status, StatusCode::SEE_OTHER, "{}", cleared.body);
    assert_eq!(
        site.text(&format!(
            "SELECT color FROM gonof_forum_forum WHERE fid={daily}"
        ))
        .await,
        ""
    );
    let home = site.get("/", None).await;
    assert_eq!(linked(&home.body), None);
    let empty = site.get(&href, None).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.body, "");
    assert_eq!(empty.header(header::CACHE_CONTROL), "no-cache");
    site.services.cleanup().await.unwrap();
}
