//! Skin palettes (docs/skins.md, "Palettes"): overrides of the base color scheme
//! `frontend/styles/themes/default.json`, merged onto it and written as custom-property rules.
//!
//! The base has `shared`, `light` and `dark` objects; light mode is `shared` then `light`, dark mode is `shared`,
//! `light` then `dark` (docs/styles.md, "Color scheme"). A skin palette has the same three objects and only lists
//! what it changes: `shared` changes both modes, `light` only light mode and `dark` only dark mode. Tokens the base
//! does not have are ignored with a warning, so a skin cannot add colors the core stylesheets never read.
//!
//! The rules are written the way `scripts/build-styles.mjs` used to write them: light values on
//! `:root[data-palette="<name>"]` (and on `:root` without `data-palette` for the skin's default palette), dark values
//! under `[data-theme="dark"]`, and dark values again under `@media (prefers-color-scheme: dark)` when `data-theme` is
//! neither `light` nor `dark`.
use std::fmt::Write as _;
use std::sync::LazyLock;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::color::{self, Rgba};
use crate::css_check::{Finding, Problem, Severity, Trust};

/// Tokens of one object, in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tokens(pub Vec<(String, String)>);

impl Tokens {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Sets a token, keeping the position of an existing one.
    fn set(&mut self, name: &str, value: &str) {
        match self.0.iter_mut().find(|(key, _)| key == name) {
            Some(slot) => slot.1 = value.to_owned(),
            None => self.0.push((name.to_owned(), value.to_owned())),
        }
    }
}

impl Serialize for Tokens {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Tokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Tokens;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of token names and string values")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tokens, A::Error> {
                let mut tokens = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, String>()? {
                    if tokens
                        .iter()
                        .any(|(seen, _): &(String, String)| *seen == name)
                    {
                        return Err(serde::de::Error::custom(format!("{name} appears twice")));
                    }
                    tokens.push((name, value));
                }
                Ok(Tokens(tokens))
            }
        }
        deserializer.deserialize_map(Ordered)
    }
}

/// One palette as written: the three objects, each optional in a skin.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default)]
    pub shared: Tokens,
    #[serde(default)]
    pub light: Tokens,
    #[serde(default)]
    pub dark: Tokens,
}

/// A skin's palettes by name, in source order; stored as one JSON object in `gonof_common_skin.palettes`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palettes(pub Vec<(String, Overrides)>);

impl Serialize for Palettes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, palette) in &self.0 {
            map.serialize_entry(name, palette)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Palettes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Palettes;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of palettes by name")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Palettes, A::Error> {
                let mut palettes = Vec::new();
                while let Some((name, palette)) = map.next_entry::<String, Overrides>()? {
                    if palettes
                        .iter()
                        .any(|(seen, _): &(String, Overrides)| *seen == name)
                    {
                        return Err(serde::de::Error::custom(format!("{name} appears twice")));
                    }
                    palettes.push((name, palette));
                }
                Ok(Palettes(palettes))
            }
        }
        deserializer.deserialize_map(Ordered)
    }
}

impl Palettes {
    pub fn get(&self, name: &str) -> Option<&Overrides> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, palette)| palette)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(name, _)| name.as_str())
    }
}

/// A palette merged onto the base: every base token in both modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub name: String,
    pub light: Tokens,
    pub dark: Tokens,
}

/// The base color scheme, the contract every palette fills.
pub struct Base {
    pub light: Tokens,
    pub dark: Tokens,
}

/// `frontend/styles/themes/default.json`, compiled in by `build.rs`.
pub static BASE: LazyLock<Base> = LazyLock::new(|| {
    let base: Overrides = serde_json::from_str(super::BASE_PALETTE)
        .expect("frontend/styles/themes/default.json must be a palette");
    let mut light = base.shared.clone();
    for (name, value) in &base.light.0 {
        light.set(name, value);
    }
    let mut dark = light.clone();
    for (name, value) in &base.dark.0 {
        dark.set(name, value);
    }
    Base { light, dark }
});

/// Palette names: they appear in `data-palette` and in the stylesheet's attribute selectors.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_token(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("--") else {
        return false;
    };
    let bytes = rest.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// A token value: one CSS value that the stylesheet checker accepts on its own, without braces, semicolons, `!`,
/// quotes, comments, line breaks or `url()` (a palette holds colors, shadows and the like, never resources). The
/// value is written into the generated rules as it is, so nothing in it may reach past its declaration.
fn valid_value(value: &str) -> bool {
    if value.trim().is_empty()
        || value.len() > 1024
        || value.contains(['{', '}', ';', '!', '<', '>', '"', '\'', '\n', '\r'])
        || value.contains("/*")
        || value.contains("*/")
    {
        return false;
    }
    crate::css_check::check(
        &format!(":root {{ --palette-value: {value}; }}"),
        crate::css_check::Profile::Skin(Trust::External),
        super::MAX_CSS_BYTES,
        1,
    )
    .iter()
    .all(|finding| finding.severity != Severity::Error)
}

fn finding(file: &str, severity: Severity, problem: Problem) -> Finding {
    Finding {
        file: file.to_owned(),
        line: None,
        severity,
        problem,
    }
}

/// Merges one palette onto the base and checks it: token names and values, unknown tokens (warnings), references
/// and cycles. `file` names the palette in findings (`palettes/<name>.json`).
pub fn merge(name: &str, overrides: &Overrides, file: &str) -> (Merged, Vec<Finding>) {
    let mut findings = Vec::new();
    let mut light = BASE.light.clone();
    let mut dark = BASE.dark.clone();
    let objects = [
        (&overrides.shared, true, true),
        (&overrides.light, true, false),
        (&overrides.dark, false, true),
    ];
    for (tokens, to_light, to_dark) in objects {
        for (token, value) in &tokens.0 {
            if token.starts_with("--gonof-") {
                findings.push(finding(
                    file,
                    Severity::Error,
                    Problem::ReservedVariable {
                        name: token.clone(),
                    },
                ));
                continue;
            }
            if !valid_token(token) {
                findings.push(finding(
                    file,
                    Severity::Error,
                    Problem::PaletteToken {
                        name: token.clone(),
                    },
                ));
                continue;
            }
            if !valid_value(value) {
                findings.push(finding(
                    file,
                    Severity::Error,
                    Problem::PaletteValue {
                        name: token.clone(),
                    },
                ));
                continue;
            }
            if BASE.light.get(token).is_none() {
                findings.push(finding(
                    file,
                    Severity::Warning,
                    Problem::PaletteUnknown {
                        name: token.clone(),
                    },
                ));
                continue;
            }
            if to_light {
                light.set(token, value);
            }
            if to_dark {
                dark.set(token, value);
            }
        }
    }
    for (mode, tokens) in [("light", &light), ("dark", &dark)] {
        findings.extend(references(tokens, mode, file));
    }
    (
        Merged {
            name: name.to_owned(),
            light,
            dark,
        },
        findings,
    )
}

/// The `var(--…)` names a value refers to.
fn referenced(value: &str) -> impl Iterator<Item = &str> {
    value.match_indices("var(").filter_map(move |(index, _)| {
        let rest = value[index + 4..].trim_start();
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        rest.starts_with("--").then(|| &rest[..end])
    })
}

/// Undefined references and reference cycles in one mode.
fn references(tokens: &Tokens, mode: &str, file: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (name, value) in &tokens.0 {
        for target in referenced(value) {
            if tokens.get(target).is_none() {
                findings.push(finding(
                    file,
                    Severity::Error,
                    Problem::PaletteReference {
                        mode: mode.to_owned(),
                        name: name.clone(),
                        target: target.to_owned(),
                    },
                ));
            }
        }
    }
    // Depth-first search from every token; a token met again on the current chain is a cycle.
    fn visit<'a>(
        tokens: &'a Tokens,
        name: &'a str,
        chain: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(start) = chain.iter().position(|seen| *seen == name) {
            let mut cycle: Vec<String> = chain[start..].iter().map(|s| (*s).to_owned()).collect();
            cycle.push(name.to_owned());
            return Some(cycle);
        }
        chain.push(name);
        for target in referenced(tokens.get(name).unwrap_or("")) {
            if tokens.get(target).is_some()
                && let Some(cycle) = visit(tokens, target, chain)
            {
                return Some(cycle);
            }
        }
        chain.pop();
        None
    }
    for (name, _) in &tokens.0 {
        if let Some(cycle) = visit(tokens, name, &mut Vec::new()) {
            findings.push(finding(
                file,
                Severity::Error,
                Problem::PaletteCycle {
                    mode: mode.to_owned(),
                    chain: cycle.join(" -> "),
                },
            ));
            break;
        }
    }
    findings
}

/// A token's value with `var()` references followed, as a color; `None` when it is not a color this checker
/// understands (a fallback in `var()` is not followed).
fn resolve(tokens: &Tokens, name: &str) -> Option<Rgba> {
    let mut value = tokens.get(name)?;
    for _ in 0..32 {
        let trimmed = value.trim();
        match trimmed
            .strip_prefix("var(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            Some(target) => value = tokens.get(target.trim())?,
            None => return color::parse(trimmed),
        }
    }
    None
}

/// An opaque ground: the token laid over the page ground when it is translucent, and the page ground over white or
/// black.
fn ground(tokens: &Tokens, name: &str, dark: bool) -> Option<Rgba> {
    let canvas = if dark {
        Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    } else {
        Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }
    };
    let page = resolve(tokens, "--bg")?.over(canvas);
    let color = resolve(tokens, name)?;
    Some(if color.opaque() {
        color
    } else {
        color.over(page)
    })
}

/// Text and controls that must stand out from their grounds (docs/skins.md, "Contrast"): text 4.5:1, input borders
/// and the focus ring 3:1.
const TEXT: [&str; 4] = ["--text-strong", "--text", "--text-muted", "--text-subtle"];
const GROUNDS: [&str; 6] = [
    "--bg",
    "--panel",
    "--panel-alt",
    "--panel-hover",
    "--input-bg",
    "--secondary-bg",
];

fn pairs() -> Vec<(&'static str, &'static str, f64)> {
    let mut pairs = Vec::new();
    for foreground in TEXT {
        for background in GROUNDS {
            pairs.push((foreground, background, 4.5));
        }
    }
    pairs.extend([
        ("--primary-text", "--primary-bg", 4.5),
        ("--primary-text", "--primary-hover", 4.5),
        ("--secondary-text", "--secondary-bg", 4.5),
        ("--input-text", "--input-bg", 4.5),
        ("--input-placeholder", "--input-bg", 4.5),
        ("--link", "--bg", 4.5),
        ("--link", "--panel", 4.5),
        ("--input-border", "--input-bg", 3.0),
        ("--input-border", "--panel", 3.0),
        ("--input-border", "--bg", 3.0),
        ("--focus-ring", "--bg", 3.0),
        ("--focus-ring", "--panel", 3.0),
    ]);
    pairs
}

fn modes(merged: &Merged) -> [(&'static str, &Tokens, bool); 2] {
    [
        ("light", &merged.light, false),
        ("dark", &merged.dark, true),
    ]
}

/// The contrast checks of one merged palette: built-in skins must pass, the others get warnings.
pub fn contrast(merged: &Merged, trust: Trust, file: &str) -> Vec<Finding> {
    let severity = trust.style();
    let mut findings = Vec::new();
    for (mode, tokens, dark) in modes(merged) {
        for (foreground, background, min) in pairs() {
            let Some(back) = ground(tokens, background, dark) else {
                findings.push(finding(
                    file,
                    severity,
                    Problem::ContrastUnknown {
                        mode: mode.to_owned(),
                        foreground: foreground.to_owned(),
                        background: background.to_owned(),
                    },
                ));
                continue;
            };
            let Some(front) = resolve(tokens, foreground) else {
                findings.push(finding(
                    file,
                    severity,
                    Problem::ContrastUnknown {
                        mode: mode.to_owned(),
                        foreground: foreground.to_owned(),
                        background: background.to_owned(),
                    },
                ));
                continue;
            };
            let ratio = color::contrast(front.over(back), back);
            if ratio + 1e-9 < min {
                findings.push(finding(
                    file,
                    severity,
                    Problem::Contrast {
                        mode: mode.to_owned(),
                        foreground: foreground.to_owned(),
                        background: background.to_owned(),
                        ratio: format!("{:.2}", (ratio * 100.0).floor() / 100.0),
                        min: format!("{min}"),
                    },
                ));
            }
        }
    }
    findings
}

/// The footer line's colors in one mode: the legal text, the brand link and their ground, as opaque literals.
pub struct FooterColors {
    pub text: Rgba,
    pub brand: Rgba,
    pub ground: Rgba,
}

/// The colors the "Built on Gono Forum" guard fixes (docs/skins.md, "Built on Gono Forum"): `--text-muted` and
/// `--text` on `--panel`, plain colors with 4.5:1 in both modes. Every skin must pass.
pub fn footer(merged: &Merged, file: &str) -> Result<[FooterColors; 2], Vec<Finding>> {
    let mut findings = Vec::new();
    let mut found = Vec::new();
    for (mode, tokens, dark) in modes(merged) {
        let ground = ground(tokens, "--panel", dark);
        let text = resolve(tokens, "--text-muted");
        let brand = resolve(tokens, "--text");
        let (Some(ground), Some(text), Some(brand)) = (ground, text, brand) else {
            findings.push(finding(
                file,
                Severity::Error,
                Problem::ColophonColor {
                    mode: mode.to_owned(),
                },
            ));
            continue;
        };
        let (text, brand) = (text.over(ground), brand.over(ground));
        for (token, color) in [("--text-muted", text), ("--text", brand)] {
            let ratio = color::contrast(color, ground);
            if ratio + 1e-9 < 4.5 {
                findings.push(finding(
                    file,
                    Severity::Error,
                    Problem::ColophonContrast {
                        mode: mode.to_owned(),
                        foreground: token.to_owned(),
                        ratio: format!("{:.2}", (ratio * 100.0).floor() / 100.0),
                    },
                ));
            }
        }
        found.push(FooterColors {
            text,
            brand,
            ground,
        });
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    let dark = found.pop().expect("two modes");
    let light = found.pop().expect("two modes");
    Ok([light, dark])
}

fn block(out: &mut String, selector: &str, scheme: &str, tokens: &Tokens) {
    let _ = writeln!(out, "{selector} {{");
    let _ = writeln!(out, "  color-scheme: {scheme};");
    for (name, value) in &tokens.0 {
        let _ = writeln!(out, "  {name}: {value};");
    }
    out.push_str("}\n");
}

/// The selectors of one palette: its own and, for the skin's default palette, a page without `data-palette`.
fn roots(name: &str, default: bool) -> Vec<String> {
    let mut roots = vec![format!(":root[data-palette=\"{name}\"]")];
    if default {
        roots.push(":root:not([data-palette])".to_owned());
    }
    roots
}

/// The three rules of one palette (light, dark, dark when following the system).
pub fn css(merged: &Merged, default: bool, out: &mut String) {
    let roots = roots(&merged.name, default);
    block(out, &roots.join(", "), "light", &merged.light);
    let dark: Vec<String> = roots
        .iter()
        .map(|root| format!("{root}[data-theme=\"dark\"]"))
        .collect();
    block(out, &dark.join(", "), "dark", &merged.dark);
    let system: Vec<String> = roots
        .iter()
        .map(|root| format!("{root}:not([data-theme=\"light\"]):not([data-theme=\"dark\"])"))
        .collect();
    out.push_str("@media (prefers-color-scheme: dark) {\n");
    block(out, &system.join(", "), "dark", &merged.dark);
    out.push_str("}\n");
}

/// The guard rules of one palette: the footer line's colors fixed in both modes, inside the `guards` layer.
pub fn footer_css(name: &str, default: bool, colors: &[FooterColors; 2], out: &mut String) {
    let rules = |out: &mut String, roots: &[String], colors: &FooterColors| {
        let select = |part: &str| {
            roots
                .iter()
                .map(|root| format!("{root} {part}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "{} {{\n  color: {} !important;\n  background-color: {} !important;\n}}",
            select(".colophon__legal"),
            colors.text.hex(),
            colors.ground.hex()
        );
        let _ = writeln!(
            out,
            "{} {{\n  color: {} !important;\n}}",
            select(".colophon__brand"),
            colors.brand.hex()
        );
    };
    let roots = roots(name, default);
    rules(out, &roots, &colors[0]);
    let dark: Vec<String> = roots
        .iter()
        .map(|root| format!("{root}[data-theme=\"dark\"]"))
        .collect();
    rules(out, &dark, &colors[1]);
    let system: Vec<String> = roots
        .iter()
        .map(|root| format!("{root}:not([data-theme=\"light\"]):not([data-theme=\"dark\"])"))
        .collect();
    out.push_str("@media (prefers-color-scheme: dark) {\n");
    rules(out, &system, &colors[1]);
    out.push_str("}\n");
}

/// The base checks `build-styles.mjs` used to run on `default.json`, for the unit tests.
#[cfg(test)]
fn base_findings() -> Vec<Finding> {
    let base: Overrides = serde_json::from_str(super::BASE_PALETTE).unwrap();
    let mut findings = Vec::new();
    for tokens in [&base.shared, &base.light, &base.dark] {
        for (name, value) in &tokens.0 {
            if !valid_token(name) || !valid_value(value) {
                findings.push(finding(
                    "default.json",
                    Severity::Error,
                    Problem::PaletteValue { name: name.clone() },
                ));
            }
        }
    }
    findings.extend(references(&BASE.light, "light", "default.json"));
    findings.extend(references(&BASE.dark, "dark", "default.json"));
    for (name, _) in &base.dark.0 {
        if BASE.light.get(name).is_none() {
            findings.push(finding(
                "default.json",
                Severity::Error,
                Problem::PaletteUnknown { name: name.clone() },
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(pairs: &[(&str, &str)]) -> Tokens {
        Tokens(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn the_base_scheme_is_valid_and_passes_every_check() {
        assert_eq!(base_findings(), []);
        let (merged, findings) = merge("default", &Overrides::default(), "palettes/default.json");
        assert_eq!(findings, []);
        assert_eq!(merged.light, BASE.light);
        assert_eq!(
            contrast(&merged, Trust::Builtin, "palettes/default.json"),
            []
        );
        assert!(footer(&merged, "palettes/default.json").is_ok());
        assert_eq!(merged.light.0.len(), merged.dark.0.len());
    }

    #[test]
    fn overrides_change_only_their_mode_and_unknown_tokens_warn() {
        let overrides = Overrides {
            shared: tokens(&[("--accent", "#0f766e")]),
            light: tokens(&[("--bg", "#fffdf7"), ("--masthead-bg", "#fff")]),
            dark: tokens(&[("--bg", "#101418")]),
        };
        let (merged, findings) = merge("main", &overrides, "palettes/main.json");
        assert_eq!(merged.light.get("--accent"), Some("#0f766e"));
        assert_eq!(merged.dark.get("--accent"), Some("#0f766e"));
        assert_eq!(merged.light.get("--bg"), Some("#fffdf7"));
        assert_eq!(merged.dark.get("--bg"), Some("#101418"));
        // Tokens keep the base's order.
        assert_eq!(
            merged
                .light
                .0
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            BASE.light
                .0
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            findings,
            [Finding {
                file: "palettes/main.json".into(),
                line: None,
                severity: Severity::Warning,
                problem: Problem::PaletteUnknown {
                    name: "--masthead-bg".into()
                }
            }]
        );
    }

    #[test]
    fn bad_names_values_references_and_cycles_are_errors() {
        let overrides = Overrides {
            shared: tokens(&[
                ("--Bad", "#fff"),
                ("--gonof-colophon", "#fff"),
                ("--panel", "url(https://evil.example/x.png)"),
                ("--panel-alt", "red; } body { display: none"),
                ("--line", "#ccc !important"),
                ("--line-soft", "#ccc /* open"),
                ("--bg", "var(--nowhere)"),
                ("--text", "var(--text-strong)"),
                ("--text-strong", "var(--text)"),
            ]),
            ..Overrides::default()
        };
        let (_, findings) = merge("main", &overrides, "palettes/main.json");
        let codes: Vec<String> = findings
            .iter()
            .map(|f| {
                serde_json::to_value(&f.problem).unwrap()["code"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            codes,
            [
                "palette-token",
                "reserved-variable",
                "palette-value",
                "palette-value",
                "palette-value",
                "palette-value",
                "palette-reference",
                "palette-cycle",
                "palette-reference",
                "palette-cycle",
            ]
        );
        assert!(findings.iter().all(|f| f.severity == Severity::Error));
    }

    #[test]
    fn low_contrast_blocks_built_in_skins_and_warns_for_others() {
        let overrides = Overrides {
            light: tokens(&[("--text-subtle", "#a0a0a0")]),
            ..Overrides::default()
        };
        let (merged, _) = merge("main", &overrides, "palettes/main.json");
        let builtin = contrast(&merged, Trust::Builtin, "palettes/main.json");
        assert!(!builtin.is_empty());
        assert!(builtin.iter().all(|f| f.severity == Severity::Error
            && matches!(&f.problem, Problem::Contrast { foreground, .. } if foreground == "--text-subtle")));
        assert!(
            contrast(&merged, Trust::External, "palettes/main.json")
                .iter()
                .all(|f| f.severity == Severity::Warning)
        );
        // Colors the checker cannot evaluate are reported, not guessed.
        let overrides = Overrides {
            shared: tokens(&[("--panel", "color-mix(in srgb, white, black)")]),
            ..Overrides::default()
        };
        let (merged, _) = merge("main", &overrides, "palettes/main.json");
        assert!(
            contrast(&merged, Trust::External, "palettes/main.json")
                .iter()
                .any(|f| matches!(f.problem, Problem::ContrastUnknown { .. }))
        );
        assert!(footer(&merged, "palettes/main.json").is_err());
    }

    #[test]
    fn the_footer_colors_must_be_legible_for_every_skin() {
        let overrides = Overrides {
            light: tokens(&[("--text-muted", "#e0e0e0")]),
            ..Overrides::default()
        };
        let (merged, _) = merge("main", &overrides, "palettes/main.json");
        let findings = footer(&merged, "palettes/main.json").err().unwrap();
        assert!(
            matches!(&findings[0].problem, Problem::ColophonContrast { mode, .. } if mode == "light")
        );
    }

    #[test]
    fn rules_follow_the_old_build_script() {
        let merged = Merged {
            name: "default".into(),
            light: tokens(&[("--bg", "#fff"), ("--text", "var(--ink)")]),
            dark: tokens(&[("--bg", "#000"), ("--text", "var(--ink)")]),
        };
        let mut out = String::new();
        css(&merged, true, &mut out);
        assert_eq!(
            out,
            ":root[data-palette=\"default\"], :root:not([data-palette]) {\n  color-scheme: light;\n  --bg: #fff;\n  --text: var(--ink);\n}\n\
             :root[data-palette=\"default\"][data-theme=\"dark\"], :root:not([data-palette])[data-theme=\"dark\"] {\n  color-scheme: dark;\n  --bg: #000;\n  --text: var(--ink);\n}\n\
             @media (prefers-color-scheme: dark) {\n:root[data-palette=\"default\"]:not([data-theme=\"light\"]):not([data-theme=\"dark\"]), :root:not([data-palette]):not([data-theme=\"light\"]):not([data-theme=\"dark\"]) {\n  color-scheme: dark;\n  --bg: #000;\n  --text: var(--ink);\n}\n}\n"
        );
    }

    #[test]
    fn palettes_keep_their_order_through_json() {
        let text =
            r##"{"main":{"light":{"--z":"#000","--a":"#fff"}},"alt":{"dark":{"--bg":"#111"}}}"##;
        let palettes: Palettes = serde_json::from_str(text).unwrap();
        assert_eq!(palettes.names().collect::<Vec<_>>(), ["main", "alt"]);
        assert_eq!(
            serde_json::to_string(&palettes).unwrap(),
            r##"{"main":{"shared":{},"light":{"--z":"#000","--a":"#fff"},"dark":{}},"alt":{"shared":{},"light":{},"dark":{"--bg":"#111"}}}"##
        );
        assert!(
            serde_json::from_str::<Palettes>(r#"{"a":{"light":{"--x":"1","--x":"2"}}}"#).is_err()
        );
        assert!(serde_json::from_str::<Palettes>(r#"{"a":{"other":{}}}"#).is_err());
    }
}
