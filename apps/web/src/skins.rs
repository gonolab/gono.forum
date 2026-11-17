//! Skins (docs/skins.md): a look made of palettes over the base color scheme, a manifest and an optional checked
//! stylesheet, on top of the hooks the core templates write.
//!
//! - **Sources**, by priority: built in (`apps/web/skins/<id>/`, compiled in by `build.rs`), imported into the
//!   database (`gonof_common_skin`) and the instance directory (`$GONOF_HOME/skins/<id>/`, read at start and on
//!   "Rescan"). Of skins with the same id only the one from the highest source is usable ([`catalog`]).
//! - **Checks** run when a skin is read from any source: the manifest, every palette (tokens, references, contrast,
//!   the footer colors) and the stylesheet ([`crate::css_check`]). Built-in skins must pass every rule; the others
//!   are blocked only by the safety rules and the "Built on Gono Forum" guard and get warnings for the rest.
//! - **Output**: each usable skin compiles to one stylesheet served at `/skin/<id>.css?v=<hash>` after `forum.css`:
//!   the footer guard colors in the `guards` layer, then the palettes, the declared header height and the skin's own
//!   stylesheet in the `skin` layer.
use std::path::Path;
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::css_check::{Finding, Problem, Severity, Trust};

pub mod catalog;
pub mod color;
pub mod manifest;
pub mod palette;
pub mod seed;
pub mod selection;
pub mod store;

pub use catalog::{Catalog, Listed, Status};
pub use manifest::{AvatarShape, Manifest};
pub use palette::Palettes;

include!(concat!(env!("OUT_DIR"), "/skins.rs"));

/// Size limits, the same for every source so that any skin can be exported and imported again (docs/skins.md,
/// "Limits"): the stylesheet as stored, comments included; all palettes together as stored; the manifest.
pub const MAX_CSS_BYTES: usize = 256 * 1024;
pub const MAX_PALETTES_BYTES: usize = 128 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;
/// A pack file holds the three above and its JSON escaping.
pub const MAX_PACK_BYTES: usize = 1024 * 1024;
/// Rules in a skin stylesheet, `@media` blocks included.
const MAX_RULES: usize = 10_000;

/// The skin every site starts with and every broken choice falls back to.
pub const DEFAULT_ID: &str = "default";

/// Where a skin comes from, in priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Builtin,
    Imported,
    Instance,
}

impl Source {
    /// The value of `<html data-skin-source>`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Imported => "imported",
            Self::Instance => "instance",
        }
    }

    pub fn trust(self) -> Trust {
        match self {
            Self::Builtin => Trust::Builtin,
            Self::Imported | Self::Instance => Trust::External,
        }
    }

    pub fn label(self, t: crate::i18n::Lang) -> &'static str {
        match self {
            Self::Builtin => t.skin_source_builtin(),
            Self::Imported => t.skin_source_imported(),
            Self::Instance => t.skin_source_instance(),
        }
    }
}

/// A skin as written: the manifest, the palettes by name and the stylesheet (empty when there is none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub manifest: Manifest,
    pub palettes: Palettes,
    pub css: String,
}

/// A usable skin: checked and compiled.
#[derive(Debug)]
pub struct Skin {
    pub manifest: Manifest,
    pub source: Source,
    /// The palette names, the default first.
    pub palettes: Vec<String>,
    /// The stylesheet served at `/skin/<id>.css`.
    pub stylesheet: Arc<str>,
    /// The first 12 hex digits of the stylesheet's SHA-256: the `?v=` of its address, so a new core version that
    /// generates other rules for the same pack also gets a new address.
    pub version_hash: String,
    /// SHA-256 of the pack (`gonof_common_skin.content_hash`).
    pub content_hash: String,
    /// Warnings of the current checks (errors make a skin unusable).
    pub warnings: Vec<Finding>,
    /// The palettes merged onto the base scheme, the default first, and the skin's own stylesheet: what a colored
    /// variant is compiled from.
    merged: Vec<palette::Merged>,
    css: String,
    /// The seed colors of a colored variant ([`Skin::colored`]); `None` for the skin as written.
    pub seeds: Option<seed::Seeds>,
    /// The skin's default layout (`[layout]` of the manifest), read once: a site that has not changed a place shows
    /// this skin's blocks there (docs/skins.md, "Places and blocks").
    pub places: Option<crate::layout::Places>,
}

impl Skin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The name in `t` ([`display_name`]).
    pub fn name(&self, t: crate::i18n::Lang) -> String {
        display_name(&self.manifest, self.source, t)
    }

    /// The palette to use: `wanted` when the skin has it, else its default.
    pub fn palette<'a>(&'a self, wanted: Option<&'a str>) -> &'a str {
        match wanted {
            Some(name) if self.palettes.iter().any(|palette| palette == name) => name,
            _ => &self.palettes[0],
        }
    }

    /// The stylesheet address without the deployment prefix; a colored variant names its seeds in `c=`.
    pub fn href(&self) -> String {
        match self.seeds {
            None => format!("/skin/{}.css?v={}", self.manifest.id, self.version_hash),
            Some(seeds) => format!(
                "/skin/{}.css?c={}&v={}",
                self.manifest.id,
                seeds.code(),
                self.version_hash
            ),
        }
    }

    /// The merged palette `name` (the default when the skin has no such palette), as the page paints it.
    pub fn merged(&self, name: &str) -> &palette::Merged {
        self.merged
            .iter()
            .find(|merged| merged.name == name)
            .unwrap_or(&self.merged[0])
    }

    /// This skin with seed colors (docs/styles.md, "Seed colors"): every palette derived from the seeds and the
    /// stylesheet compiled again, with its own hash. The skin's checks have passed already, and the derivation keeps
    /// every contrast pair, so only the footer colors are taken again.
    pub fn colored(&self, seeds: seed::Seeds) -> Self {
        let derived: Vec<palette::Merged> = self
            .merged
            .iter()
            .map(|merged| seed::derive(merged, seeds))
            .collect();
        let palettes: Vec<(palette::Merged, [palette::FooterColors; 2])> = derived
            .iter()
            .zip(&self.merged)
            .map(|(derived, merged)| {
                // The derivation raises the footer pairs like every other; should one still fail, the skin's own
                // palette stands in for that palette rather than an illegible footer.
                match palette::footer(derived, "") {
                    Ok(colors) => (derived.clone(), colors),
                    Err(_) => (
                        merged.clone(),
                        palette::footer(merged, "").expect("a usable skin passes the footer check"),
                    ),
                }
            })
            .collect();
        let stylesheet = compile(
            &self.manifest,
            self.source,
            &palettes,
            &self.css,
            Some(seeds),
        );
        Self {
            manifest: self.manifest.clone(),
            source: self.source,
            palettes: self.palettes.clone(),
            version_hash: hex(&Sha256::digest(stylesheet.as_bytes()))[..12].to_owned(),
            stylesheet: stylesheet.into(),
            content_hash: self.content_hash.clone(),
            warnings: self.warnings.clone(),
            merged: palettes.into_iter().map(|(merged, _)| merged).collect(),
            css: self.css.clone(),
            seeds: Some(seeds),
            places: self.places.clone(),
        }
    }
}

/// The name and description of a built-in skin in `t`: messages (`skin-builtin-<id>-name` and `-description` in
/// `locales/<language>/skin.ftl`), so built-in skins are translated like the rest of the interface and no language but
/// English is written outside the translation files. `skin.toml` keeps the English text too, because the contract
/// requires it and packs and the command line show it; a unit test keeps the two equal. External skins carry no
/// translation files and write each language in their `skin.toml`. `None` for an id without messages.
pub fn builtin_text(id: &str, t: crate::i18n::Lang) -> Option<(&'static str, &'static str)> {
    match id {
        DEFAULT_ID => Some((
            t.skin_builtin_default_name(),
            t.skin_builtin_default_description(),
        )),
        _ => None,
    }
}

/// A skin's name in `t`: a built-in skin's from the translation files, any other's from its manifest.
pub fn display_name(manifest: &Manifest, source: Source, t: crate::i18n::Lang) -> String {
    match (source, builtin_text(&manifest.id, t)) {
        (Source::Builtin, Some((name, _))) => name.to_owned(),
        _ => manifest.name_in(t).to_owned(),
    }
}

/// What reading and checking a skin found: the skin when it has no errors, and every finding.
pub struct Checked {
    pub skin: Option<Arc<Skin>>,
    pub findings: Vec<Finding>,
}

impl Checked {
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warning)
    }

    fn failed(findings: Vec<Finding>) -> Self {
        Self {
            skin: None,
            findings,
        }
    }
}

fn error(file: &str, problem: Problem) -> Finding {
    Finding {
        file: file.to_owned(),
        line: None,
        severity: Severity::Error,
        problem,
    }
}

/// Reads a skin from its files: `skin.toml`, `palettes/<name>.json` and an optional `skin.css` (paths relative to
/// the skin directory, `/` separated). Other files are ignored with a warning.
pub fn from_files<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(Draft, Vec<Finding>), Vec<Finding>> {
    let mut findings = Vec::new();
    let mut manifest = None;
    let mut css = String::new();
    let mut palettes = Vec::new();
    for (path, text) in files {
        if path == "skin.toml" {
            if text.len() > MAX_MANIFEST_BYTES {
                return Err(vec![error(
                    "skin.toml",
                    Problem::ManifestTooLong {
                        kib: MAX_MANIFEST_BYTES / 1024,
                    },
                )]);
            }
            manifest = Some(Manifest::from_toml(text)?);
        } else if path == "skin.css" {
            css = text.to_owned();
        } else if let Some(name) = path
            .strip_prefix("palettes/")
            .and_then(|file| file.strip_suffix(".json"))
            .filter(|name| !name.contains('/'))
        {
            match serde_json::from_str::<palette::Overrides>(text) {
                Ok(overrides) => palettes.push((name.to_owned(), overrides)),
                Err(error_) => findings.push(error(
                    path,
                    Problem::PaletteSyntax {
                        detail: error_.to_string(),
                    },
                )),
            }
        } else {
            findings.push(Finding {
                file: path.to_owned(),
                line: None,
                severity: Severity::Warning,
                problem: Problem::PackFile,
            });
        }
    }
    let Some(manifest) = manifest else {
        findings.push(error("skin.toml", Problem::ManifestMissing));
        return Err(findings);
    };
    if findings
        .iter()
        .any(|finding| finding.severity == Severity::Error)
    {
        return Err(findings);
    }
    palettes.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok((
        Draft {
            manifest,
            palettes: Palettes(palettes),
            css,
        },
        findings,
    ))
}

/// Reads the files of a skin directory. Files over [`MAX_PACK_BYTES`] and unreadable ones are errors.
pub fn read_dir(dir: &Path) -> Result<(Draft, Vec<Finding>), Vec<Finding>> {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut findings = Vec::new();
    let mut visit = |relative: String, path: &Path| match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_PACK_BYTES as u64 => {
            findings.push(error(
                &relative,
                Problem::PackTooLong {
                    kib: MAX_PACK_BYTES / 1024,
                },
            ));
        }
        Ok(_) => match std::fs::read_to_string(path) {
            Ok(text) => files.push((relative, text)),
            Err(error_) => findings.push(error(
                &relative,
                Problem::PackSyntax {
                    detail: error_.to_string(),
                },
            )),
        },
        Err(error_) => findings.push(error(
            &relative,
            Problem::PackSyntax {
                detail: error_.to_string(),
            },
        )),
    };
    let entries = std::fs::read_dir(dir).map_err(|error_| {
        vec![error(
            "",
            Problem::PackSyntax {
                detail: error_.to_string(),
            },
        )]
    })?;
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    names.sort();
    for path in names {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() && name == "palettes" {
            let mut inner: Vec<_> = std::fs::read_dir(&path)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .collect()
                })
                .unwrap_or_default();
            inner.sort();
            for file in inner {
                let file_name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !file_name.starts_with('.') {
                    visit(format!("palettes/{file_name}"), &file);
                }
            }
        } else {
            visit(name, &path);
        }
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    from_files(
        files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    )
}

/// The exchange format: one JSON file made by `gono-forum-cli skin pack` (docs/skins.md, "Packs").
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pack {
    format: String,
    manifest: Manifest,
    palettes: Palettes,
    #[serde(default)]
    css: String,
}

const PACK_FORMAT: &str = "gono-forum-skin";

/// The pack of a draft: deterministic, so the same skin always has the same content hash.
pub fn pack(draft: &Draft) -> String {
    let mut text = serde_json::to_string_pretty(&Pack {
        format: PACK_FORMAT.into(),
        manifest: draft.manifest.clone(),
        palettes: draft.palettes.clone(),
        css: draft.css.clone(),
    })
    .expect("a pack always serializes");
    text.push('\n');
    text
}

pub fn from_pack(text: &str) -> Result<Draft, Vec<Finding>> {
    if text.len() > MAX_PACK_BYTES {
        return Err(vec![error(
            "",
            Problem::PackTooLong {
                kib: MAX_PACK_BYTES / 1024,
            },
        )]);
    }
    let pack: Pack = serde_json::from_str(text).map_err(|error_| {
        vec![error(
            "",
            Problem::PackSyntax {
                detail: error_.to_string(),
            },
        )]
    })?;
    if pack.format != PACK_FORMAT {
        return Err(vec![error("", Problem::PackFormat)]);
    }
    Ok(Draft {
        manifest: pack.manifest,
        palettes: pack.palettes,
        css: pack.css,
    })
}

/// SHA-256 of the pack, in hex.
pub fn content_hash(draft: &Draft) -> String {
    hex(&Sha256::digest(pack(draft).as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether every comment, string and bracket is closed: the compiled stylesheet puts the skin's text inside a layer
/// block, and an open one would swallow what comes after it.
fn closed(css: &str) -> bool {
    let mut depth = 0_i64;
    let mut chars = css.chars().peekable();
    let mut quote = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\n' {
                return false;
            }
            continue;
        }
        match c {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut star = false;
                let mut ended = false;
                for c in chars.by_ref() {
                    if star && c == '/' {
                        ended = true;
                        break;
                    }
                    star = c == '*';
                }
                if !ended {
                    return false;
                }
            }
            '"' | '\'' => quote = Some(c),
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    quote.is_none() && depth == 0
}

/// Checks a draft and, when it has no errors, compiles it. `directory` is the directory name of a built-in or
/// instance skin, which must equal the id.
pub fn check(draft: &Draft, source: Source, directory: Option<&str>) -> Checked {
    let trust = source.trust();
    let mut findings = draft.manifest.check(trust);
    if let Some(directory) = directory
        && directory != draft.manifest.id
    {
        findings.push(error(
            "skin.toml",
            Problem::ManifestDirectory {
                id: draft.manifest.id.clone(),
                directory: directory.to_owned(),
            },
        ));
    }
    let manifest_json = draft.manifest.to_json();
    if manifest_json.len() > MAX_MANIFEST_BYTES {
        findings.push(error(
            "skin.toml",
            Problem::ManifestTooLong {
                kib: MAX_MANIFEST_BYTES / 1024,
            },
        ));
    }
    let palettes_json = serde_json::to_string(&draft.palettes).expect("palettes serialize");
    if palettes_json.len() > MAX_PALETTES_BYTES {
        findings.push(error(
            "palettes",
            Problem::PaletteTooLong {
                kib: MAX_PALETTES_BYTES / 1024,
            },
        ));
    }
    // Palettes: every one the manifest names must exist; extra files are ignored with a warning.
    let mut merged = Vec::new();
    for name in draft.manifest.palette_names() {
        let file = format!("palettes/{name}.json");
        let Some(overrides) = draft.palettes.get(name) else {
            findings.push(error(
                &file,
                Problem::PaletteMissing {
                    name: name.to_owned(),
                },
            ));
            continue;
        };
        let (palette, found) = palette::merge(name, overrides, &file);
        findings.extend(found);
        findings.extend(palette::contrast(&palette, trust, &file));
        match palette::footer(&palette, &file) {
            Ok(colors) => merged.push((palette, colors)),
            Err(found) => findings.extend(found),
        }
    }
    for name in draft.palettes.names() {
        if !draft.manifest.palette_names().any(|listed| listed == name) {
            findings.push(Finding {
                file: format!("palettes/{name}.json"),
                line: None,
                severity: Severity::Warning,
                problem: Problem::PaletteUnused {
                    name: name.to_owned(),
                },
            });
        }
    }
    let mut css_findings = crate::css_check::check(
        &draft.css,
        crate::css_check::Profile::Skin(trust),
        MAX_CSS_BYTES,
        MAX_RULES,
    );
    if !closed(&draft.css) && !css_findings.iter().any(|f| f.severity == Severity::Error) {
        css_findings.push(error("", Problem::Syntax));
    }
    for finding in &mut css_findings {
        finding.file = "skin.css".into();
    }
    findings.extend(css_findings);
    if findings
        .iter()
        .any(|finding| finding.severity == Severity::Error)
    {
        return Checked::failed(findings);
    }
    let stylesheet = compile(&draft.manifest, source, &merged, &draft.css, None);
    let version_hash = hex(&Sha256::digest(stylesheet.as_bytes()))[..12].to_owned();
    let warnings = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Warning)
        .cloned()
        .collect();
    Checked {
        skin: Some(Arc::new(Skin {
            manifest: draft.manifest.clone(),
            source,
            palettes: draft.manifest.palette_names().map(str::to_owned).collect(),
            stylesheet: stylesheet.into(),
            version_hash,
            content_hash: content_hash(draft),
            warnings,
            merged: merged.into_iter().map(|(merged, _)| merged).collect(),
            css: draft.css.clone(),
            seeds: None,
            places: draft.manifest.places(),
        })),
        findings,
    }
}

/// The stylesheet of a checked skin, or of its variant with seed colors. The guard rules come first, so nothing in
/// the skin's own text can reach them; the order of the layers themselves is declared once at the top of
/// `forum.css`.
fn compile(
    manifest: &Manifest,
    source: Source,
    merged: &[(palette::Merged, [palette::FooterColors; 2])],
    css: &str,
    seeds: Option<seed::Seeds>,
) -> String {
    let colors = seeds.map_or(String::new(), |seeds| {
        format!(
            " with the seed colors {} and {}",
            seed::hex(seeds.primary),
            seed::hex(seeds.secondary)
        )
    });
    let mut out = format!(
        "/* Skin \"{}\" {} ({}){colors}, generated by Gono Forum; see docs/skins.md. */\n",
        manifest.id,
        manifest.version,
        source.as_str()
    );
    out.push_str("@layer guards {\n");
    for (index, (palette, colors)) in merged.iter().enumerate() {
        palette::footer_css(&palette.name, index == 0, colors, &mut out);
    }
    out.push_str("}\n@layer skin {\n");
    for (index, (palette, _)) in merged.iter().enumerate() {
        palette::css(palette, index == 0, &mut out);
    }
    if let Some(height) = &manifest.frame.masthead_height {
        out.push_str(&format!(":root {{\n  --masthead-h: {height};\n}}\n"));
    }
    if !css.trim().is_empty() {
        out.push_str(css);
        if !css.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str("}\n");
    out
}

/// Reads a skin directory or a pack file and checks it as a skin of `source` (`gono-forum-cli skin check` and
/// `skin pack`). The draft is `None` when the files could not be read at all.
pub fn check_path(path: &Path, source: Source) -> (Option<Draft>, Checked) {
    let read = if path.is_dir() {
        read_dir(path)
    } else {
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_PACK_BYTES as u64 => Err(vec![error(
                "",
                Problem::PackTooLong {
                    kib: MAX_PACK_BYTES / 1024,
                },
            )]),
            _ => std::fs::read_to_string(path)
                .map_err(|error_| {
                    vec![error(
                        "",
                        Problem::PackSyntax {
                            detail: error_.to_string(),
                        },
                    )]
                })
                .and_then(|text| from_pack(&text).map(|draft| (draft, Vec::new()))),
        }
    };
    match read {
        Ok((draft, mut found)) => {
            let mut checked = check(&draft, source, None);
            found.append(&mut checked.findings);
            checked.findings = found;
            (Some(draft), checked)
        }
        Err(findings) => (None, Checked::failed(findings)),
    }
}

/// A plain-text report of a check, one finding per line, for the command line.
pub fn report(checked: &Checked) -> String {
    // Command-line output is English (docs/architecture.md, "Errors").
    let t = crate::i18n::Lang::default();
    let mut out = String::new();
    for finding in &checked.findings {
        let kind = match finding.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!("{kind}: {}\n", finding.text(t)));
    }
    out.push_str(&format!(
        "{} errors, {} warnings\n",
        checked.errors().count(),
        checked.warnings().count()
    ));
    out
}

/// The built-in skins, read and checked once per process.
pub static BUILTIN_SKINS: LazyLock<Vec<(String, Checked)>> = LazyLock::new(|| {
    BUILTIN
        .iter()
        .map(|(directory, files)| {
            let checked = match from_files(files.iter().copied()) {
                Ok((draft, mut found)) => {
                    let mut checked = check(&draft, Source::Builtin, Some(directory));
                    found.append(&mut checked.findings);
                    checked.findings = found;
                    checked
                }
                Err(findings) => Checked::failed(findings),
            };
            for finding in checked.errors() {
                tracing::error!(skin = directory, finding = %finding.text(crate::i18n::Lang::default()), "built-in skin fails its checks");
            }
            ((*directory).to_owned(), checked)
        })
        .collect()
});

/// The built-in default skin. It is checked by the unit tests; should it ever fail at run time, a skin with only the
/// base palette stands in, so pages always have their colors.
pub static DEFAULT_SKIN: LazyLock<Arc<Skin>> = LazyLock::new(|| {
    for (directory, checked) in BUILTIN_SKINS.iter() {
        if directory == DEFAULT_ID
            && let Some(skin) = &checked.skin
        {
            return skin.clone();
        }
    }
    let draft = Draft {
        manifest: Manifest::from_toml(&format!(
            "contract = 1\nid = \"{DEFAULT_ID}\"\nversion = \"1.0.0\"\nname = {{ en = \"Default\" }}\n[palettes]\ndefault = \"default\"\n"
        ))
        .expect("the stand-in manifest parses"),
        palettes: Palettes(vec![("default".into(), palette::Overrides::default())]),
        css: String::new(),
    };
    let (merged, _) = palette::merge(
        "default",
        &palette::Overrides::default(),
        "palettes/default.json",
    );
    let colors = palette::footer(&merged, "palettes/default.json").ok();
    let stylesheet = match colors {
        Some(colors) => compile(
            &draft.manifest,
            Source::Builtin,
            &[(merged.clone(), colors)],
            "",
            None,
        ),
        None => {
            let mut out = String::from("@layer skin {\n");
            palette::css(&merged, true, &mut out);
            out.push_str("}\n");
            out
        }
    };
    Arc::new(Skin {
        version_hash: hex(&Sha256::digest(stylesheet.as_bytes()))[..12].to_owned(),
        content_hash: content_hash(&draft),
        manifest: draft.manifest,
        source: Source::Builtin,
        palettes: vec!["default".into()],
        stylesheet: stylesheet.into(),
        warnings: Vec::new(),
        merged: vec![merged],
        css: String::new(),
        seeds: None,
        places: None,
    })
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_skins_pass_every_rule() {
        assert!(!BUILTIN_SKINS.is_empty());
        for (directory, checked) in BUILTIN_SKINS.iter() {
            let text: Vec<String> = checked
                .findings
                .iter()
                .map(|f| f.text(crate::i18n::current()))
                .collect();
            assert!(checked.skin.is_some(), "{directory}: {text:?}");
            assert_eq!(text, Vec::<String>::new(), "{directory}");
        }
        assert_eq!(DEFAULT_SKIN.id(), DEFAULT_ID);
        assert_eq!(DEFAULT_SKIN.palettes, ["default"]);
    }

    /// Built-in skins write only English in skin.toml, and their messages say the same in English.
    #[test]
    fn built_in_names_come_from_the_translation_files() {
        let en = crate::i18n::Lang::default();
        for (directory, checked) in BUILTIN_SKINS.iter() {
            let skin = checked.skin.as_ref().expect("built-in skins are usable");
            let (name, description) =
                builtin_text(directory, en).unwrap_or_else(|| panic!("{directory} has messages"));
            assert_eq!(
                skin.manifest.name.keys().collect::<Vec<_>>(),
                ["en"],
                "{directory}"
            );
            assert_eq!(skin.manifest.name["en"], name, "{directory}");
            assert_eq!(
                skin.manifest.description.get("en").map(String::as_str),
                Some(description),
                "{directory}"
            );
            assert_eq!(skin.name(en), name);
        }
        let zh = crate::i18n::Lang::parse("zh-CN").unwrap();
        assert_eq!(DEFAULT_SKIN.name(zh), zh.skin_builtin_default_name());
        assert_ne!(DEFAULT_SKIN.name(zh), DEFAULT_SKIN.manifest.name["en"]);
    }

    #[test]
    fn the_default_skin_writes_the_base_scheme_unchanged() {
        let css = &DEFAULT_SKIN.stylesheet;
        let (merged, _) = palette::merge("default", &palette::Overrides::default(), "x");
        let mut expected = String::new();
        palette::css(&merged, true, &mut expected);
        assert!(css.contains(&expected));
        assert!(css.starts_with("/* Skin \"default\""));
        let guards = css.find("@layer guards {").unwrap();
        let skin = css.find("@layer skin {").unwrap();
        assert!(guards < skin, "the guards come first");
        // The footer guard keeps the default colors.
        assert!(css.contains(":root[data-palette=\"default\"] .colophon__legal, :root:not([data-palette]) .colophon__legal {\n  color: #57637a !important;\n  background-color: #ffffff !important;\n}"));
    }

    #[test]
    fn packs_round_trip_and_hash_their_content() {
        let draft = Draft {
            manifest: Manifest::from_toml(
                "contract = 1\nid = \"ledger\"\nversion = \"1.0.0\"\nname = { en = \"Ledger\" }\n[palettes]\ndefault = \"main\"\n",
            )
            .unwrap(),
            palettes: serde_json::from_str(r##"{"main":{"light":{"--bg":"#fff"}}}"##).unwrap(),
            css: ".pane { border: 0; }\n".into(),
        };
        let text = pack(&draft);
        assert_eq!(from_pack(&text).unwrap(), draft);
        assert_eq!(content_hash(&draft).len(), 64);
        assert!(from_pack("{\"format\":\"other\",\"manifest\":{},\"palettes\":{}}").is_err());
        assert!(matches!(
            &from_pack(&text.replace("gono-forum-skin", "zip")).unwrap_err()[0].problem,
            Problem::PackFormat
        ));
    }

    #[test]
    fn a_skin_compiles_into_layers_with_its_palettes_and_header_height() {
        let (draft, found) = from_files([
            (
                "skin.toml",
                "contract = 1\nid = \"ledger\"\nversion = \"1.0.0\"\nname = { en = \"Ledger\" }\n[palettes]\ndefault = \"main\"\nvariants = [\"night\"]\n[frame]\nmasthead_height = \"52px\"\n",
            ),
            ("palettes/main.json", r##"{"light":{"--accent":"#0f766e"}}"##),
            ("palettes/night.json", r##"{"shared":{"--accent":"#115e59"}}"##),
            ("skin.css", ".pane { border: 0; font-weight: 700; }"),
            ("README.md", "notes"),
        ])
        .unwrap();
        assert!(matches!(found[0].problem, Problem::PackFile));
        let checked = check(&draft, Source::Instance, Some("ledger"));
        let skin = checked.skin.expect("only warnings");
        assert_eq!(skin.palettes, ["main", "night"]);
        assert_eq!(skin.palette(Some("night")), "night");
        assert_eq!(skin.palette(Some("gone")), "main");
        let css = &skin.stylesheet;
        assert!(css.contains(":root[data-palette=\"main\"], :root:not([data-palette]) {"));
        assert!(css.contains(":root[data-palette=\"night\"] {"));
        assert!(css.contains("--masthead-h: 52px;"));
        assert!(
            css.trim_end()
                .ends_with(".pane { border: 0; font-weight: 700; }\n}")
        );
        let warnings: Vec<_> = skin.warnings.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(warnings, ["skin.toml", "skin.css"]);
        // The same draft as a built-in skin is refused for the same reasons, and because no messages name it.
        let builtin = check(&draft, Source::Builtin, Some("ledger"));
        assert!(builtin.skin.is_none());
        assert_eq!(builtin.errors().count(), 3);
        assert!(
            builtin
                .errors()
                .any(|f| matches!(f.problem, Problem::ManifestBuiltinMessages { .. }))
        );
        // An instance directory must be named after the id.
        assert!(
            check(&draft, Source::Instance, Some("other"))
                .skin
                .is_none()
        );
    }

    #[test]
    fn open_blocks_comments_and_strings_are_refused() {
        for css in [
            ".a { color: red;",
            ".a { color: red; } /* open",
            ".a { font-family: \"x; }",
        ] {
            let draft = Draft {
                manifest: DEFAULT_SKIN.manifest.clone(),
                palettes: Palettes(vec![("default".into(), palette::Overrides::default())]),
                css: css.into(),
            };
            assert!(
                check(&draft, Source::Instance, None).skin.is_none(),
                "{css}"
            );
        }
        assert!(closed(
            ".a { color: red; } /* ok */ .b::before { content: \"\"; }"
        ));
    }
}
