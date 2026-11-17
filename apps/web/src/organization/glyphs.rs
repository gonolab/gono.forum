//! The built-in board glyphs (docs/skins.md, "Board colors and glyphs"): 32 icons drawn for the project on the
//! 24 × 24 grid of docs/styles.md, "Icons" (round caps and joins, stroke width 1.8, `currentColor`). A board's `glyph`
//! column is empty or one of these names. The same bodies feed the `<symbol>`s of `partials/board_glyphs.html` and the
//! `--board-glyph` image of the board stylesheet ([`super::style`]), so the two never differ. Listed in
//! docs/assets.md.

/// One glyph: its stored name and the SVG elements inside `<svg viewBox="0 0 24 24">`.
pub struct Glyph {
    pub name: &'static str,
    pub body: &'static str,
}

impl Glyph {
    /// The glyph's name in the request's language, for the admin form and screen readers.
    pub fn label(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.name {
            "chat" => t.forum_glyph_chat(),
            "megaphone" => t.forum_glyph_megaphone(),
            "question" => t.forum_glyph_question(),
            "lightbulb" => t.forum_glyph_lightbulb(),
            "bug" => t.forum_glyph_bug(),
            "wrench" => t.forum_glyph_wrench(),
            "code" => t.forum_glyph_code(),
            "book" => t.forum_glyph_book(),
            "graduation" => t.forum_glyph_graduation(),
            "newspaper" => t.forum_glyph_newspaper(),
            "calendar" => t.forum_glyph_calendar(),
            "people" => t.forum_glyph_people(),
            "coffee" => t.forum_glyph_coffee(),
            "star" => t.forum_glyph_star(),
            "image" => t.forum_glyph_image(),
            "music" => t.forum_glyph_music(),
            "film" => t.forum_glyph_film(),
            "gamepad" => t.forum_glyph_gamepad(),
            "palette" => t.forum_glyph_palette(),
            "briefcase" => t.forum_glyph_briefcase(),
            "tag" => t.forum_glyph_tag(),
            "globe" => t.forum_glyph_globe(),
            "pin" => t.forum_glyph_pin(),
            "archive" => t.forum_glyph_archive(),
            "heart" => t.forum_glyph_heart(),
            "home" => t.forum_glyph_home(),
            "flag" => t.forum_glyph_flag(),
            "shield" => t.forum_glyph_shield(),
            "leaf" => t.forum_glyph_leaf(),
            "rocket" => t.forum_glyph_rocket(),
            "trophy" => t.forum_glyph_trophy(),
            "lock" => t.forum_glyph_lock(),
            // Every glyph has its message above; a test keeps the list complete.
            other => other,
        }
    }
}

/// Every built-in glyph, in the order the admin form offers them.
pub const GLYPHS: &[Glyph] = &[
    Glyph {
        name: "chat",
        body: r#"<path d="M5.5 4h13A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-4.5 4v-4A1.5 1.5 0 0 1 4 14.5v-9A1.5 1.5 0 0 1 5.5 4Z"/><path d="M8 9h8M8 12h5"/>"#,
    },
    Glyph {
        name: "megaphone",
        body: r#"<path d="M4 10v4a1 1 0 0 0 1 1h2l8 4V5L7 9H5a1 1 0 0 0-1 1Z"/><path d="m7 15 1 5h2l-1-4.5"/><path d="M18.5 9.5a3.5 3.5 0 0 1 0 5"/>"#,
    },
    Glyph {
        name: "question",
        body: r#"<circle cx="12" cy="12" r="8.5"/><path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .8-1 1.5v.7"/><path d="M12 17h.01"/>"#,
    },
    Glyph {
        name: "lightbulb",
        body: r#"<path d="M9 18h6M10 21h4"/><path d="M12 3a6 6 0 0 0-3.5 10.9c.6.4 1 1.1 1 1.9v.2h5v-.2c0-.8.4-1.5 1-1.9A6 6 0 0 0 12 3Z"/>"#,
    },
    Glyph {
        name: "bug",
        body: r#"<rect x="7" y="8" width="10" height="12" rx="5"/><path d="M12 11v9M9.5 5l1.5 2.5M14.5 5 13 7.5M4 13h3M17 13h3M5 18l2.3-1.2M19 18l-2.3-1.2M5 8l2.3 1.2M19 8l-2.3 1.2"/>"#,
    },
    Glyph {
        name: "wrench",
        body: r#"<path d="M14.5 4.2a4.5 4.5 0 0 0-5.7 5.9L4.3 14.6a2 2 0 0 0 2.8 2.8l4.5-4.5a4.5 4.5 0 0 0 5.9-5.7l-2.6 2.6-2.4-.6-.6-2.4 2.6-2.6Z"/>"#,
    },
    Glyph {
        name: "code",
        body: r#"<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 5l-3 14"/>"#,
    },
    Glyph {
        name: "book",
        body: r#"<path d="M12 6.5C10.2 5.2 7.5 4.5 4 4.5v13.5c3.5 0 6.2.7 8 2 1.8-1.3 4.5-2 8-2V4.5c-3.5 0-6.2.7-8 2Z"/><path d="M12 6.5V20"/>"#,
    },
    Glyph {
        name: "graduation",
        body: r#"<path d="m3 9 9-4 9 4-9 4-9-4Z"/><path d="M7 11v4.5c0 1.4 2.2 2.5 5 2.5s5-1.1 5-2.5V11"/><path d="M21 9v5"/>"#,
    },
    Glyph {
        name: "newspaper",
        body: r#"<path d="M4 5h12v13.5a1.5 1.5 0 0 0 1.5 1.5H6a2 2 0 0 1-2-2V5Z"/><path d="M16 9h3.5v9.5a1.5 1.5 0 0 1-3 0"/><path d="M7.5 9h5M7.5 12.5h5M7.5 16h3"/>"#,
    },
    Glyph {
        name: "calendar",
        body: r#"<rect x="4" y="5.5" width="16" height="14.5" rx="2"/><path d="M4 10h16M8.5 3.5v4M15.5 3.5v4"/>"#,
    },
    Glyph {
        name: "people",
        body: r#"<circle cx="9" cy="8.5" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0"/><path d="M16 5a3.5 3.5 0 0 1 0 7M18 14.2a6.5 6.5 0 0 1 3.5 5.8"/>"#,
    },
    Glyph {
        name: "coffee",
        body: r#"<path d="M4.5 9.5h11V14a5 5 0 0 1-5 5h-1a5 5 0 0 1-5-5V9.5Z"/><path d="M15.5 11h1.5a2.5 2.5 0 0 1 0 5h-1.8M8 3.5v3M11.5 3.5v3"/>"#,
    },
    Glyph {
        name: "star",
        body: r#"<path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.3-4.1 5.9-.9L12 3.5Z"/>"#,
    },
    Glyph {
        name: "image",
        body: r#"<rect x="3.5" y="5" width="17" height="14" rx="2"/><circle cx="9" cy="10" r="1.5"/><path d="m20.5 16-5-5-9 8"/>"#,
    },
    Glyph {
        name: "music",
        body: r#"<path d="M9 18V6l11-2v12"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="17.5" cy="16" r="2.5"/>"#,
    },
    Glyph {
        name: "film",
        body: r#"<rect x="3.5" y="5" width="17" height="14" rx="2"/><path d="M7.5 5v14M16.5 5v14M3.5 9.5h4M3.5 14.5h4M16.5 9.5h4M16.5 14.5h4"/>"#,
    },
    Glyph {
        name: "gamepad",
        body: r#"<path d="M7.5 7h9a4.5 4.5 0 0 1 4.4 5.4l-.8 4a2.5 2.5 0 0 1-4.3 1.2L14 15.5h-4l-1.8 2.1a2.5 2.5 0 0 1-4.3-1.2l-.8-4A4.5 4.5 0 0 1 7.5 7Z"/><path d="M8 10v3M6.5 11.5h3M15.5 10.5h.01M17.5 12.5h.01"/>"#,
    },
    Glyph {
        name: "palette",
        body: r#"<path d="M12 3.5a8.5 8.5 0 1 0 0 17c1.1 0 1.8-.9 1.5-1.9l-.4-1.2a1.6 1.6 0 0 1 1.5-2.1H17a3.5 3.5 0 0 0 3.5-3.5C20.5 7 16.7 3.5 12 3.5Z"/><circle cx="7.5" cy="11.5" r="1"/><circle cx="10" cy="7.5" r="1"/><circle cx="14.5" cy="7.5" r="1"/>"#,
    },
    Glyph {
        name: "briefcase",
        body: r#"<rect x="3.5" y="7" width="17" height="12.5" rx="2"/><path d="M9 7V5.5A1.5 1.5 0 0 1 10.5 4h3A1.5 1.5 0 0 1 15 5.5V7M3.5 12.5h17"/>"#,
    },
    Glyph {
        name: "tag",
        body: r#"<path d="M3.5 12.6V4.5a1 1 0 0 1 1-1h8.1a1 1 0 0 1 .7.3l7.2 7.2a1 1 0 0 1 0 1.4l-8.1 8.1a1 1 0 0 1-1.4 0l-7.2-7.2a1 1 0 0 1-.3-.7Z"/><circle cx="8.5" cy="8.5" r="1.5"/>"#,
    },
    Glyph {
        name: "globe",
        body: r#"<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.3 2.3 3.5 5.2 3.5 8.5s-1.2 6.2-3.5 8.5c-2.3-2.3-3.5-5.2-3.5-8.5S9.7 5.8 12 3.5Z"/>"#,
    },
    Glyph {
        name: "pin",
        body: r#"<path d="M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11Z"/><circle cx="12" cy="10" r="2.5"/>"#,
    },
    Glyph {
        name: "archive",
        body: r#"<rect x="3.5" y="4.5" width="17" height="4" rx="1"/><path d="M5 8.5V18a1.5 1.5 0 0 0 1.5 1.5h11A1.5 1.5 0 0 0 19 18V8.5M10 12h4"/>"#,
    },
    Glyph {
        name: "heart",
        body: r#"<path d="M12 20s-7.5-4.6-7.5-10A4.5 4.5 0 0 1 12 7a4.5 4.5 0 0 1 7.5 3c0 5.4-7.5 10-7.5 10Z"/>"#,
    },
    Glyph {
        name: "home",
        body: r#"<path d="M4 10.5 12 4l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5.5h-5V20H5a1 1 0 0 1-1-1v-8.5Z"/>"#,
    },
    Glyph {
        name: "flag",
        body: r#"<path d="M5 21V4"/><path d="M5 4h12l-2.5 4.5L17 13H5"/>"#,
    },
    Glyph {
        name: "shield",
        body: r#"<path d="M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6l-7-3Z"/>"#,
    },
    Glyph {
        name: "leaf",
        body: r#"<path d="M5.5 19C5 10.5 10 5 20 4.5 19.5 14 14 19 5.5 19Z"/><path d="M5.5 19c3-4 6-6.5 9.5-8"/>"#,
    },
    Glyph {
        name: "rocket",
        body: r#"<path d="M14.5 4.5c2.5-1 4.5-1 5-.5s.5 2.5-.5 5L13 15.5 8.5 11l6-6.5Z"/><path d="M8.5 11 5 11.5 3.5 14l4 .5M13 15.5l-.5 3.5-2.5 1.5-.5-4"/><circle cx="15.5" cy="8.5" r="1.5"/>"#,
    },
    Glyph {
        name: "trophy",
        body: r#"<path d="M8 4h8v6a4 4 0 0 1-8 0V4Z"/><path d="M8 6H5a3 3 0 0 0 3 4M16 6h3a3 3 0 0 1-3 4M12 14v4M8.5 20h7"/>"#,
    },
    Glyph {
        name: "lock",
        body: r#"<rect x="5" y="10.5" width="14" height="9.5" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>"#,
    },
];

/// The built-in glyph with this name.
pub fn find(name: &str) -> Option<&'static Glyph> {
    GLYPHS.iter().find(|glyph| glyph.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names_fit_the_column_and_are_unique() {
        assert_eq!(GLYPHS.len(), 32);
        let mut names: Vec<_> = GLYPHS.iter().map(|glyph| glyph.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), GLYPHS.len(), "a glyph name repeats");
        for glyph in GLYPHS {
            assert!(
                !glyph.name.is_empty()
                    && glyph.name.len() <= 32
                    && glyph.name.bytes().all(|b| b.is_ascii_lowercase()),
                "{}",
                glyph.name
            );
            assert_eq!(find(glyph.name).map(|found| found.name), Some(glyph.name));
            assert_ne!(glyph.label(), glyph.name, "{} has a message", glyph.name);
            // Plain shapes only: no attributes that would change the shared stroke style, no text, no quotes that the
            // stylesheet's data address would have to escape differently.
            for banned in ["stroke", "fill", "style", "<text", "'", "href", "<script"] {
                assert!(!glyph.body.contains(banned), "{}: {banned}", glyph.name);
            }
        }
        assert!(find("unknown").is_none());
    }
}
