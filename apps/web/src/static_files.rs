//! Static assets under `/static` (styles, scripts, fonts, icons).
//!
//! Source builds serve `apps/web/static` from disk, so a style change needs no recompile. Release packages (cargo
//! feature `bundled`) serve the table `build.rs` compiles in, so an install is a single binary with no asset
//! directory next to it.
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, RawQuery},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::services::ServeDir;

include!(concat!(env!("OUT_DIR"), "/static_assets.rs"));

type Table = &'static [(&'static str, &'static [u8])];

#[derive(Clone, Debug)]
pub enum StaticFiles {
    Disk(PathBuf),
    /// Sorted by name (build.rs sorts them), looked up by binary search.
    Embedded(Table),
}

impl StaticFiles {
    pub fn for_root(root: &Path) -> Self {
        if crate::BUNDLED {
            Self::Embedded(EMBEDDED)
        } else {
            Self::Disk(root.join("apps/web/static"))
        }
    }

    /// One asset's bytes by its name under `/static`, e.g. `css/forum.css`.
    pub fn read(&self, name: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            Self::Disk(dir) => std::fs::read(dir.join(name)).ok().map(Cow::Owned),
            Self::Embedded(files) => lookup(files, name).map(Cow::Borrowed),
        }
    }

    /// Mounts the asset service at `/static`. `/static/i18n/<language>.js` is generated from the translations
    /// rather than read from a file (docs/architecture.md, "Languages"), in source builds and release packages alike.
    /// `version` is this process's asset version (`router::asset_version`), the only `?v=` cached for good.
    pub fn mount<S>(&self, router: Router<S>, version: Arc<str>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let catalogs = version.clone();
        let scripts = Router::new().route(
            "/i18n/{file}",
            get(
                move |UrlPath(file): UrlPath<String>, RawQuery(query): RawQuery| async move {
                    translations(&file, query.as_deref(), &catalogs)
                },
            ),
        );
        match self {
            Self::Disk(dir) => {
                router.nest_service("/static", scripts.fallback_service(ServeDir::new(dir)))
            }
            Self::Embedded(files) => {
                let files: Table = files;
                let service = scripts.route(
                    "/{*path}",
                    get(
                        move |UrlPath(path): UrlPath<String>,
                              RawQuery(query): RawQuery,
                              headers: HeaderMap| async move {
                            serve(files, &path, query.as_deref(), &headers, &version)
                        },
                    ),
                );
                router.nest_service("/static", service)
            }
        }
    }
}

/// The cache rule of an asset address. Pages link assets with `?v=<this build's asset version>`
/// (`router::asset_version`), so such an address never changes content and is cached for a year. Any other address
/// is revalidated: one without `v=` (icons, fonts loaded from CSS), and one with another build's version, which a
/// process of the old build answers with its old bytes while a rolling release is under way, or one of the new build
/// with its new bytes for a page of the old one; cached for good, either would outlast the rollout.
fn cache_rule(query: Option<&str>, version: &str) -> HeaderValue {
    let current = query.is_some_and(|query| {
        query
            .split('&')
            .any(|part| part.strip_prefix("v=") == Some(version))
    });
    HeaderValue::from_static(if current {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    })
}

/// One language's script catalog; addresses with this build's version are cached for good like the other assets (the
/// asset version hashes the catalogs too, see `router::asset_version`).
fn translations(file: &str, query: Option<&str>, version: &str) -> Response {
    let Some(lang) = file.strip_suffix(".js").and_then(crate::i18n::Lang::parse) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, cache_rule(query, version)),
        ],
        crate::i18n::script(lang),
    )
        .into_response()
}

fn lookup(files: Table, name: &str) -> Option<&'static [u8]> {
    files
        .binary_search_by(|(candidate, _)| (*candidate).cmp(name))
        .ok()
        .map(|index| files[index].1)
}

/// Strong validator per asset: the first 16 hex digits of its SHA-256, computed once per process.
fn etag(files: Table, name: &str, bytes: &[u8]) -> HeaderValue {
    use sha2::{Digest, Sha256};
    static CACHE: OnceLock<std::sync::Mutex<HashMap<(usize, String), HeaderValue>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    // Keyed by the table's address as well, so tests with their own tables never see each other's entries.
    let key = (files.as_ptr() as usize, name.to_owned());
    if let Some(value) = cache.lock().ok().and_then(|map| map.get(&key).cloned()) {
        return value;
    }
    let digest = Sha256::digest(bytes);
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let value = HeaderValue::from_str(&format!("\"{hex}\"")).expect("hex is a valid header value");
    if let Ok(mut map) = cache.lock() {
        map.insert(key, value.clone());
    }
    value
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default() {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

fn matches(if_none_match: &HeaderValue, etag: &HeaderValue) -> bool {
    let Ok(value) = if_none_match.to_str() else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.trim_start_matches("W/").as_bytes() == etag.as_bytes()
    })
}

fn serve(
    files: Table,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    version: &str,
) -> Response {
    let Some(bytes) = lookup(files, path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = etag(files, path, bytes);
    // Revalidated with the ETag unless the address carries this build's version ([`cache_rule`]).
    let cache = cache_rule(query, version);
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| matches(value, &etag))
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache)],
        )
            .into_response();
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(path)),
            ),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache),
        ],
        Body::from(bytes),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    const FILES: Table = &[
        ("css/forum.css", b"body{color:red}"),
        ("fonts/a.woff2", b"wOF2"),
        ("js/site.js", b"void 0"),
    ];

    /// The asset version the test service was started with.
    const VERSION: &str = "abc";

    async fn get(uri: &str, if_none_match: Option<&str>) -> Response {
        let mut request = Request::get(uri);
        if let Some(value) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, value);
        }
        StaticFiles::Embedded(FILES)
            .mount(Router::new(), VERSION.into())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[test]
    fn the_source_build_table_is_empty() {
        // Tests run without the bundled feature: the generated table must not drag the asset directory into every
        // test binary.
        if !crate::BUNDLED {
            assert!(EMBEDDED.is_empty());
        }
        assert!(EMBEDDED.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[tokio::test]
    async fn embedded_assets_have_types_validators_and_cache_rules() {
        let response = get("/static/css/forum.css?v=abc", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        let etag = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(etag.starts_with('"') && etag.len() == 18, "{etag}");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"body{color:red}");

        let response = get("/static/fonts/a.woff2", None).await;
        assert_eq!(response.headers()[header::CONTENT_TYPE], "font/woff2");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");

        for validator in [etag.clone(), format!("W/{etag}"), format!("\"x\", {etag}")] {
            let response = get("/static/css/forum.css", Some(&validator)).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{validator}");
            assert_eq!(response.headers()[header::ETAG], etag.as_str());
        }
        assert_eq!(
            get("/static/css/forum.css", Some("\"other\""))
                .await
                .status(),
            StatusCode::OK
        );
    }

    /// During a rolling release a process of one build can be asked for an address with another build's version:
    /// it answers with its own bytes, which must not be cached for good under that address.
    #[tokio::test]
    async fn only_this_builds_version_is_cached_for_good() {
        let immutable = "public, max-age=31536000, immutable";
        for (uri, rule) in [
            ("/static/js/site.js?v=abc", immutable),
            ("/static/js/site.js?x=1&v=abc", immutable),
            ("/static/js/site.js?v=abc&x=1", immutable),
            ("/static/js/site.js?v=def", "no-cache"),
            ("/static/js/site.js?v=abcd", "no-cache"),
            ("/static/js/site.js?v=", "no-cache"),
            ("/static/js/site.js?x=abc", "no-cache"),
            ("/static/i18n/en.js?v=abc", immutable),
            ("/static/i18n/en.js?v=def", "no-cache"),
            ("/static/i18n/en.js", "no-cache"),
        ] {
            let response = get(uri, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], rule, "{uri}");
        }
        // The revalidation of an old version's address keeps that rule too.
        let etag = get("/static/js/site.js", None).await.headers()[header::ETAG].clone();
        let response = get("/static/js/site.js?v=def", Some(etag.to_str().unwrap())).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    }

    #[tokio::test]
    async fn only_listed_names_are_served() {
        for uri in [
            "/static/css/missing.css",
            "/static/css",
            "/static/",
            "/static/../css/forum.css",
            "/static/css/%2e%2e/css/forum.css",
            "/static/./css/forum.css",
            "/static/CSS/forum.css",
        ] {
            assert_eq!(
                get(uri, None).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
        // Same as ServeDir: an empty segment still names the same file.
        for uri in ["/static/js/site.js", "/static//css/forum.css"] {
            assert_eq!(get(uri, None).await.status(), StatusCode::OK, "{uri}");
        }
    }

    #[test]
    fn read_finds_embedded_and_disk_assets() {
        let embedded = StaticFiles::Embedded(FILES);
        assert_eq!(embedded.read("js/site.js").as_deref(), Some(&b"void 0"[..]));
        assert!(embedded.read("js/none.js").is_none());
        let disk = StaticFiles::Disk(crate::project_root().join("apps/web/static"));
        assert!(disk.read("js/site.js").is_some());
    }
}
