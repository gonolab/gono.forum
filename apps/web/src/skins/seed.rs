//! Seed colors (docs/styles.md, "Seed colors"): an administrator picks a primary and a secondary color for the site
//! or a domain, and the color tokens of the skin's palettes are derived from them in light and dark mode, then raised
//! to the contrast the checks ask for. Only the two seeds are stored (`colors` in the layout document); the derived
//! palettes are compiled into a variant of the skin's stylesheet whenever it is built, so they follow the skin in use.
//!
//! - The **primary** seed drives every colored role: fills (main buttons, the brand block), the text on them, colored
//!   text (links, current items, info), the focus ring, avatars and the soft grounds behind colored text.
//! - The **secondary** seed only lends its hue, with a small share of its chroma, to the neutrals: grounds, panels,
//!   lines, body and muted text, secondary buttons and form fields.
//!
//! The derivation works on a merged palette (the skin's palette over the base scheme), so it keeps the skin's
//! structure: a token the skin writes as `var(--…)` or `transparent` is left alone and follows what it refers to; a
//! literal color takes its hue and chroma from its seed and its lightness from the skin's own value in that mode. In
//! light mode the fills are the primary seed itself. Then every pair of [`pairs`] is raised to its contrast by moving
//! the text's lightness no further than needed, and the text on fills is white or the light mode's ink, whichever
//! stands out more.
use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::color::{self, Oklch, Rgba};
use super::palette::{self, Merged, Tokens};

/// The two seed colors, opaque sRGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Seeds {
    pub primary: [u8; 3],
    pub secondary: [u8; 3],
}

/// Parses `#rrggbb` in either case: what `<input type="color">` submits, and what a text field must hold where the
/// browser shows one instead.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// `#rrggbb` in lowercase.
pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn rgba(rgb: [u8; 3]) -> Rgba {
    Rgba {
        r: f64::from(rgb[0]) / 255.0,
        g: f64::from(rgb[1]) / 255.0,
        b: f64::from(rgb[2]) / 255.0,
        a: 1.0,
    }
}

impl Seeds {
    pub fn parse(primary: &str, secondary: &str) -> Option<Self> {
        Some(Self {
            primary: parse_hex(primary)?,
            secondary: parse_hex(secondary)?,
        })
    }

    /// The `c=` of a colored stylesheet's address: both seeds as twelve lowercase hex digits.
    pub fn code(&self) -> String {
        format!("{}{}", &hex(self.primary)[1..], &hex(self.secondary)[1..])
    }

    pub fn from_code(code: &str) -> Option<Self> {
        if code.len() != 12 || !code.is_ascii() {
            return None;
        }
        Self::parse(&format!("#{}", &code[..6]), &format!("#{}", &code[6..]))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedsJson {
    primary: String,
    secondary: String,
}

/// Stored as `{"primary":"#rrggbb","secondary":"#rrggbb"}`, lowercase.
impl Serialize for Seeds {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SeedsJson {
            primary: hex(self.primary),
            secondary: hex(self.secondary),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Seeds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let json = SeedsJson::deserialize(deserializer)?;
        Seeds::parse(&json.primary, &json.secondary)
            .ok_or_else(|| serde::de::Error::custom("seed colors are written #rrggbb"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seed {
    Primary,
    Secondary,
}

/// Fills: the primary seed itself in light mode, the seed at the skin's lightness in dark mode.
const FILLS: [&str; 4] = ["--primary-bg", "--primary-border", "--mark", "--brand-bg"];
/// Hovered fills and the fill they shift from.
const HOVERS: [(&str, &str); 3] = [
    ("--primary-hover", "--primary-bg"),
    ("--mark-deep", "--mark"),
    ("--brand-hover-bg", "--brand-bg"),
];
/// Text on fills, white or ink, and the fills it sits on.
const ON_FILLS: [(&str, [&str; 2]); 3] = [
    ("--primary-text", ["--primary-bg", "--primary-hover"]),
    ("--mark-text", ["--mark", "--mark-deep"]),
    ("--brand-text", ["--brand-bg", "--brand-hover-bg"]),
];
/// Colored text, and the focus ring and the accent of checkboxes, with the contrast each needs on the grounds.
const TEXTS: [(&str, f64); 9] = [
    ("--accent", 4.5),
    ("--link", 4.5),
    ("--mark-soft-text", 4.5),
    ("--info", 4.5),
    ("--info-soft-text", 4.5),
    ("--avatar-text", 4.5),
    ("--toolbar-selected-text", 4.5),
    ("--choice-accent", 3.0),
    ("--focus-ring", 3.0),
];
/// Hovered text and the text it shifts from.
const TEXT_HOVERS: [(&str, &str); 2] = [("--accent-hover", "--accent"), ("--link-hover", "--link")];
/// Soft grounds: tints of the primary seed behind colored text.
const SOFTS: [&str; 7] = [
    "--accent-soft",
    "--link-soft",
    "--mark-soft",
    "--info-soft",
    "--selection",
    "--avatar-bg",
    "--toolbar-selected-bg",
];
/// The grounds colored text is raised on.
const GROUNDS: [&str; 4] = ["--bg", "--panel", "--panel-alt", "--panel-hover"];

/// Neutral tokens and the share of the secondary seed's chroma (itself capped at [`NEUTRAL_CHROMA`]) they take:
/// grounds very little, lines and muted text more, so a vivid seed still gives quiet surfaces.
const NEUTRALS: [(&str, f64); 31] = [
    ("--bg", 0.06),
    ("--panel", 0.06),
    ("--panel-alt", 0.08),
    ("--panel-hover", 0.10),
    ("--input-bg", 0.06),
    ("--editor-bg", 0.06),
    ("--secondary-bg", 0.06),
    ("--secondary-hover", 0.10),
    ("--disabled-bg", 0.08),
    ("--code-inline-bg", 0.08),
    ("--code-block-bg", 0.08),
    ("--inverse-text", 0.06),
    ("--line", 0.12),
    ("--line-soft", 0.12),
    ("--secondary-border", 0.12),
    ("--disabled-border", 0.12),
    ("--quote-border", 0.12),
    ("--line-strong", 0.20),
    ("--accent-border", 0.20),
    ("--inverse-border", 0.20),
    ("--input-border", 0.20),
    ("--text-strong", 0.08),
    ("--text", 0.08),
    ("--inverse", 0.08),
    ("--secondary-text", 0.08),
    ("--code-block-text", 0.06),
    ("--text-muted", 0.22),
    ("--text-subtle", 0.22),
    ("--text-disabled", 0.18),
    ("--disabled-text", 0.18),
    ("--quote-text", 0.22),
];
const NEUTRAL_CHROMA: f64 = 0.16;

/// A token's value as a literal color, not a reference: `None` for `var()`, `transparent` and forms the contrast
/// checks do not understand, which the derivation leaves as they are.
fn literal(tokens: &Tokens, name: &str) -> Option<Rgba> {
    let value = tokens.get(name)?.trim();
    if value.starts_with("var(") || value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    color::parse(value)
}

/// The token whose literal value `name` resolves to through `var()` references.
fn holder<'a>(tokens: &'a Tokens, name: &'a str) -> Option<&'a str> {
    let mut name = name;
    for _ in 0..32 {
        let value = tokens.get(name)?.trim();
        match value
            .strip_prefix("var(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            Some(target) => {
                name = tokens
                    .0
                    .iter()
                    .find(|(key, _)| key == target.trim())?
                    .0
                    .as_str();
            }
            None => return Some(name),
        }
    }
    None
}

fn solid(value: f64) -> Rgba {
    Rgba {
        r: value,
        g: value,
        b: value,
        a: 1.0,
    }
}

/// The lowest contrast of `front` on `grounds`, laid over each ground both exactly (as the skin checks measure) and
/// rounded to the 8-bit color a browser paints.
fn worst(front: Rgba, grounds: &[Rgba]) -> f64 {
    grounds
        .iter()
        .map(|ground| {
            let laid = front.over(*ground);
            color::contrast(laid, *ground).min(color::contrast(laid.rounded(), *ground))
        })
        .fold(f64::INFINITY, f64::min)
}

/// How far past its minimum a raised color goes, so that no rounding on the way to the page takes it below.
const MARGIN: f64 = 0.01;

/// The color nearest in lightness to `start` (same hue, as much of its chroma as sRGB allows, same alpha) whose
/// contrast on every ground is at least `min`: `start` itself when it passes. A translucent color that cannot pass
/// becomes opaque; black or white when no lightness passes at all.
pub(crate) fn lift(start: Rgba, grounds: &[Rgba], min: f64) -> Rgba {
    if grounds.is_empty() || worst(start, grounds) + 1e-9 >= min {
        return start;
    }
    let lch = start.oklch();
    let at = |l: f64| Oklch { l, ..lch }.rgb().with_alpha(start.a).rounded();
    let target = min + MARGIN;
    let mut best: Option<(f64, Rgba)> = None;
    for end in [0.0, 1.0] {
        if worst(at(end), grounds) < target {
            continue;
        }
        // Between the start and this end contrast changes one way, so halving finds the nearest passing lightness.
        let (mut fail, mut pass) = (lch.l, end);
        for _ in 0..30 {
            let mid = (fail + pass) / 2.0;
            if worst(at(mid), grounds) >= target {
                pass = mid;
            } else {
                fail = mid;
            }
        }
        let distance = (pass - lch.l).abs();
        if best.is_none_or(|(seen, _)| distance < seen) {
            best = Some((distance, at(pass)));
        }
    }
    match best {
        Some((_, color)) => color,
        None if !start.opaque() => lift(start.with_alpha(1.0), grounds, min),
        None if worst(solid(0.0), grounds) >= worst(solid(1.0), grounds) => solid(0.0),
        None => solid(1.0),
    }
}

/// The seed's hue and chroma at another lightness, inside sRGB.
fn tone(seed: Oklch, l: f64) -> Rgba {
    Oklch { l, ..seed }.rgb().rounded()
}

/// One mode being derived.
struct Mode<'a> {
    tokens: Tokens,
    skin: &'a Tokens,
    dark: bool,
    derived: Vec<&'static str>,
}

impl Mode<'_> {
    fn set(&mut self, name: &'static str, value: Rgba) {
        self.tokens.set(name, &value.css());
        if !self.derived.contains(&name) {
            self.derived.push(name);
        }
    }

    fn set_owned(&mut self, name: &str, value: Rgba) {
        self.tokens.set(name, &value.css());
    }

    fn ground(&self, name: &str) -> Option<Rgba> {
        palette::ground(&self.tokens, name, self.dark).map(Rgba::rounded)
    }

    fn grounds(&self, names: &[&str]) -> Vec<Rgba> {
        names.iter().filter_map(|name| self.ground(name)).collect()
    }

    /// The skin's own lightness for a token, when it writes a literal color there.
    fn skin_l(&self, name: &str) -> Option<f64> {
        literal(self.skin, name).map(|c| c.oklch().l)
    }

    /// The skin writes a literal color for this token, so the derivation replaces it.
    fn owned(&self, name: &str) -> bool {
        literal(self.skin, name).is_some()
    }
}

/// Every pair the derivation keeps legible, as (text, ground, minimum): the contrast checks of skins
/// (docs/skins.md, "Contrast") and the pairs of fills, soft grounds, status colors, code and tags.
pub fn pairs() -> &'static [(&'static str, &'static str, f64)] {
    static PAIRS: LazyLock<Vec<(&'static str, &'static str, f64)>> = LazyLock::new(|| {
        let mut pairs = palette::pairs();
        let mut add = |front: &'static str, grounds: &[&'static str], min: f64| {
            for ground in grounds {
                if !pairs.iter().any(|(f, g, _)| *f == front && g == ground) {
                    pairs.push((front, ground, min));
                }
            }
        };
        add("--mark-text", &["--mark", "--mark-deep"], 4.5);
        add("--brand-text", &["--brand-bg", "--brand-hover-bg"], 4.5);
        add("--selected-text", &["--selected-bg"], 4.5);
        add("--secondary-text", &["--secondary-hover"], 4.5);
        add("--accent", &["--bg", "--panel", "--accent-soft"], 4.5);
        add("--link", &["--panel-alt"], 4.5);
        add("--mark-soft-text", &["--mark-soft"], 4.5);
        add("--info-soft-text", &["--info-soft"], 4.5);
        add("--avatar-text", &["--avatar-bg"], 4.5);
        add("--toolbar-selected-text", &["--toolbar-selected-bg"], 4.5);
        add("--choice-accent", &["--bg", "--panel"], 3.0);
        add("--success", &["--bg", "--panel"], 4.5);
        add("--success-soft-text", &["--success-soft"], 4.5);
        add("--warning", &["--bg", "--panel"], 4.5);
        add("--warning-soft-text", &["--warning-soft"], 4.5);
        add("--danger-text", &["--bg", "--panel"], 4.5);
        add("--danger-soft-text", &["--danger-soft"], 4.5);
        add("--on-danger-fill", &["--danger-fill"], 4.5);
        add("--favorite-text", &["--panel"], 4.5);
        add("--reaction-active", &["--panel"], 4.5);
        add("--inverse-text", &["--inverse"], 4.5);
        add("--editor-text", &["--editor-bg"], 4.5);
        add("--code-inline-text", &["--code-inline-bg"], 4.5);
        add("--code-block-text", &["--code-block-bg"], 4.5);
        add("--quote-text", &["--bg", "--panel"], 4.5);
        for (front, ground) in [
            ("--tag-bounty-text", "--tag-bounty-bg"),
            ("--tag-redpacket-text", "--tag-redpacket-bg"),
            ("--tag-lottery-text", "--tag-lottery-bg"),
            ("--tag-poll-text", "--tag-poll-bg"),
            ("--tag-pinned-text", "--tag-pinned-bg"),
            ("--tag-featured-text", "--tag-featured-bg"),
            ("--tag-news-text", "--tag-news-bg"),
        ] {
            add(front, &[ground], 4.5);
        }
        pairs
    });
    &PAIRS
}

fn derive_mode(skin: &Tokens, ink: Option<Rgba>, dark: bool, seeds: Seeds) -> Tokens {
    let primary = rgba(seeds.primary);
    let secondary = rgba(seeds.secondary);
    let seed = |which: Seed| match which {
        Seed::Primary => primary,
        Seed::Secondary => secondary,
    };
    let mut mode = Mode {
        tokens: skin.clone(),
        skin,
        dark,
        derived: Vec::new(),
    };
    // Neutrals: the skin's lightness, the secondary hue, a share of its chroma; translucent ones keep their alpha.
    let tint = seed(Seed::Secondary).oklch();
    for (name, share) in NEUTRALS {
        let Some(own) = literal(skin, name) else {
            continue;
        };
        let value = Oklch {
            l: own.oklch().l,
            c: tint.c.min(NEUTRAL_CHROMA) * share,
            h: tint.h,
        }
        .rgb()
        .with_alpha(own.a)
        .rounded();
        mode.set(name, value);
    }
    let from = seed(Seed::Primary).oklch();
    let grounds = mode.grounds(&GROUNDS);
    // Colored text: the skin's lightness, the primary's hue and chroma, raised on the grounds.
    for (name, min) in TEXTS {
        if !mode.owned(name) {
            continue;
        }
        let l = mode.skin_l(name).unwrap_or(from.l);
        mode.set(name, lift(tone(from, l), &grounds, min));
    }
    for (name, base) in TEXT_HOVERS {
        if !mode.owned(name) {
            continue;
        }
        // The skin's own step from text to hovered text, or a small step towards more contrast.
        let shift = match (mode.skin_l(name), mode.skin_l(base)) {
            (Some(hover), Some(base)) => hover - base,
            _ if dark => 0.06,
            _ => -0.06,
        };
        let start = literal(&mode.tokens, base).map_or(from.l, |c| c.oklch().l);
        mode.set(name, lift(tone(from, start + shift), &grounds, 4.5));
    }
    // Soft grounds: a translucent one is the derived accent with the skin's alpha; an opaque one keeps the skin's
    // lightness and the share of chroma the skin gives it next to its accent.
    let accent_chroma = literal(skin, "--accent").map(|c| c.oklch().c);
    for name in SOFTS {
        let Some(own) = literal(skin, name) else {
            continue;
        };
        let value = if own.opaque() {
            let share = match accent_chroma {
                Some(accent) if accent > 0.02 => (own.oklch().c / accent).min(1.0),
                _ => 0.1,
            };
            Oklch {
                l: own.oklch().l,
                c: from.c * share,
                h: from.h,
            }
            .rgb()
            .rounded()
        } else {
            literal(&mode.tokens, "--accent")
                .unwrap_or_else(|| tone(from, from.l))
                .with_alpha(own.a)
        };
        mode.set(name, value);
    }
    for name in FILLS {
        if !mode.owned(name) {
            continue;
        }
        let value = match (dark, mode.skin_l(name)) {
            (true, Some(l)) => tone(from, l),
            _ => seed(Seed::Primary),
        };
        mode.set(name, value);
    }
    for (name, base) in HOVERS {
        if !mode.owned(name) {
            continue;
        }
        let Some(fill) = literal(&mode.tokens, base) else {
            continue;
        };
        let shift = match (mode.skin_l(name), mode.skin_l(base)) {
            (Some(hover), Some(base)) if (hover - base).abs() > 0.005 => hover - base,
            _ if dark => 0.06,
            _ => -0.06,
        };
        let lch = fill.oklch();
        mode.set(name, tone(lch, lch.l + shift));
    }
    // Text on fills: white or the light mode's ink, whichever stands out more; a fill that still falls short of
    // 4.5:1 moves away from its text instead.
    let white = solid(1.0);
    let ink = ink
        .or_else(|| palette::resolve(&mode.tokens, "--text-strong").map(Rgba::rounded))
        .unwrap_or(solid(0.0));
    for (name, fills) in ON_FILLS {
        if !mode.owned(name) {
            continue;
        }
        let grounds = mode.grounds(&fills);
        if grounds.is_empty() {
            continue;
        }
        let text = if worst(white, &grounds) >= worst(ink, &grounds) {
            white
        } else {
            ink
        };
        mode.set(name, text);
        for fill in fills {
            if !mode.derived.contains(&fill) {
                continue;
            }
            let Some(ground) = mode.ground(fill) else {
                continue;
            };
            if color::contrast(text, ground) + 1e-9 < 4.5 {
                mode.set(fill, lift(ground, &[text], 4.5));
            }
        }
    }
    // Last, every pair is raised on all of its grounds at once, by changing the token that holds the text's literal
    // value; twice, since a raised token can be the text of another pair.
    for _ in 0..2 {
        let mut wanted: Vec<(String, Vec<Rgba>, f64)> = Vec::new();
        for (front, back, min) in pairs() {
            let Some(holder) = holder(&mode.tokens, front) else {
                continue;
            };
            if literal(&mode.tokens, holder).is_none() {
                continue;
            }
            let Some(ground) = mode.ground(back) else {
                continue;
            };
            match wanted.iter_mut().find(|(name, _, _)| name == holder) {
                Some(entry) => {
                    entry.1.push(ground);
                    entry.2 = entry.2.max(*min);
                }
                None => wanted.push((holder.to_owned(), vec![ground], *min)),
            }
        }
        for (name, grounds, min) in wanted {
            let Some(current) = literal(&mode.tokens, &name) else {
                continue;
            };
            let lifted = lift(current, &grounds, min);
            if lifted != current {
                mode.set_owned(&name, lifted);
            }
        }
    }
    mode.tokens
}

/// A merged palette with the tokens the seeds derive, in both modes.
pub fn derive(merged: &Merged, seeds: Seeds) -> Merged {
    let light = derive_mode(&merged.light, None, false, seeds);
    let ink = palette::resolve(&light, "--text-strong").map(Rgba::rounded);
    let dark = derive_mode(&merged.dark, ink, true, seeds);
    Merged {
        name: merged.name.clone(),
        light,
        dark,
    }
}

/// One token the seeds changed, with its value in each mode (`None` where that mode keeps the skin's).
pub struct Changed {
    pub name: String,
    pub light: Option<String>,
    pub dark: Option<String>,
}

/// The tokens `derived` differs in from `merged`, in the base scheme's order.
pub fn changed(merged: &Merged, derived: &Merged) -> Vec<Changed> {
    let differs = |before: &Tokens, after: &Tokens, name: &str| {
        let value = after.get(name)?;
        (before.get(name) != Some(value)).then(|| value.to_owned())
    };
    derived
        .light
        .0
        .iter()
        .filter_map(|(name, _)| {
            let light = differs(&merged.light, &derived.light, name);
            let dark = differs(&merged.dark, &derived.dark, name);
            (light.is_some() || dark.is_some()).then(|| Changed {
                name: name.clone(),
                light,
                dark,
            })
        })
        .collect()
}

/// The contrast of one pair in light and dark mode (`None` where it cannot be evaluated), for the admin page.
pub struct Measured {
    pub front: &'static str,
    pub back: &'static str,
    pub min: f64,
    pub light: Option<f64>,
    pub dark: Option<f64>,
}

/// An opaque color for a token as a page paints it: laid over the page ground when translucent.
pub fn painted(tokens: &Tokens, name: &str, dark: bool) -> Option<Rgba> {
    palette::ground(tokens, name, dark).map(Rgba::rounded)
}

/// The contrast of every pair of [`pairs`] in a merged palette.
pub fn measure(merged: &Merged) -> Vec<Measured> {
    let ratio = |tokens: &Tokens, dark: bool, front: &str, back: &str| {
        let ground = painted(tokens, back, dark)?;
        let text = palette::resolve(tokens, front)?;
        Some(color::contrast(text.over(ground).rounded(), ground))
    };
    pairs()
        .iter()
        .map(|&(front, back, min)| Measured {
            front,
            back,
            min,
            light: ratio(&merged.light, false, front, back),
            dark: ratio(&merged.dark, true, front, back),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_check::Trust;

    fn base() -> Merged {
        palette::merge("default", &palette::Overrides::default(), "x").0
    }

    fn ruled() -> Merged {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/skins/ruled");
        let (draft, _) = super::super::read_dir(&dir).unwrap();
        palette::merge("main", draft.palettes.get("main").unwrap(), "x").0
    }

    const SEEDS: [(&str, &str); 10] = [
        ("#4f46e5", "#57637a"),
        ("#ffd700", "#00a86b"),
        ("#e11d48", "#7c3aed"),
        ("#0f766e", "#f97316"),
        ("#000000", "#ffffff"),
        ("#ffffff", "#000000"),
        ("#808080", "#808080"),
        ("#22c55e", "#0ea5e9"),
        ("#00ffff", "#ff00ff"),
        ("#1e3a8a", "#fef08a"),
    ];

    #[test]
    fn every_pair_reaches_its_contrast_on_both_skins() {
        for merged in [base(), ruled()] {
            for (p, s) in SEEDS {
                let seeds = Seeds::parse(p, s).unwrap();
                let derived = derive(&merged, seeds);
                assert_eq!(
                    palette::contrast(&derived, Trust::Builtin, "x"),
                    [],
                    "{} {p} {s}",
                    merged.name
                );
                assert!(palette::footer(&derived, "x").is_ok());
                for pair in measure(&derived) {
                    for ratio in [pair.light, pair.dark].into_iter().flatten() {
                        assert!(
                            ratio + 1e-9 >= pair.min,
                            "{} {p} {s}: {} on {} is {ratio:.2}",
                            merged.name,
                            pair.front,
                            pair.back
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fills_are_the_seed_and_text_on_them_is_white_or_ink() {
        let derived = derive(&base(), Seeds::parse("#0f766e", "#f97316").unwrap());
        assert_eq!(derived.light.get("--primary-bg"), Some("#0f766e"));
        assert_eq!(derived.light.get("--primary-text"), Some("#ffffff"));
        let gold = derive(&base(), Seeds::parse("#ffd700", "#00a86b").unwrap());
        assert_eq!(gold.light.get("--primary-bg"), Some("#ffd700"));
        // Ink is the light mode's strongest text, tinted by the secondary seed.
        assert_eq!(
            gold.light.get("--primary-text"),
            gold.light.get("--text-strong")
        );
        // Gold is too light as text on white: links are darkened until they reach 4.5:1.
        let link = color::parse(gold.light.get("--link").unwrap()).unwrap();
        assert!(link.oklch().l < 0.7);
    }

    #[test]
    fn references_and_transparent_tokens_are_left_alone() {
        let merged = ruled();
        let derived = derive(&merged, Seeds::parse("#e11d48", "#7c3aed").unwrap());
        for name in [
            "--brand-bg",
            "--brand-text",
            "--choice-accent",
            "--selected-bg",
        ] {
            assert_eq!(derived.light.get(name), merged.light.get(name), "{name}");
            assert_eq!(derived.dark.get(name), merged.dark.get(name), "{name}");
        }
        // Shadows and other values that are not plain colors stay too.
        assert_eq!(
            derived.light.get("--shadow-medium"),
            merged.light.get("--shadow-medium")
        );
        // Translucent tokens keep their alpha.
        let line = color::parse(
            derive(&base(), Seeds::parse("#e11d48", "#7c3aed").unwrap())
                .dark
                .get("--line")
                .unwrap(),
        )
        .unwrap();
        assert!((line.a - 0.16).abs() < 0.01);
    }

    #[test]
    fn the_derivation_is_stable_and_changes_only_what_seeds_drive() {
        let seeds = Seeds::parse("#4f46e5", "#57637a").unwrap();
        assert_eq!(derive(&base(), seeds), derive(&base(), seeds));
        let changed = changed(&base(), &derive(&base(), seeds));
        assert!(changed.iter().all(|c| !c.name.starts_with("--tag-")));
        assert!(changed.iter().any(|c| c.name == "--primary-bg"));
        assert!(!changed.iter().any(|c| c.name == "--backdrop"));
    }

    #[test]
    fn seeds_parse_from_forms_documents_and_addresses() {
        let seeds = Seeds::parse("#4F46E5", " #2563eb ").unwrap();
        assert_eq!(seeds.code(), "4f46e52563eb");
        assert_eq!(Seeds::from_code("4f46e52563eb"), Some(seeds));
        assert_eq!(
            serde_json::to_string(&seeds).unwrap(),
            r##"{"primary":"#4f46e5","secondary":"#2563eb"}"##
        );
        for bad in ["4f46e5", "#4f46e", "#4f46e5f", "#gggggg", "", "red", "#4f4"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        for bad in [
            "4f46e52563e",
            "4f46e52563ebx",
            "zzzzzzzzzzzz",
            "4f46e5\u{e9}2563",
        ] {
            assert_eq!(Seeds::from_code(bad), None, "{bad}");
        }
        assert!(
            serde_json::from_str::<Seeds>(r##"{"primary":"red","secondary":"#000000"}"##).is_err()
        );
        assert!(
            serde_json::from_str::<Seeds>(r##"{"primary":"#000000","secondary":"#000000","x":1}"##)
                .is_err()
        );
    }
}
