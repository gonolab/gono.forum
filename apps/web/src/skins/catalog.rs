//! Every skin of the three sources and which one of each id is usable (docs/skins.md, "Sources").
//!
//! Built-in skins win over imported ones, and imported ones over the instance directory. The first source that has
//! an id decides it: when that skin fails its checks the id has no usable skin at all, rather than silently falling
//! to a lower source the administrator may not expect.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::{Checked, Skin, Source};
use crate::css_check::{Finding, Severity};

/// One skin as the admin list shows it.
#[derive(Clone, Debug)]
pub struct Listed {
    pub id: String,
    pub source: Source,
    pub status: Status,
    /// The manifest, when it could be read.
    pub manifest: Option<super::Manifest>,
    /// The current errors and warnings.
    pub findings: Vec<Finding>,
    pub skin: Option<Arc<Skin>>,
    /// Imported skins: their row.
    pub imported: Option<Imported>,
}

#[derive(Clone, Debug)]
pub struct Imported {
    pub at: i64,
    pub by: Option<i64>,
    /// The row's concurrency version.
    pub row_version: i64,
    /// The warnings the administrator confirmed when importing.
    pub confirmed: Vec<Finding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Available,
    /// A skin with the same id from a higher source is the one used.
    Shadowed(Source),
    /// The skin fails its checks (the findings say why).
    Invalid,
}

impl Listed {
    pub fn from_checked(
        id: &str,
        source: Source,
        checked: Checked,
        manifest: Option<super::Manifest>,
    ) -> Self {
        Self {
            id: id.to_owned(),
            source,
            status: if checked.skin.is_some() {
                Status::Available
            } else {
                Status::Invalid
            },
            manifest: checked
                .skin
                .as_ref()
                .map(|skin| skin.manifest.clone())
                .or(manifest),
            findings: checked.findings,
            skin: checked.skin,
            imported: None,
        }
    }

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

    pub fn name(&self, t: crate::i18n::Lang) -> String {
        self.manifest.as_ref().map_or_else(
            || self.id.clone(),
            |manifest| super::display_name(manifest, self.source, t),
        )
    }

    pub fn version(&self) -> &str {
        self.manifest
            .as_ref()
            .map_or("", |manifest| manifest.version.as_str())
    }
}

/// All skins, built-in first, then imported, then the instance directory, each source by id.
#[derive(Debug, Default)]
pub struct Catalog {
    pub listed: Vec<Listed>,
    /// Colored variants of usable skins (docs/styles.md, "Seed colors"), compiled on first use and dropped with the
    /// catalog: the ones pages of the site and previews ask for, at most [`VARIANTS`], the oldest going first.
    variants: Mutex<Vec<Arc<Skin>>>,
    /// How many variants this catalog has compiled; the tests check with it what compiles and what does not.
    compiled: AtomicU64,
}

/// How many colored variants a catalog keeps: enough for the site, every domain with its own colors or skin, and a
/// few previews.
const VARIANTS: usize = 32;

impl Catalog {
    /// Joins the three sources and marks every skin that a higher source shadows.
    pub fn build(mut listed: Vec<Listed>) -> Self {
        listed.sort_by(|a, b| (a.source, &a.id).cmp(&(b.source, &b.id)));
        let mut claimed: Vec<(String, Source)> = Vec::new();
        for entry in &mut listed {
            if let Some((_, by)) = claimed.iter().find(|(id, _)| *id == entry.id) {
                entry.status = Status::Shadowed(*by);
            } else {
                claimed.push((entry.id.clone(), entry.source));
            }
        }
        Self {
            listed,
            variants: Mutex::default(),
            compiled: AtomicU64::new(0),
        }
    }

    /// The usable skin with this id.
    pub fn get(&self, id: &str) -> Option<&Arc<Skin>> {
        self.listed
            .iter()
            .find(|entry| entry.id == id && entry.status == Status::Available)
            .and_then(|entry| entry.skin.as_ref())
    }

    /// `skin` with the seed colors, or `skin` itself without them. `remember` keeps a newly compiled variant for the
    /// next request; the stylesheet route passes `false` for an administrator's own preview, so it cannot push out
    /// the variants pages need. Nothing else compiles a variant: the stylesheet route serves colors that no page uses
    /// and no preview of the requester shows without them.
    pub fn colored(
        &self,
        skin: &Arc<Skin>,
        seeds: Option<super::seed::Seeds>,
        remember: bool,
    ) -> Arc<Skin> {
        let Some(seeds) = seeds else {
            return skin.clone();
        };
        let found = |variants: &[Arc<Skin>]| {
            variants
                .iter()
                .find(|variant| {
                    variant.id() == skin.id()
                        && variant.source == skin.source
                        && variant.seeds == Some(seeds)
                })
                .cloned()
        };
        if let Some(variant) = found(&self.variants.lock().unwrap_or_else(PoisonError::into_inner))
        {
            return variant;
        }
        // Compiled outside the lock; two requests racing for the same variant compile it twice and keep one.
        self.compiled.fetch_add(1, Ordering::Relaxed);
        let variant = Arc::new(skin.colored(seeds));
        if remember {
            let mut variants = self.variants.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = found(&variants) {
                return existing;
            }
            if variants.len() >= VARIANTS {
                variants.remove(0);
            }
            variants.push(variant.clone());
        }
        variant
    }

    /// The usable skin or kept colored variant whose stylesheet hash is `hash` (`Skin::version_hash`).
    pub fn by_hash(&self, hash: &str) -> Option<Arc<Skin>> {
        self.usable()
            .find(|skin| skin.version_hash == hash)
            .cloned()
            .or_else(|| {
                self.variants
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|variant| variant.version_hash == hash)
                    .cloned()
            })
    }

    /// How many colored variants this catalog has compiled so far.
    pub fn compiled(&self) -> u64 {
        self.compiled.load(Ordering::Relaxed)
    }

    pub fn usable(&self) -> impl Iterator<Item = &Arc<Skin>> {
        self.listed
            .iter()
            .filter(|entry| entry.status == Status::Available)
            .filter_map(|entry| entry.skin.as_ref())
    }
}

/// The built-in skins as listed entries.
pub fn builtin() -> Vec<Listed> {
    super::BUILTIN_SKINS
        .iter()
        .map(|(directory, checked)| Listed {
            id: directory.clone(),
            source: Source::Builtin,
            status: if checked.skin.is_some() {
                Status::Available
            } else {
                Status::Invalid
            },
            manifest: checked.skin.as_ref().map(|skin| skin.manifest.clone()),
            findings: checked.findings.clone(),
            skin: checked.skin.clone(),
            imported: None,
        })
        .collect()
}

/// Reads every skin under the instance directory (`$GONOF_HOME/skins/<id>/`). A missing directory has no skins;
/// skins that fail their checks are listed as invalid and logged.
pub fn scan(dir: &Path) -> Vec<Listed> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::error!(dir = %dir.display(), %error, "cannot read the instance skins directory");
            return Vec::new();
        }
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    paths.sort();
    let mut listed = Vec::new();
    for path in paths {
        let directory = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if directory.starts_with('.') {
            continue;
        }
        let (checked, manifest) = match super::read_dir(&path) {
            Ok((draft, mut found)) => {
                let mut checked = super::check(&draft, Source::Instance, Some(&directory));
                found.append(&mut checked.findings);
                checked.findings = found;
                (checked, Some(draft.manifest))
            }
            Err(findings) => (
                Checked {
                    skin: None,
                    findings,
                },
                None,
            ),
        };
        // Logs are English, whatever language the request that loaded the catalog has.
        let t = crate::i18n::Lang::default();
        for finding in checked.errors() {
            tracing::error!(skin = %directory, finding = %finding.text(t), "instance skin is unusable");
        }
        listed.push(Listed::from_checked(
            &directory,
            Source::Instance,
            checked,
            manifest,
        ));
    }
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, id: &str, extra: &str) {
        let skin = dir.join(id);
        std::fs::create_dir_all(skin.join("palettes")).unwrap();
        std::fs::write(
            skin.join("skin.toml"),
            format!("contract = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = {{ en = \"{id}\" }}\n[palettes]\ndefault = \"main\"\n{extra}"),
        )
        .unwrap();
        std::fs::write(skin.join("palettes/main.json"), "{}").unwrap();
    }

    #[test]
    fn higher_sources_shadow_lower_ones_and_broken_skins_are_listed() {
        let dir = std::env::temp_dir().join(format!("gonof-skins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "default", "");
        write(&dir, "ruled", "");
        write(&dir, "broken", "[frame]\nnav = \"sideways\"\n");
        std::fs::create_dir_all(dir.join("misnamed")).unwrap();
        std::fs::copy(dir.join("ruled/skin.toml"), dir.join("misnamed/skin.toml")).unwrap();
        let mut listed = builtin();
        listed.extend(scan(&dir));
        let catalog = Catalog::build(listed);
        let status = |id: &str, source| {
            catalog
                .listed
                .iter()
                .find(|entry| entry.id == id && entry.source == source)
                .map(|entry| entry.status)
        };
        assert_eq!(status("default", Source::Builtin), Some(Status::Available));
        assert_eq!(
            status("default", Source::Instance),
            Some(Status::Shadowed(Source::Builtin))
        );
        assert_eq!(status("ruled", Source::Instance), Some(Status::Available));
        assert_eq!(status("broken", Source::Instance), Some(Status::Invalid));
        assert_eq!(status("misnamed", Source::Instance), Some(Status::Invalid));
        assert_eq!(catalog.get("default").unwrap().source, Source::Builtin);
        assert_eq!(catalog.get("ruled").unwrap().source, Source::Instance);
        assert!(catalog.get("broken").is_none());
        assert!(scan(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
