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
    Some(
        href[..href.find('"').unwrap()]
            .replace("&#38;", "&")
            .replace("&amp;", "&"),
    )
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
    assert!(href.starts_with("/boards.css?k="), "{href}");
    assert!(href.contains("&v="), "{href}");
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
    // Another version, or an address from before skins had their own, gets the current stylesheet without being
    // kept; a skin no page uses gets the built-in default's.
    let skin = &href[href.find("k=").unwrap() + 2..href.find("&v=").unwrap()];
    for other in [
        format!("/boards.css?k={skin}&v=000000000000"),
        "/boards.css?v=000000000000".to_owned(),
        "/boards.css?k=000000000000&v=000000000000".to_owned(),
    ] {
        let old = site.get(&other, None).await;
        assert_eq!(old.header(header::CACHE_CONTROL), "no-cache", "{other}");
        assert_eq!(old.body, style.body, "{other}");
    }

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

/// Writes the instance skin `id` with two palettes, one with dark grounds (the default) and one with light ones, and
/// makes it the site's skin.
async fn grounded_skin(site: &Site, id: &str) {
    let dir = site.services.root.join(format!("skins/{id}"));
    std::fs::create_dir_all(dir.join("palettes")).unwrap();
    // The night palette's text is light on its dark grounds, so the footer line keeps its contrast.
    std::fs::write(
        dir.join("palettes/night.json"),
        r##"{"shared": {}, "light": {"--panel": "#111827", "--bg": "#0b1120", "--text-strong": "#ffffff", "--text": "#f1f5f9", "--text-muted": "#cbd5e1"}, "dark": {}}"##,
    )
    .unwrap();
    std::fs::write(
        dir.join("palettes/day.json"),
        r##"{"shared": {}, "light": {"--panel": "#ffffff", "--bg": "#f8fafc"}, "dark": {}}"##,
    )
    .unwrap();
    std::fs::write(
        dir.join("skin.toml"),
        format!(
            "contract = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = {{ en = \"Grounds\" }}\n[palettes]\ndefault = \"night\"\nvariants = [\"day\"]\n"
        ),
    )
    .unwrap();
    gono_forum_web::test_support::set_setting(
        site.db(),
        "settings.layout.body",
        &format!("{{\"v\":1,\"skin\":\"{id}\"}}"),
    )
    .await
    .unwrap();
    site.services.global.refresh_all().await.unwrap();
}

#[tokio::test]
async fn board_colors_are_fitted_to_the_skin_in_use() {
    let site = Site::new(false).await;
    let daily = site.fid("daily").await;
    // A dark blue reads on the core scheme's white panels as it is.
    let saved = save(&site, daily, &[("color_on", "1"), ("color", "#1e3a8a")]).await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let core = site
        .get(&linked(&site.get("/", None).await.body).unwrap(), None)
        .await;
    let rule = |css: &str, prefix: &str| -> String {
        let start = css
            .find(&format!(
                "{prefix}[data-board=\"{daily}\"] {{ --board-color: "
            ))
            .unwrap_or_else(|| panic!("{prefix}: {css}"));
        let color = &css[start..];
        let color = &color[color.find("--board-color: ").unwrap() + 15..];
        color[..7].to_owned()
    };
    assert_eq!(rule(&core.body, ""), "#1e3a8a");

    // Under a skin whose default palette has dark panels it is lifted to stand out, per palette.
    grounded_skin(&site, "grounds").await;
    let home = site.get("/", None).await;
    assert!(home.body.contains("data-skin=\"grounds\""));
    let href = linked(&home.body).unwrap();
    assert_ne!(Some(href.clone()), linked(&core.body));
    let style = site.get(&href, None).await;
    assert_eq!(
        style.header(header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    let night = rule(&style.body, ":root[data-palette=\"night\"] ");
    let day = rule(&style.body, ":root[data-palette=\"day\"] ");
    assert_eq!(day, "#1e3a8a", "the day palette's white panels keep it");
    assert_ne!(night, "#1e3a8a", "the night palette's dark panels lift it");
    assert!(style.body.contains(&format!(
        ":root[data-palette=\"night\"][data-theme=\"dark\"] [data-board=\"{daily}\"]"
    )));

    // Previewed seed colors get their own board colors, served only privately.
    let preview = site
        .post(
            "/manage/appearance/colors/preview",
            &site.admin,
            &[("primary", "#be123c"), ("secondary", "#64748b")],
        )
        .await;
    assert_eq!(preview.status, StatusCode::SEE_OTHER, "{}", preview.body);
    let sealed = preview
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .find(|value| value.starts_with("gonof_skin_preview__port58833="))
        .map(|value| value[..value.find(';').unwrap()].to_owned())
        .unwrap();
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
    let own = linked(&previewed.body).unwrap();
    assert_ne!(own, href, "the colored variant has its own board colors");
    let private = site.get(&own, None).await;
    assert_eq!(
        private.header(header::CACHE_CONTROL),
        "private, max-age=31536000, immutable"
    );
    assert!(private.body.contains(&format!("[data-board=\"{daily}\"]")));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn with_caching_off_a_skin_with_seed_colors_keeps_its_board_colors() {
    let site = Site::new(false).await;
    let daily = site.fid("daily").await;
    let saved = save(&site, daily, &[("color_on", "1"), ("color", "#1e3a8a")]).await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    // Harbor, with its three palettes, over seed colors: a colored variant that only the page frame compiles.
    gono_forum_web::test_support::set_setting(
        site.db(),
        "settings.layout.body",
        r##"{"v":1,"skin":"harbor","palette":"main","colors":{"primary":"#be123c","secondary":"#64748b"}}"##,
    )
    .await
    .unwrap();
    site.services.global.refresh_all().await.unwrap();
    let href = linked(&site.get("/", None).await.body).expect("the board stylesheet");
    let cached = site.get(&href, None).await;
    assert_eq!(
        cached.header(header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    for palette in ["main", "contrast", "colored"] {
        assert!(
            cached.body.contains(&format!(
                ":root[data-palette=\"{palette}\"] [data-board=\"{daily}\"]"
            )),
            "{palette}: {}",
            cached.body
        );
    }

    // With caching off every request loads the skins again, so the variant has not been compiled for it yet; the
    // page links the same address and the stylesheet is still the variant's, kept for a year.
    gono_forum_web::test_support::set_setting(site.db(), "settings.cache.ttl_seconds", "0")
        .await
        .unwrap();
    site.services.global.refresh_all().await.unwrap();
    for _ in 0..2 {
        let home = site.get("/", None).await;
        assert_eq!(linked(&home.body).as_deref(), Some(href.as_str()));
        let style = site.get(&href, None).await;
        assert_eq!(style.status, StatusCode::OK);
        assert_eq!(
            style.header(header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(style.body, cached.body);
    }
    site.services.cleanup().await.unwrap();
}
