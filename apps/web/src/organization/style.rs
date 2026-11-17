//! The board stylesheet (docs/skins.md, "Board colors and glyphs"), served at `/boards.css?k=<skin>&v=<hash>`: one
//! rule per board that has a color or a glyph, on the `data-board="<fid>"` hook list rows, board blocks and the board
//! head carry. It is compiled for the skin a page uses, colored variants included (`k` is the skin stylesheet's hash),
//! because the board colors are fitted to that skin's grounds.
//!
//! - `--board-color` is the board's color, moved in lightness only as far as needed for 3:1 against the skin's
//!   `--panel` and `--bg` of the palette and mode, so it works as a fill, a line or an accent on cards and on the page.
//! - `--board-on-color` is the text to write on it: white or the light mode's ink, whichever needs the smaller move of
//!   `--board-color` to reach 4.5:1.
//! - `--board-glyph` is the board's glyph as an SVG image (black strokes), for `mask-image`; the markup draws the
//!   same glyph from `partials/board_glyphs.html`.
//!
//! Light values sit on the board's rule; dark values under `[data-theme="dark"]` and again inside
//! `@media (prefers-color-scheme: dark)` when `data-theme` is neither, like the skin palettes. A skin with several
//! palettes gets the colors of each under `:root[data-palette="<name>"]`. Everything is in the `core` cascade layer,
//! so skins and personal styles override it.
use sha2::{Digest, Sha256};

use super::ForumRecord;
use crate::skins::color::{self, Rgba};
use crate::skins::palette::Tokens;
use crate::skins::seed;

/// The compiled board stylesheet; empty when no board has a color or a glyph, and pages then link nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardStyle {
    pub css: String,
    /// The first 12 hex digits of the stylesheet's SHA-256, the `?v=` of its address.
    pub version: String,
    /// The `version_hash` of the skin (or colored variant) it was compiled for, the `?k=` of its address.
    pub skin: String,
}

impl BoardStyle {
    pub fn is_empty(&self) -> bool {
        self.css.is_empty()
    }

    /// The address pages link, without the path prefix.
    pub fn href(&self) -> String {
        format!("/boards.css?k={}&v={}", self.skin, self.version)
    }
}

/// The two shades of a board color in one mode.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shades {
    fill: Rgba,
    on: Rgba,
}

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Contrast of a board color as a fill or line on cards and the page, and of the text written on it.
const FILL_MIN: f64 = 3.0;
const TEXT_MIN: f64 = 4.5;

/// What a palette offers a board color: the grounds of each mode it must stand out from, and the light mode's ink
/// for text on it.
struct Grounds {
    light: Vec<Rgba>,
    dark: Vec<Rgba>,
    ink: Rgba,
}

impl Grounds {
    fn of(light: &Tokens, dark: &Tokens) -> Self {
        let pick = |tokens: &Tokens, dark: bool| {
            ["--panel", "--bg"]
                .into_iter()
                .filter_map(|name| seed::painted(tokens, name, dark))
                .collect()
        };
        Self {
            light: pick(light, false),
            dark: pick(dark, true),
            ink: seed::painted(light, "--text-strong", false).unwrap_or(Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
        }
    }

    fn mode(&self, dark: bool) -> &[Rgba] {
        if dark { &self.dark } else { &self.light }
    }
}

fn shades(color: Rgba, palette: &Grounds, dark: bool) -> Shades {
    let grounds = palette.mode(dark);
    let ink = palette.ink;
    let start = color.oklch().l;
    let mut best: Option<(f64, Shades)> = None;
    for on in [WHITE, ink] {
        let fill = seed::lift(seed::lift(color, grounds, FILL_MIN), &[on], TEXT_MIN);
        let passes = grounds
            .iter()
            .all(|ground| color::contrast(fill, *ground) + 1e-9 >= FILL_MIN)
            && color::contrast(fill, on) + 1e-9 >= TEXT_MIN;
        if !passes {
            continue;
        }
        let distance = (fill.oklch().l - start).abs();
        if best.is_none_or(|(seen, _)| distance < seen) {
            best = Some((distance, Shades { fill, on }));
        }
    }
    best.map(|(_, shades)| shades).unwrap_or_else(|| {
        let fill = seed::lift(color, grounds, FILL_MIN);
        let on = if color::contrast(fill, WHITE) >= color::contrast(fill, ink) {
            WHITE
        } else {
            ink
        };
        Shades { fill, on }
    })
}

/// The glyph as a `data:` image for `url()`: double quotes become single ones and `<`, `>`, `#` and `%` are
/// percent-encoded, so the address needs no other escaping inside `url("…")`.
fn glyph_image(body: &str) -> String {
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='#000' stroke-width='1.8' stroke-linecap='round' stroke-linejoin='round'>{}</svg>",
        body.replace('"', "'")
    );
    let mut encoded = String::with_capacity(svg.len() + 64);
    for ch in svg.chars() {
        match ch {
            '<' => encoded.push_str("%3C"),
            '>' => encoded.push_str("%3E"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            other => encoded.push(other),
        }
    }
    format!("url(\"data:image/svg+xml,{encoded}\")")
}

fn color_rules(fill: Rgba, on: Rgba) -> String {
    format!(
        "--board-color: {}; --board-on-color: {};",
        fill.rounded().hex(),
        on.rounded().hex()
    )
}

/// The stylesheet for these boards, by `fid`, fitted to the palettes of `skin`. A color that is not `#rrggbb` or an
/// unknown glyph is skipped: saves check both, so only a row edited by hand can hold one.
pub fn compile(forums: &[ForumRecord], skin: &crate::skins::Skin) -> BoardStyle {
    let mut boards: Vec<&ForumRecord> = forums
        .iter()
        .filter(|forum| !forum.color.is_empty() || !forum.glyph.is_empty())
        .collect();
    boards.sort_by_key(|forum| forum.fid);
    // One set of grounds per palette; a skin with one palette needs no palette in its selectors.
    let palettes: Vec<(&str, Grounds)> = skin
        .palettes
        .iter()
        .map(|name| {
            let merged = skin.merged(name);
            (name.as_str(), Grounds::of(&merged.light, &merged.dark))
        })
        .collect();
    let qualified = palettes.len() > 1;
    let mut light = String::new();
    let mut dark = String::new();
    for forum in boards {
        let selector = format!("[data-board=\"{}\"]", forum.fid);
        let color = seed::parse_hex(&forum.color).map(|[r, g, b]| Rgba {
            r: f64::from(r) / 255.0,
            g: f64::from(g) / 255.0,
            b: f64::from(b) / 255.0,
            a: 1.0,
        });
        let glyph = super::glyphs::find(&forum.glyph);
        let mut own = Vec::new();
        if let Some(glyph) = glyph {
            own.push(format!("--board-glyph: {};", glyph_image(glyph.body)));
        }
        if let Some(color) = color {
            for (name, grounds) in &palettes {
                let palette = if qualified {
                    format!("[data-palette=\"{name}\"]")
                } else {
                    String::new()
                };
                let day = shades(color, grounds, false);
                let rules = color_rules(day.fill, day.on);
                if qualified {
                    light.push_str(&format!(":root{palette} {selector} {{ {rules} }}\n"));
                } else {
                    own.insert(0, rules);
                }
                let night = shades(color, grounds, true);
                dark.push_str(&format!(
                    "{{dark{palette}}} {selector} {{ {} }}\n",
                    color_rules(night.fill, night.on)
                ));
            }
        }
        if !own.is_empty() {
            light.push_str(&format!("{selector} {{ {} }}\n", own.join(" ")));
        }
    }
    if light.is_empty() {
        return BoardStyle {
            skin: skin.version_hash.clone(),
            ..BoardStyle::default()
        };
    }
    let mut css = String::from("@layer core {\n");
    css.push_str(&light);
    let roots = |root: &dyn Fn(&str) -> String| {
        let mut out = dark.clone();
        for (name, _) in &palettes {
            let palette = if qualified {
                format!("[data-palette=\"{name}\"]")
            } else {
                String::new()
            };
            out = out.replace(&format!("{{dark{palette}}}"), &root(&palette));
        }
        out
    };
    css.push_str(&roots(&|palette| {
        format!(":root{palette}[data-theme=\"dark\"]")
    }));
    if !dark.is_empty() {
        css.push_str("@media (prefers-color-scheme: dark) {\n");
        css.push_str(&roots(&|palette| {
            format!(":root{palette}:not([data-theme=\"light\"]):not([data-theme=\"dark\"])")
        }));
        css.push_str("}\n");
    }
    css.push_str("}\n");
    let digest = Sha256::digest(css.as_bytes());
    let version = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    BoardStyle {
        css,
        version,
        skin: skin.version_hash.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(fid: i64, color: &str, glyph: &str) -> ForumRecord {
        ForumRecord {
            fid,
            parent_fid: None,
            icon_aid: None,
            name: format!("Board {fid}"),
            slug: format!("board-{fid}"),
            description: String::new(),
            guidance: String::new(),
            displayorder: 0,
            status: 1,
            visibility: "public".into(),
            version: 1,
            color: color.into(),
            glyph: glyph.into(),
        }
    }

    fn compile(forums: &[ForumRecord]) -> BoardStyle {
        super::compile(forums, &crate::skins::DEFAULT_SKIN)
    }

    #[test]
    fn boards_without_color_or_glyph_write_nothing() {
        let style = compile(&[board(1, "", ""), board(2, "", "")]);
        assert!(style.is_empty());
        assert_eq!(style.css, "");
    }

    #[test]
    fn every_color_reaches_its_contrast_in_both_modes() {
        // Light, dark, saturated and grey colors.
        for hex in [
            "#ffffff", "#000000", "#ffff00", "#0f766e", "#e11d48", "#7c3aed", "#94a3b8", "#22c55e",
        ] {
            let [r, g, b] = seed::parse_hex(hex).unwrap();
            let color = Rgba {
                r: f64::from(r) / 255.0,
                g: f64::from(g) / 255.0,
                b: f64::from(b) / 255.0,
                a: 1.0,
            };
            let base = &crate::skins::palette::BASE;
            let palette = Grounds::of(&base.light, &base.dark);
            for dark in [false, true] {
                let Shades { fill, on } = shades(color, &palette, dark);
                let (fill, on) = (fill.rounded(), on.rounded());
                for &ground in palette.mode(dark) {
                    assert!(
                        color::contrast(fill, ground) >= FILL_MIN,
                        "{hex} dark={dark}: {} on {}",
                        fill.hex(),
                        ground.hex()
                    );
                }
                assert!(
                    color::contrast(on, fill) >= TEXT_MIN,
                    "{hex} dark={dark}: {} on {}",
                    on.hex(),
                    fill.hex()
                );
            }
        }
    }

    #[test]
    fn the_stylesheet_lists_boards_by_fid_in_the_core_layer() {
        let style = compile(&[
            board(9, "#0f766e", "chat"),
            board(3, "", "code"),
            board(5, "#e11d48", ""),
            board(7, "nonsense", "unknown"),
        ]);
        let css = &style.css;
        assert!(css.starts_with("@layer core {\n"), "{css}");
        assert!(css.ends_with("}\n"));
        let three = css.find("[data-board=\"3\"] {").unwrap();
        let five = css.find("[data-board=\"5\"] {").unwrap();
        let nine = css.find("[data-board=\"9\"] {").unwrap();
        assert!(three < five && five < nine);
        assert!(
            !css.contains("[data-board=\"7\"]"),
            "invalid values write nothing"
        );
        assert!(css.contains(":root[data-theme=\"dark\"] [data-board=\"9\"] { --board-color: #"));
        assert!(css.contains(
            "@media (prefers-color-scheme: dark) {\n:root:not([data-theme=\"light\"]):not([data-theme=\"dark\"]) [data-board=\"5\"]"
        ));
        assert!(css.contains(
            "--board-glyph: url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'"
        ));
        // The data address never closes the url() early or starts another declaration.
        for line in css.lines().filter(|line| line.contains("--board-glyph")) {
            let image = &line[line.find("url(\"").unwrap() + 5..];
            let image = &image[..image.find("\")").unwrap()];
            assert!(
                !image.contains(['"', '<', '>', '#', ';', '{', '}']),
                "{image}"
            );
        }
        assert_eq!(style.version.len(), 12);
        // The order boards come in does not change the stylesheet or its address.
        let shuffled = compile(&[
            board(5, "#e11d48", ""),
            board(7, "nonsense", "unknown"),
            board(3, "", "code"),
            board(9, "#0f766e", "chat"),
        ]);
        assert_eq!(shuffled, style);
        assert_eq!(
            style.href(),
            format!(
                "/boards.css?k={}&v={}",
                crate::skins::DEFAULT_SKIN.version_hash,
                style.version
            )
        );
    }
}
