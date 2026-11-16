//! `skin.toml`, the skin manifest (docs/skins.md, "The manifest"). Read from TOML in a skin directory and from JSON
//! in a skin pack and in `gonof_common_skin.manifest`; both come into [`Manifest`] and are stored as its normalized
//! JSON.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::css_check::{Finding, Problem, Severity, Trust};

/// The version of the skin contract this core reads.
pub const CONTRACT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub contract: u32,
    pub id: String,
    pub version: String,
    /// By language tag; English is required.
    pub name: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub description: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    /// The oldest Gono Forum version the skin works with; empty for any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub min_forum: String,
    pub palettes: PaletteList,
    #[serde(default)]
    pub frame: Frame,
    #[serde(default)]
    pub behaviors: Behaviors,
    /// The skin's default layout; layouts arrive with the data sources (phase 2), so it must be absent or empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteList {
    /// The palette used unless another is chosen: `palettes/<default>.json`.
    pub default: String,
    #[serde(default)]
    pub variants: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Nav {
    Top,
    Side,
}

/// Who decides the avatar shape (docs/skins.md, "Frame"): a fixed shape, or the site default and each member's
/// choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AvatarShape {
    Circle,
    Rounded,
    Square,
    Member,
}

impl AvatarShape {
    /// The value of `<html data-avatar-shape>`; rounded is the default look and has none.
    pub fn attribute(self) -> Option<&'static str> {
        match self {
            Self::Circle => Some("circle"),
            Self::Square => Some("square"),
            Self::Rounded | Self::Member => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    #[serde(default = "Frame::top")]
    pub nav: Nav,
    #[serde(default = "Frame::yes")]
    pub header_boards: bool,
    /// The declared header height (`--masthead-h`), such as `52px`; `None` keeps the core's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masthead_height: Option<String>,
    #[serde(default = "Frame::member")]
    pub avatar_shape: AvatarShape,
}

impl Frame {
    /// The value of `<html data-nav>`.
    pub fn nav_attribute(&self) -> &'static str {
        match self.nav {
            Nav::Top => "top",
            Nav::Side => "side",
        }
    }

    fn top() -> Nav {
        Nav::Top
    }
    fn yes() -> bool {
        true
    }
    fn member() -> AvatarShape {
        AvatarShape::Member
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            nav: Nav::Top,
            header_boards: true,
            masthead_height: None,
            avatar_shape: AvatarShape::Member,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Behaviors {
    #[serde(default)]
    pub enabled: Vec<String>,
}

/// `^[a-z][a-z0-9-]{1,31}$`: it is a directory name, a URL segment and the value of `data-skin`.
pub fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `major.minor.patch`; a pre-release suffix after `-` (letters, digits, dots and hyphens) is allowed and ignored
/// when comparing. The version is written into the comment at the top of the compiled stylesheet, so nothing else may
/// appear in it.
pub fn parse_version(version: &str) -> Option<[u64; 3]> {
    if version.is_empty() || version.len() > 32 {
        return None;
    }
    let (core, suffix) = version.split_once('-').unwrap_or((version, ""));
    if !suffix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || (version.contains('-') && suffix.is_empty())
    {
        return None;
    }
    let mut parts = core.split('.');
    let mut numbers = [0_u64; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(numbers)
}

fn valid_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let first = parts.next().unwrap_or("");
    tag.len() <= 16
        && (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn plain(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

impl Manifest {
    /// Reads `skin.toml`; the error is the one finding that says why it cannot be read.
    pub fn from_toml(text: &str) -> Result<Self, Vec<Finding>> {
        toml::from_str(text).map_err(|error| {
            vec![finding(
                Severity::Error,
                Problem::ManifestSyntax {
                    detail: error.message().to_owned(),
                },
            )]
        })
    }

    /// The normalized form stored in `gonof_common_skin.manifest` and in a pack.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a manifest always serializes")
    }

    /// The name in `lang`, else English.
    pub fn name_in(&self, lang: crate::i18n::Lang) -> &str {
        self.name
            .get(lang.tag())
            .or_else(|| self.name.get("en"))
            .map_or(self.id.as_str(), String::as_str)
    }

    pub fn description_in(&self, lang: crate::i18n::Lang) -> &str {
        self.description
            .get(lang.tag())
            .or_else(|| self.description.get("en"))
            .map_or("", String::as_str)
    }

    /// Every palette the manifest names, the default first.
    pub fn palette_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.palettes.default.as_str())
            .chain(self.palettes.variants.iter().map(String::as_str))
    }

    /// The manifest's own rules. `trust` decides how the rules that are not about safety count.
    pub fn check(&self, trust: Trust) -> Vec<Finding> {
        let mut findings = Vec::new();
        let mut error = |problem| findings.push(finding(Severity::Error, problem));
        if self.contract != CONTRACT {
            error(Problem::ManifestContract {
                found: self.contract.to_string(),
            });
        }
        if !valid_id(&self.id) {
            error(Problem::ManifestId {
                value: self.id.chars().take(40).collect(),
            });
        }
        if parse_version(&self.version).is_none() {
            error(Problem::ManifestVersion {
                field: "version".into(),
            });
        }
        for (field, texts, max, required) in [
            ("name", &self.name, 60, true),
            ("description", &self.description, 300, false),
        ] {
            if (required || !texts.is_empty()) && !texts.contains_key("en") {
                error(Problem::ManifestName {
                    field: field.into(),
                });
            }
            if texts.len() > 32
                || texts
                    .iter()
                    .any(|(tag, text)| !valid_tag(tag) || !plain(text, max))
            {
                error(Problem::ManifestText {
                    field: field.into(),
                });
            }
        }
        for (field, text, max) in [
            ("author", &self.author, 100),
            ("license", &self.license, 64),
        ] {
            if !text.is_empty() && !plain(text, max) {
                error(Problem::ManifestText {
                    field: field.into(),
                });
            }
        }
        if !self.min_forum.is_empty() {
            match parse_version(&self.min_forum) {
                None => error(Problem::ManifestVersion {
                    field: "min_forum".into(),
                }),
                Some(needed) if parse_version(crate::VERSION).is_some_and(|ours| ours < needed) => {
                    error(Problem::ManifestMinForum {
                        version: self.min_forum.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        let names: Vec<&str> = self.palette_names().collect();
        if names.len() > 9
            || names.iter().any(|name| !super::palette::valid_name(name))
            || (1..names.len()).any(|i| names[..i].contains(&names[i]))
        {
            error(Problem::ManifestValue {
                field: "palettes".into(),
            });
        }
        if self.frame.nav != Nav::Top {
            error(Problem::ManifestUnsupported {
                field: "frame.nav".into(),
            });
        }
        if !self.frame.header_boards {
            error(Problem::ManifestUnsupported {
                field: "frame.header_boards".into(),
            });
        }
        if let Some(height) = &self.frame.masthead_height {
            let valid = height
                .strip_suffix("px")
                .and_then(|number| number.parse::<u32>().ok())
                .is_some_and(|number| (40..=120).contains(&number) && height.len() <= 6);
            if !valid {
                error(Problem::ManifestValue {
                    field: "frame.masthead_height".into(),
                });
            }
        }
        if !self.behaviors.enabled.is_empty() {
            error(Problem::ManifestUnsupported {
                field: "behaviors.enabled".into(),
            });
        }
        if self.layout.as_ref().is_some_and(|layout| {
            !(layout.is_null() || layout.as_object().is_some_and(|o| o.is_empty()))
        }) {
            error(Problem::ManifestUnsupported {
                field: "layout".into(),
            });
        }
        // A built-in skin keeps the header the core measured (docs/styles.md, "Header navigation widths").
        if self.frame.masthead_height.is_some() {
            findings.push(finding(
                trust.style(),
                Problem::MastheadSize {
                    property: "frame.masthead_height".into(),
                },
            ));
        }
        findings
    }
}

fn finding(severity: Severity, problem: Problem) -> Finding {
    Finding {
        file: "skin.toml".into(),
        line: None,
        severity,
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = r#"
contract = 1
id = "ledger"
version = "1.2.0"
name = { en = "Ledger", fr = "Registre" }
description = { en = "Flat surfaces separated by rules" }
author = "Someone"
license = "MIT"
min_forum = "0.1.0"

[palettes]
default = "main"
variants = ["contrast"]

[frame]
avatar_shape = "circle"
"#;

    #[test]
    fn a_manifest_reads_and_normalizes() {
        let manifest = Manifest::from_toml(LEDGER).unwrap();
        assert_eq!(manifest.check(Trust::External), []);
        assert_eq!(manifest.frame.avatar_shape, AvatarShape::Circle);
        assert_eq!(manifest.frame.nav, Nav::Top);
        assert_eq!(
            manifest.palette_names().collect::<Vec<_>>(),
            ["main", "contrast"]
        );
        let json = manifest.to_json();
        assert_eq!(
            json,
            r#"{"contract":1,"id":"ledger","version":"1.2.0","name":{"en":"Ledger","fr":"Registre"},"description":{"en":"Flat surfaces separated by rules"},"author":"Someone","license":"MIT","min_forum":"0.1.0","palettes":{"default":"main","variants":["contrast"]},"frame":{"nav":"top","header_boards":true,"avatar_shape":"circle"},"behaviors":{"enabled":[]}}"#
        );
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), manifest);
    }

    fn problems(text: &str) -> Vec<Problem> {
        match Manifest::from_toml(text) {
            Ok(manifest) => manifest
                .check(Trust::Builtin)
                .into_iter()
                .map(|f| f.problem)
                .collect(),
            Err(findings) => findings.into_iter().map(|f| f.problem).collect(),
        }
    }

    #[test]
    fn invalid_manifests_say_why() {
        let base = "contract = 1\nid = \"ledger\"\nversion = \"1.0.0\"\nname = { en = \"Ledger\" }\n[palettes]\ndefault = \"main\"\n";
        assert_eq!(problems(base), []);
        assert!(matches!(
            problems(&base.replace("contract = 1", "contract = 2"))[..],
            [Problem::ManifestContract { .. }]
        ));
        assert!(matches!(
            problems(&base.replace("\"ledger\"", "\"Ledger\""))[..],
            [Problem::ManifestId { .. }]
        ));
        assert!(matches!(
            problems(&base.replace("1.0.0", "one"))[..],
            [Problem::ManifestVersion { .. }]
        ));
        assert!(matches!(
            problems(&base.replace("en = \"Ledger\"", "fr = \"Registre\""))[..],
            [Problem::ManifestName { .. }]
        ));
        assert!(matches!(
            problems(&base.replacen("[palettes]", "min_forum = \"99.0.0\"\n[palettes]", 1))[..],
            [Problem::ManifestMinForum { .. }]
        ));
        assert!(matches!(
            problems(&format!("{base}[frame]\nnav = \"side\"\n"))[..],
            [Problem::ManifestUnsupported { .. }]
        ));
        assert!(matches!(
            problems(&format!("{base}[behaviors]\nenabled = [\"timeline\"]\n"))[..],
            [Problem::ManifestUnsupported { .. }]
        ));
        assert!(matches!(
            problems(&format!("{base}[layout]\nhome = 1\n"))[..],
            [Problem::ManifestUnsupported { .. }]
        ));
        assert!(matches!(
            problems(&format!("{base}color = \"red\"\n"))[..],
            [Problem::ManifestSyntax { .. }]
        ));
        // A declared header height is a style rule: it blocks built-in skins and warns for the others.
        let manifest =
            Manifest::from_toml(&format!("{base}[frame]\nmasthead_height = \"52px\"\n")).unwrap();
        assert_eq!(manifest.check(Trust::Builtin)[0].severity, Severity::Error);
        assert_eq!(
            manifest.check(Trust::External)[0].severity,
            Severity::Warning
        );
    }

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(parse_version("0.1.3"), Some([0, 1, 3]));
        assert_eq!(parse_version("0.1.1-dev.1"), Some([0, 1, 1]));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.2.3-rc.1"), Some([1, 2, 3]));
        // The version goes into a CSS comment: nothing that could end it.
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1..2",
            "-1.0.0",
            "1.0.0-",
            "1.0.0-*/x{}/*",
            "1.0.0 beta",
        ] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }
}
