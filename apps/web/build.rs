//! With the `bundled` feature (release packages), compiles `static/` into the binary: generates
//! `$OUT_DIR/static_assets.rs` with one `include_bytes!` per file. Source builds generate an empty table and keep
//! reading the directory at run time.
//!
//! Always compiles the built-in skins under `skins/` and the base color scheme (docs/skins.md, "Sources") into
//! `$OUT_DIR/skins.rs`.
//!
//! Always compiles the translations under `locales/` (docs/architecture.md, "Languages"): generates
//! `$OUT_DIR/locales.rs` with every language's files, the languages' sample content and one typed method per base
//! (en) message, so a wrong key or argument count fails to compile.
#[path = "src/i18n/syntax.rs"]
mod syntax;

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

/// Build outputs of `npm run build` that are not in Git; a package without them would serve broken pages.
const REQUIRED: &[&str] = &["css/forum.css", "css/debug.css", "js/composer.js"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GONOF_BUILD_VERSION");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut table = String::from("pub(crate) static EMBEDDED: &[(&str, &[u8])] = &[\n");
    if env::var_os("CARGO_FEATURE_BUNDLED").is_some() {
        let root = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
        )
        .join("static");
        println!("cargo:rerun-if-changed={}", root.display());
        for required in REQUIRED {
            assert!(
                root.join(required).is_file(),
                "static/{required} is missing: run `npm ci --ignore-scripts && npm run build` before building with the bundled feature"
            );
        }
        let mut files = Vec::new();
        collect(&root, &root, &mut files);
        files.sort();
        for (name, path) in files {
            println!("cargo:rerun-if-changed={}", path.display());
            writeln!(
                table,
                "    ({name:?}, include_bytes!({:?})),",
                path.display().to_string()
            )
            .expect("writing to a String cannot fail");
        }
    }
    table.push_str("];\n");
    fs::write(out.join("static_assets.rs"), table).expect("cannot write static_assets.rs");
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    fs::write(out.join("locales.rs"), locales(&manifest.join("locales")))
        .expect("cannot write locales.rs");
    fs::write(
        out.join("skins.rs"),
        skins(
            &manifest.join("skins"),
            &manifest.join("frontend/styles/themes/default.json"),
        ),
    )
    .expect("cannot write skins.rs");
}

/// The built-in skins (docs/skins.md, "Sources"): `skins/<id>/` with `skin.toml`, `palettes/<name>.json` and an
/// optional `skin.css`, embedded like the translations; and the base color scheme every palette overrides. The
/// skins are checked by the unit tests and when the program starts; here only their layout is enforced.
fn skins(root: &Path, base: &Path) -> String {
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed={}", base.display());
    let mut code = format!(
        "pub(crate) static BASE_PALETTE: &str = include_str!({:?});\n",
        base.display().to_string()
    );
    code.push_str("pub(crate) static BUILTIN: &[(&str, &[(&str, &str)])] = &[\n");
    let mut directories: Vec<PathBuf> = fs::read_dir(root)
        .expect("cannot read the built-in skins directory")
        .map(|entry| entry.expect("cannot read a skins entry").path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    assert!(
        directories
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == "default")),
        "skins/default is missing: every site falls back to it"
    );
    for directory in directories {
        println!("cargo:rerun-if-changed={}", directory.display());
        let id = directory
            .file_name()
            .and_then(|name| name.to_str())
            .expect("skin directory names must be UTF-8")
            .to_owned();
        let mut files = Vec::new();
        for entry in fs::read_dir(&directory).expect("cannot read a skin directory") {
            let path = entry.expect("cannot read a skin file entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("skin file names must be UTF-8")
                .to_owned();
            if path.is_dir() {
                assert_eq!(
                    name, "palettes",
                    "skins/{id}/{name}: only palettes/ may be a directory"
                );
                for palette in fs::read_dir(&path).expect("cannot read a palettes directory") {
                    let palette = palette.expect("cannot read a palette entry").path();
                    let file = palette
                        .file_name()
                        .and_then(|name| name.to_str())
                        .expect("palette file names must be UTF-8")
                        .to_owned();
                    assert!(
                        file.ends_with(".json"),
                        "skins/{id}/palettes/{file}: palettes are .json files"
                    );
                    files.push((format!("palettes/{file}"), palette));
                }
            } else {
                assert!(
                    matches!(name.as_str(), "skin.toml" | "skin.css"),
                    "skins/{id}/{name}: a skin holds skin.toml, skin.css and palettes/ only"
                );
                files.push((name, path));
            }
        }
        files.sort();
        write!(code, "    ({id:?}, &[").expect("writing to a String cannot fail");
        for (name, path) in files {
            println!("cargo:rerun-if-changed={}", path.display());
            write!(
                code,
                "({name:?}, include_str!({:?})), ",
                path.display().to_string()
            )
            .expect("writing to a String cannot fail");
        }
        code.push_str("]),\n");
    }
    code.push_str("];\n");
    code
}

/// The base language: every key the code uses exists in it, and other languages fall back to it.
const BASE: &str = "en";

/// A language's sample content, next to its translation files.
const SAMPLE_FILE: &str = "sample.json";

/// `locales/<tag>/*.ftl`: one directory per language, the base first, then the others by tag.
fn languages(root: &Path) -> Vec<(String, Vec<(String, PathBuf)>)> {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut languages = Vec::new();
    for entry in fs::read_dir(root).expect("cannot read the locales directory") {
        let path = entry.expect("cannot read a locales entry").path();
        if !path.is_dir() {
            continue;
        }
        let tag = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("language directory names must be UTF-8")
            .to_owned();
        let mut files = Vec::new();
        for file in fs::read_dir(&path).expect("cannot read a language directory") {
            let file = file.expect("cannot read a translation file entry").path();
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .expect("translation file names must be UTF-8")
                .to_owned();
            if name.ends_with(".ftl") {
                println!("cargo:rerun-if-changed={}", file.display());
                files.push((name, file));
            }
        }
        files.sort();
        languages.push((tag, files));
    }
    languages.sort_by(|(a, _), (b, _)| (a != BASE, a).cmp(&(b != BASE, b)));
    assert!(
        languages.first().is_some_and(|(tag, _)| tag == BASE),
        "locales/{BASE} is missing: it is the base every other language falls back to"
    );
    assert!(
        languages.len() <= usize::from(u8::MAX),
        "too many languages"
    );
    languages
}

/// One generated method: the message key (with `.attribute`), its variables and the base text for the doc comment.
struct Method {
    key: String,
    variables: Vec<String>,
    text: String,
}

fn locales(root: &Path) -> String {
    use fluent_syntax::ast::Entry;
    let languages = languages(root);
    let mut code = String::from("pub(super) static SOURCES: &[(&str, &[(&str, &str)])] = &[\n");
    for (tag, files) in &languages {
        write!(code, "    ({tag:?}, &[").expect("writing to a String cannot fail");
        for (name, path) in files {
            write!(
                code,
                "({name:?}, include_str!({:?})), ",
                path.display().to_string()
            )
            .expect("writing to a String cannot fail");
        }
        code.push_str("]),\n");
    }
    code.push_str("];\n");
    // The setup wizard's sample content (docs/translating.md, "Sample content"): `locales/<tag>/sample.json`, the
    // base's complete and every other one a translation of it. Languages without the file offer no sample.
    code.push_str("pub(crate) static SAMPLES: &[(&str, &str)] = &[\n");
    for (tag, _) in &languages {
        let path = root.join(tag).join(SAMPLE_FILE);
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            writeln!(
                code,
                "    ({tag:?}, include_str!({:?})),",
                path.display().to_string()
            )
            .expect("writing to a String cannot fail");
        } else {
            assert!(
                tag != BASE,
                "locales/{BASE}/{SAMPLE_FILE} is missing: every translated sample is checked against it"
            );
        }
    }
    code.push_str("];\n");
    let mut methods: Vec<Method> = Vec::new();
    for (name, path) in &languages[0].1 {
        let source = fs::read_to_string(path).expect("cannot read a base translation file");
        let resource =
            fluent_syntax::parser::parse(source.as_str()).unwrap_or_else(|(_, errors)| {
                panic!("locales/{BASE}/{name} does not parse: {errors:?}")
            });
        for entry in &resource.body {
            let message = match entry {
                Entry::Message(message) => message,
                Entry::Junk { content } => {
                    panic!("locales/{BASE}/{name}: cannot parse {content:?}")
                }
                _ => continue,
            };
            let key = message.id.name;
            assert!(
                syntax::valid_key(key),
                "locales/{BASE}/{name}: bad key {key:?}"
            );
            let mut push = |key: String, pattern: &fluent_syntax::ast::Pattern<&str>| {
                let variables: Vec<String> = syntax::variables(pattern)
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                for variable in &variables {
                    assert!(
                        syntax::valid_variable(variable),
                        "locales/{BASE}/{name}: {key} uses a bad variable name ${variable}"
                    );
                }
                let text = syntax::texts(pattern).concat();
                methods.push(Method {
                    key,
                    variables,
                    text,
                });
            };
            if let Some(value) = &message.value {
                push(key.to_owned(), value);
            }
            for attribute in &message.attributes {
                push(format!("{key}.{}", attribute.id.name), &attribute.value);
            }
        }
    }
    let mut seen = std::collections::BTreeMap::new();
    for method in &methods {
        let name = syntax::method_name(&method.key);
        if let Some(other) = seen.insert(name.clone(), method.key.clone()) {
            panic!("{other} and {} both generate the method {name}", method.key);
        }
    }
    code.push_str("pub(super) static KEYS: &[&str] = &[\n");
    for method in &methods {
        writeln!(code, "    {:?},", method.key).expect("writing to a String cannot fail");
    }
    code.push_str("];\n\nimpl Lang {\n");
    for (index, method) in methods.iter().enumerate() {
        let name = syntax::method_name(&method.key);
        let markup = method
            .variables
            .iter()
            .any(|variable| syntax::is_markup(variable));
        let parameters: String = method
            .variables
            .iter()
            .map(|variable| {
                let parameter = syntax::parameter_name(variable);
                if syntax::is_markup(variable) {
                    format!(", {parameter}: Markup")
                } else {
                    format!(", {parameter}: impl Into<Arg>")
                }
            })
            .collect();
        writeln!(
            code,
            "    #[doc = {:?}]",
            format!("`{}` — en: {}", method.key, method.text.trim())
        )
        .expect("writing to a String cannot fail");
        if markup {
            let (plain, slots): (Vec<&String>, Vec<&String>) = method
                .variables
                .iter()
                .partition(|variable| !syntax::is_markup(variable));
            let plain: String = plain
                .iter()
                .map(|variable| {
                    format!(
                        "({variable:?}, {}.into()), ",
                        syntax::parameter_name(variable)
                    )
                })
                .collect();
            let slots: String = slots
                .iter()
                .map(|variable| format!("({variable:?}, {}), ", syntax::parameter_name(variable)))
                .collect();
            writeln!(
                code,
                "    pub fn {name}(self{parameters}) -> Markup {{ self.format_markup({:?}, vec![{plain}], vec![{slots}]) }}",
                method.key
            )
            .expect("writing to a String cannot fail");
        } else if method.variables.is_empty() {
            // Messages without variables are `&'static str` (docs/architecture.md, "Languages"): formatted once
            // per language and kept for the life of the process, so labels and error payloads keep their
            // `&'static str` types.
            writeln!(
                code,
                "    pub fn {name}(self) -> &'static str {{ self.fixed({index}) }}"
            )
            .expect("writing to a String cannot fail");
        } else {
            let arguments: String = method
                .variables
                .iter()
                .map(|variable| {
                    format!(
                        "({variable:?}, {}.into()), ",
                        syntax::parameter_name(variable)
                    )
                })
                .collect();
            writeln!(
                code,
                "    pub fn {name}(self{parameters}) -> String {{ self.format({:?}, vec![{arguments}]) }}",
                method.key
            )
            .expect("writing to a String cannot fail");
        }
    }
    code.push_str("}\n");
    code
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).expect("cannot read the static directory") {
        let path = entry.expect("cannot read a static directory entry").path();
        let name = path
            .strip_prefix(root)
            .expect("entries are under the root")
            .to_str()
            .expect("static file names must be UTF-8")
            .replace('\\', "/");
        if path.is_dir() {
            collect(root, &path, files);
        } else if !name
            .rsplit('/')
            .next()
            .is_some_and(|file| file.starts_with('.'))
        {
            files.push((name, path));
        }
    }
}
