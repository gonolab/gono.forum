use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use url::Url;

/// Deployment configuration: only deployment facts and functional settings of the same kind
/// (docs/settings-and-caching.md, "Where settings live").
///
/// Site name, description, forum URL, debug panel, posting quotas, growth points, member API, retention periods,
/// leaderboard refresh interval and other operational items are admin-area settings (the `settings.*` rows of
/// `gonof_common_settings`), read only from the database and its process cache, never from here.
/// Forums are not created from configuration: a fresh install starts from an empty database and the administrator
/// creates them by hand at `/manage/forums`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub redis: RedisConfig,
    #[serde(default)]
    pub identity: crate::identity::IdentityConfig,
    /// Only the attachment count `max_attachments` remains (attachments are a functional setting of the same kind).
    #[serde(default)]
    pub content: crate::content::writes::WriteConfig,
    #[serde(default)]
    pub media: crate::media::MediaConfig,
    /// Encrypts the external-service secrets entered in the admin area (SMTP authorization code, SMS AccessKey Secret).
    /// Its old name `[mail]` is rejected explicitly when reading the configuration, see [`Config::read_files`].
    #[serde(default)]
    pub common: crate::secrets::CommonConfig,
    /// Where the admin area looks for new releases (one-click upgrade).
    #[serde(default)]
    pub update: crate::updates::UpdateConfig,
    /// Google and GitHub endpoints. Not read from the configuration file (`serde(skip)`; writing it into the file is
    /// rejected at startup as an unknown field); production uses the defaults. Only tests replace it in code with a
    /// local fake service.
    #[serde(skip)]
    pub oauth: crate::oauth::Endpoints,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout_seconds: u64,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("url", &"[redacted]")
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout_seconds", &self.acquire_timeout_seconds)
            .finish()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: 5,
            acquire_timeout_seconds: 10,
        }
    }
}

impl DatabaseConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.url.trim().is_empty(),
            crate::i18n::current().setup_config_database_missing()
        );
        match crate::db::Backend::of(&self.url) {
            Some(crate::db::Backend::MySql) => {
                let url = Url::parse(&self.url)
                    .context(crate::i18n::current().setup_config_database_url())?;
                ensure!(
                    url.host_str().is_some(),
                    crate::i18n::current().setup_config_database_host()
                );
                ensure!(
                    !url.path().trim_matches('/').is_empty() && url.fragment().is_none(),
                    crate::i18n::current().setup_config_database_name()
                );
            }
            Some(crate::db::Backend::Sqlite) => {
                // Files only: every connection to an in-memory database gets its own copy, so the forum's data would
                // vanish with the connection. The file is read as the driver reads it (percent-decoded).
                let file = crate::db::sqlite::file_of(&self.url)
                    .context(crate::i18n::current().setup_config_sqlite_form())?;
                let parameters = self
                    .url
                    .split_once('?')
                    .map_or("", |(_, parameters)| parameters);
                ensure!(
                    !file.as_os_str().is_empty()
                        && file != Path::new(":memory:")
                        && !parameters.contains("mode=memory"),
                    crate::i18n::current().setup_config_sqlite_memory()
                );
            }
            None => anyhow::bail!(crate::i18n::current().setup_config_database_scheme()),
        }
        ensure!(
            self.max_connections > 0,
            crate::i18n::current().setup_config_max_connections()
        );
        ensure!(
            self.acquire_timeout_seconds > 0,
            crate::i18n::current().setup_config_acquire_timeout()
        );
        Ok(())
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisConfig {
    /// When disabled, Redis is not connected, background jobs no longer wait for it, and cache reads and writes all
    /// pass through to MySQL.
    ///
    /// Deployment-shape switch: disabled for single-machine single-process, enabled for multi-process or multi-machine
    /// multi-process; local SQL and timing profiling also disables it. Once enabled, Redis is a hard dependency, see
    /// `ensure_available` in cache.rs and the startup order in main.rs.
    pub enabled: bool,
    pub url: String,
    pub namespace: String,
    pub timeout_milliseconds: u64,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: String::new(),
            namespace: "gonof".into(),
            timeout_milliseconds: 2000,
        }
    }
}

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisConfig")
            .field("enabled", &self.enabled)
            .field("url", &"[redacted]")
            .field("namespace", &self.namespace)
            .field("timeout_milliseconds", &self.timeout_milliseconds)
            .finish()
    }
}

impl RedisConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            // When disabled the connection string is not validated: neither single-process deployments nor local
            // profiling require Redis to be filled in.
            return Ok(());
        }
        ensure!(
            !self.url.trim().is_empty(),
            crate::i18n::current().setup_config_redis_missing()
        );
        let url = Url::parse(&self.url).context(crate::i18n::current().setup_config_redis_url())?;
        ensure!(
            matches!(url.scheme(), "redis" | "rediss")
                && url.host_str().is_some()
                && url.fragment().is_none(),
            crate::i18n::current().setup_config_redis_scheme()
        );
        ensure!(
            !self.namespace.is_empty()
                && self.namespace.len() <= 120
                && self
                    .namespace
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'-')),
            crate::i18n::current().setup_config_redis_namespace()
        );
        ensure!(
            self.timeout_milliseconds > 0,
            crate::i18n::current().setup_config_redis_timeout()
        );
        Ok(())
    }
}

/// Validates and normalizes a site origin URL: a full HTTP(S) URL with a host, no username, password, query or
/// fragment, and a root path; returns it without the trailing slash.
///
/// The forum URL editable in the admin area and the URL derived from the request `Host` share this one rule; the
/// deployment configuration has no site URL.
pub fn normalize_site_url(value: &str) -> Result<String> {
    let url = Url::parse(value).context(crate::i18n::current().setup_config_site_url())?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        crate::i18n::current().setup_config_site_scheme()
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        crate::i18n::current().setup_config_site_origin()
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Validates and normalizes the forum URL (`settings.site.url`): an origin plus an optional path prefix, used for
/// sub-path deployment. Returns it without the trailing slash, e.g. `https://demo.com/bbs`.
///
/// The origin part reuses [`normalize_site_url`]; the path may have several levels, each segment only lowercase
/// letters, digits and `-`, at most 64 long (same as forum slugs); no empty segments, `.`, `%`, `\`, query strings or
/// fragments. The path is checked as written and not handed to the URL parser, which would simplify `..`.
pub fn normalize_forum_url(value: &str) -> Result<String> {
    let scheme_end = value
        .find("://")
        .context(crate::i18n::current().setup_config_forum_url())?;
    let rest = &value[scheme_end + 3..];
    let cut = rest
        .find(['/', '?', '#', '\\'])
        .map_or(value.len(), |at| scheme_end + 3 + at);
    let (origin, path) = value.split_at(cut);
    ensure!(
        path.is_empty() || path.starts_with('/'),
        crate::i18n::current().setup_config_forum_query()
    );
    let origin = normalize_site_url(origin)?;
    let path = path.trim_end_matches('/');
    ensure!(
        path.is_empty() || path_prefix_shape(path),
        crate::i18n::current().setup_config_forum_path()
    );
    Ok(format!("{origin}{path}"))
}

/// Shape of a path prefix: starts with `/`, one or more segments, each 1–64 lowercase letters, digits or `-`.
pub fn path_prefix_shape(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|rest| {
        rest.split('/').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 64
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    })
}

/// Built-in defaults (`config/default.toml`), compiled in so a release package needs no file next to the binary.
/// They are deployment defaults, not a place for site-specific values: those go in the private configuration.
const DEFAULTS: &str = include_str!("../../../config/default.toml");

impl Config {
    /// The private configuration read when `GONOF_CONFIG` is not set: release packages read
    /// `config/gono-forum.toml` under the instance directory (written by the setup wizard), source builds keep
    /// `config/development.toml` in the source tree.
    pub fn default_path(root: &Path) -> PathBuf {
        root.join(if crate::BUNDLED {
            "config/gono-forum.toml"
        } else {
            "config/development.toml"
        })
    }

    pub fn load(root: &Path, override_path: Option<&Path>) -> Result<Self> {
        Self::read(root, override_path)?.validated()
    }

    /// Reads the defaults, the private configuration and the `GONOF_*` overrides without validating them. The web
    /// entry calls this first: an instance whose `database.url` is still empty has not been set up and opens the
    /// setup wizard instead of failing validation.
    pub fn read(root: &Path, override_path: Option<&Path>) -> Result<Self> {
        let local = Self::default_path(root);
        let path = override_path.unwrap_or(&local);
        let mut config = Self::read_files(path, override_path.is_some())?;
        if let Some(url) = std::env::var_os("GONOF_DATABASE_URL") {
            config.database.url = url
                .into_string()
                .map_err(|_| anyhow::anyhow!("GONOF_DATABASE_URL must be UTF-8"))?;
        }
        if let Some(url) = std::env::var_os("GONOF_REDIS_URL") {
            config.redis.url = url
                .into_string()
                .map_err(|_| anyhow::anyhow!("GONOF_REDIS_URL must be UTF-8"))?;
        }
        if let Some(namespace) = std::env::var_os("GONOF_REDIS_NAMESPACE") {
            config.redis.namespace = namespace
                .into_string()
                .map_err(|_| anyhow::anyhow!("GONOF_REDIS_NAMESPACE must be UTF-8"))?;
        }
        config.apply_cookie_overrides(
            std::env::var_os("GONOF_AUTH_COOKIE_KEY").as_deref(),
            std::env::var_os("GONOF_AUTH_COOKIE_PREVIOUS_KEYS").as_deref(),
        )?;
        config.apply_two_factor_overrides(
            std::env::var_os("GONOF_TWO_FACTOR_KEY").as_deref(),
            std::env::var_os("GONOF_TWO_FACTOR_PREVIOUS_KEYS").as_deref(),
        )?;
        // A relative SQLite file is in the instance directory, wherever the command runs (docs/releasing.md, "How the
        // binaries are built"), whether it comes from the file or from GONOF_DATABASE_URL.
        config.database.url = crate::db::sqlite::resolve(&config.database.url, root)?;
        Ok(config)
    }

    /// The test connection reads only the test configuration and GONOF_TEST_*, so it never picks up development or
    /// production connections by mistake. The configuration file is optional: environment variables alone are enough.
    pub fn load_test(root: &Path) -> Result<Self> {
        const MISSING: &str = "the separate test services are not configured: fill in config/test.toml or set \
            GONOF_TEST_REDIS_URL, and GONOF_TEST_MYSQL_URL unless GONOF_TEST_BACKEND=sqlite; tests do not fall back \
            or skip";
        let override_path = std::env::var_os("GONOF_TEST_CONFIG").map(std::path::PathBuf::from);
        let local = root.join("config/test.toml");
        let path = override_path.as_deref().unwrap_or(&local);
        let mut config = Self::read_files(path, override_path.is_some())?;
        for (key, target) in [
            ("GONOF_TEST_MYSQL_URL", &mut config.database.url),
            ("GONOF_TEST_REDIS_URL", &mut config.redis.url),
        ] {
            if let Some(value) = std::env::var_os(key) {
                *target = value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("{key} must be UTF-8"))?;
            }
        }
        config.apply_cookie_overrides(
            std::env::var_os("GONOF_TEST_AUTH_COOKIE_KEY").as_deref(),
            std::env::var_os("GONOF_TEST_AUTH_COOKIE_PREVIOUS_KEYS").as_deref(),
        )?;
        config.apply_two_factor_overrides(
            std::env::var_os("GONOF_TEST_TWO_FACTOR_KEY").as_deref(),
            std::env::var_os("GONOF_TEST_TWO_FACTOR_PREVIOUS_KEYS").as_deref(),
        )?;
        // The test databases are created fresh each run and deleted afterwards, so the test configuration need not
        // hold a Cookie or two-factor key: one left empty gets a random key for this process. A key that is given is
        // used and checked as usual.
        static COOKIE_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        static TWO_FACTOR_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        for (key, generated) in [
            (&mut config.identity.cookie.active_key, &COOKIE_KEY),
            (&mut config.identity.two_factor.active_key, &TWO_FACTOR_KEY),
        ] {
            if key.is_empty() {
                *key = generated.get_or_init(crate::identity::random_token).clone();
            }
        }
        // On SQLite every fixture creates its own database file and sets the URL itself (`TestFixture::new` in
        // test_support.rs), so no database server is configured. A MySQL URL from the test configuration is cleared
        // rather than left for code to connect to by mistake, and the rest is checked with a URL of the form the
        // fixtures use.
        if crate::test_support::sqlite_tests() {
            config.database.url.clear();
            let mut checked = config.clone();
            checked.database.url = "sqlite://gonof_test.db".into();
            checked.validate().context(MISSING)?;
            return Ok(config);
        }
        config.validated().context(MISSING)
    }

    fn read_files(path: &Path, required: bool) -> Result<Self> {
        let mut value: toml::Value = toml::from_str(DEFAULTS)
            .map_err(|_| anyhow::anyhow!("the default configuration is not valid TOML"))?;
        if required || path.exists() {
            // TOML parser errors may contain raw configuration lines, so the raw error, which may contain credentials,
            // is not returned.
            let overrides: toml::Table = toml::from_str(
                &std::fs::read_to_string(path).context("cannot read the local configuration")?,
            )
            .map_err(|_| {
                anyhow::anyhow!(
                    "the local configuration is not valid TOML; check the configuration file"
                )
            })?;
            // `[mail]` is the old name of `[common]`: refuse it with a message that says to rename it, rather than
            // as an unknown field.
            anyhow::ensure!(
                !overrides.contains_key("mail"),
                crate::i18n::current().setup_config_mail_renamed()
            );
            merge_tables(
                value
                    .as_table_mut()
                    .context("the default configuration must be a TOML table")?,
                overrides,
            );
        }
        value.try_into().map_err(|_| {
            anyhow::anyhow!("the configuration is invalid; check the field names and types")
        })
    }

    fn validated(self) -> Result<Self> {
        self.validate()?;
        Ok(self)
    }

    fn apply_cookie_overrides(
        &mut self,
        active: Option<&std::ffi::OsStr>,
        previous: Option<&std::ffi::OsStr>,
    ) -> Result<()> {
        if let Some(value) = active {
            self.identity.cookie.active_key = value
                .to_str()
                .ok_or_else(|| {
                    anyhow::anyhow!("the cookie key environment variable must be UTF-8")
                })?
                .to_owned();
        }
        if let Some(value) = previous {
            let value = value.to_str().ok_or_else(|| {
                anyhow::anyhow!("the previous cookie keys environment variable must be UTF-8")
            })?;
            self.identity.cookie.previous_keys = if value.trim().is_empty() {
                vec![]
            } else {
                value.split(',').map(|key| key.trim().to_owned()).collect()
            };
        }
        Ok(())
    }

    fn apply_two_factor_overrides(
        &mut self,
        active: Option<&std::ffi::OsStr>,
        previous: Option<&std::ffi::OsStr>,
    ) -> Result<()> {
        if let Some(value) = active {
            self.identity.two_factor.active_key = value
                .to_str()
                .ok_or_else(|| {
                    anyhow::anyhow!("the two-factor key environment variable must be UTF-8")
                })?
                .to_owned();
        }
        if let Some(value) = previous {
            let value = value.to_str().ok_or_else(|| {
                anyhow::anyhow!("the previous two-factor keys environment variable must be UTF-8")
            })?;
            self.identity.two_factor.previous_keys = if value.trim().is_empty() {
                vec![]
            } else {
                value.split(',').map(|key| key.trim().to_owned()).collect()
            };
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.database.validate()?;
        self.redis.validate()?;
        self.identity.validate()?;
        self.content.validate()?;
        self.media.validate()?;
        self.common.validate()?;
        self.update.validate()?;
        // This key has no rotation mechanism and is kept separate from the Cookie and two-factor keys: changing the
        // Cookie key must not make the authorization codes undecryptable.
        anyhow::ensure!(
            self.common.encryption_key.is_empty()
                || !self.identity.uses_key(&self.common.encryption_key),
            crate::i18n::current().setup_config_encryption_key_reused()
        );
        Ok(())
    }
}

fn merge_tables(defaults: &mut toml::Table, overrides: toml::Table) {
    for (key, value) in overrides {
        match (defaults.get_mut(&key), value) {
            (Some(toml::Value::Table(current)), toml::Value::Table(overrides)) => {
                merge_tables(current, overrides);
            }
            (_, value) => {
                defaults.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Config {
        let mut config: Config = toml::from_str(DEFAULTS).unwrap();
        config.database.url = "mysql://127.0.0.1/gonof_test".into();
        config.redis.url = "redis://127.0.0.1/0".into();
        config.identity.cookie.active_key = "11".repeat(32);
        config.identity.two_factor.active_key = "44".repeat(32);
        config
    }

    const CONNECTIONS: &str = "\n[database]\nurl = 'mysql://127.0.0.1/gonof_test'\n[redis]\nurl = 'redis://127.0.0.1/0'\n[identity.cookie]\nactive_key = '1111111111111111111111111111111111111111111111111111111111111111'\n[identity.two_factor]\nactive_key = '4444444444444444444444444444444444444444444444444444444444444444'\n";

    /// `[common] encryption_key`: may be left empty; if set it must be 64 hex digits and must not equal the Cookie or
    /// two-factor keys; the old `[mail]` section reports the rename explicitly.
    #[test]
    fn common_key_is_separate_from_cookie_and_two_factor_keys() {
        let path = std::env::temp_dir().join(format!(
            "gonof-common-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let load = |prefix: &str| {
            std::fs::write(&path, format!("{prefix}{CONNECTIONS}")).unwrap();
            Config::load(&crate::project_root(), Some(&path))
        };
        assert!(load("").unwrap().common.encryption_key.is_empty());
        let distinct = format!("[common]\nencryption_key = '{}'\n", "5".repeat(64));
        assert!(load(&distinct).unwrap().common.configured());
        for reused in ["1".repeat(64), "4".repeat(64)] {
            let error = load(&format!("[common]\nencryption_key = '{reused}'\n"))
                .unwrap_err()
                .to_string();
            assert!(error.contains("must differ from"), "{error}");
        }
        assert!(load("[common]\nencryption_key = 'short'\n").is_err());
        let renamed = load("[mail]\nencryption_key = ''\n")
            .unwrap_err()
            .to_string();
        assert!(renamed.contains("is now [common]"), "{renamed}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn forum_url_allows_a_slug_shaped_path() {
        for (input, normal) in [
            ("https://demo.com", "https://demo.com"),
            ("https://demo.com/", "https://demo.com"),
            ("HTTPS://Demo.COM/bbs/", "https://demo.com/bbs"),
            (
                "http://192.0.2.10:8080/a/b-2",
                "http://192.0.2.10:8080/a/b-2",
            ),
        ] {
            assert_eq!(normalize_forum_url(input).unwrap(), normal, "{input}");
        }
        let long = format!("https://demo.com/{}", "a".repeat(65));
        for bad in [
            "demo.com/bbs",
            "ftp://demo.com/bbs",
            "https://demo.com/BBS",
            "https://demo.com/b_s",
            "https://demo.com/a//b",
            "https://demo.com/a/../b",
            "https://demo.com/./b",
            "https://demo.com/%62bs",
            "https://demo.com/a\\b",
            "https://demo.com/bbs?x=1",
            "https://demo.com?x=1",
            "https://demo.com/bbs#top",
            "https://user@demo.com/bbs",
            long.as_str(),
        ] {
            assert!(normalize_forum_url(bad).is_err(), "{bad}");
        }
        // An origin derived from the request still contains only the origin.
        assert!(normalize_site_url("https://demo.com/bbs").is_err());
    }

    #[test]
    fn startup_names_a_missing_or_shared_two_factor_key() {
        let mut config = defaults();
        config.identity.two_factor.active_key.clear();
        let error = format!("{:#}", config.validate().unwrap_err());
        assert!(error.contains("identity.two_factor.active_key"), "{error}");
        config.identity.two_factor.active_key = "11".repeat(32);
        let error = format!("{:#}", config.validate().unwrap_err());
        assert!(
            error.contains("must differ from those of identity.cookie"),
            "{error}"
        );
        config.identity.two_factor.active_key = "44".repeat(32);
        assert!(config.validate().is_ok());
        config
            .apply_two_factor_overrides(
                Some(std::ffi::OsStr::new(&"55".repeat(32))),
                Some(std::ffi::OsStr::new(&format!(" {} ", "44".repeat(32)))),
            )
            .unwrap();
        assert_eq!(config.identity.two_factor.active_key, "55".repeat(32));
        assert_eq!(config.identity.two_factor.previous_keys, ["44".repeat(32)]);
        assert!(config.validate().is_ok());
        assert!(!format!("{config:?}").contains(&"55".repeat(32)));
    }

    /// Fields retired in 9.5 (and `app_debug`, which corresponds to `APP_DEBUG`) are rejected at startup if written into
    /// the private configuration: the configuration structs carry `deny_unknown_fields`, so remove these keys from the
    /// private configuration before deploying.
    #[test]
    fn retired_fields_are_rejected() {
        let path = std::env::temp_dir().join(format!(
            "gonof-retired-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for retired in [
            "app_debug = true\n",
            "base_url = 'http://127.0.0.1:58833'\n",
            "site_name = 'Sample Community'\n",
            "brand_name = 'Sample Forum'\n",
            "description = 'x'\n",
            "[[forums]]\nname = 'General'\nslug = 'daily'\ndescription = ''\nguidance = ''\n",
            "[identity]\ninvite_daily_limit = 5\n",
            "[identity]\nsession_idle_seconds = 1209600\n",
            "[identity.security_limits]\nreset_ip = { attempts = 30, window_seconds = 300 }\n",
            "[content]\ndaily_threads = 5\n",
            "[content]\nmax_title_chars = 255\n",
            "[content]\nedit_window_seconds = 86400\n",
            "[growth]\nthread = 3\n",
            "[api]\nkey_ttl_seconds = 31536000\n",
            "[maintenance]\noutbox_retention_days = 30\n",
            "[ranks]\nrefresh_seconds = 60\n",
            // The old name of [common].
            "[mail]\nencryption_key = ''\n",
            "[identity.providers]\nencryption_key = ''\n",
        ] {
            // A table header placed after the connection settings would swallow the following keys into that table, so
            // the retired items are written first.
            std::fs::write(&path, format!("{retired}{CONNECTIONS}")).unwrap();
            assert!(
                Config::load(&crate::project_root(), Some(&path)).is_err(),
                "{retired}"
            );
        }
        std::fs::write(&path, CONNECTIONS).unwrap();
        let config = Config::load(&crate::project_root(), Some(&path)).unwrap();
        assert_eq!(
            config.content.max_attachments,
            crate::markdown::MAX_ATTACHMENTS
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn validates_database_driver_and_pool_limits() {
        assert!(defaults().database.validate().is_ok());
        for url in [
            "",
            "sqlite::memory:",
            "postgres://localhost/gonof",
            "mysql://localhost",
            "redis://localhost",
        ] {
            let database = DatabaseConfig {
                url: url.into(),
                ..DatabaseConfig::default()
            };
            assert!(database.validate().is_err());
        }
        let mut database = defaults().database;
        database.max_connections = 0;
        assert!(database.validate().is_err());
        database.max_connections = 5;
        database.acquire_timeout_seconds = 0;
        assert!(database.validate().is_err());
    }

    #[test]
    fn validates_required_redis_and_hides_credentials() {
        assert!(RedisConfig::default().validate().is_err());
        let mut config = defaults().redis;
        config.url = "rediss://private-user:private-password@example.invalid/0".into();
        assert!(config.validate().is_ok());
        assert!(!format!("{config:?}").contains("private-password"));
        config.namespace = "unsafe*namespace".into();
        assert!(config.validate().is_err());
        config.namespace = "gonof:test".into();
        config.timeout_milliseconds = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn cookie_keys_are_required_rotatable_and_environment_values_stay_private() {
        let mut config = defaults();
        config.identity.cookie = crate::identity::CookieConfig::default();
        assert!(config.identity.validate().is_err());
        let active = "22".repeat(32);
        let previous = "33".repeat(32);
        config
            .apply_cookie_overrides(
                Some(std::ffi::OsStr::new(&active)),
                Some(std::ffi::OsStr::new(&previous)),
            )
            .unwrap();
        assert!(config.identity.validate().is_ok());
        assert_eq!(config.identity.cookie.active_key, active);
        assert_eq!(config.identity.cookie.previous_keys, vec![previous.clone()]);
        let debug = format!("{config:?}");
        assert!(!debug.contains(&active));
        assert!(!debug.contains(&previous));
        config
            .apply_cookie_overrides(None, Some(std::ffi::OsStr::new("")))
            .unwrap();
        assert!(config.identity.cookie.previous_keys.is_empty());
        assert_eq!(config.identity.cookie.active_key, active);
    }

    #[test]
    fn partial_database_overrides_preserve_default_table_values() {
        let root = std::env::temp_dir().join(format!(
            "gonof-nested-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("config")).unwrap();
        // The defaults are compiled in; a stray default.toml next to the private file is not read.
        std::fs::write(
            root.join("config/default.toml"),
            DEFAULTS.replace(
                "acquire_timeout_seconds = 10",
                "acquire_timeout_seconds = 19",
            ),
        )
        .unwrap();
        std::fs::write(
            Config::default_path(&root),
            CONNECTIONS.replace("[database]\n", "[database]\nmax_connections = 3\n"),
        )
        .unwrap();
        let config = Config::load(&root, None).unwrap();
        assert_eq!(config.database.max_connections, 3);
        assert_eq!(config.database.acquire_timeout_seconds, 10);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A relative SQLite file names a file in the instance directory, not in the directory the command runs in.
    #[test]
    fn relative_sqlite_files_are_in_the_instance_directory() {
        let root = std::env::temp_dir().join(format!(
            "gonof-sqlite-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("config")).unwrap();
        let path = root.join("config/gono-forum.toml");
        std::fs::write(
            &path,
            CONNECTIONS.replace(
                "url = 'mysql://127.0.0.1/gonof_test'",
                "url = 'sqlite://data/review%25.db?mode=rwc'",
            ),
        )
        .unwrap();
        let config = Config::load(&root, Some(&path)).unwrap();
        assert_eq!(
            crate::db::sqlite::file_of(&config.database.url).unwrap(),
            root.join("data/review%.db")
        );
        assert!(config.database.url.ends_with("?mode=rwc"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_overrides_are_strict() {
        let path = std::env::temp_dir().join(format!(
            "gonof-config-test-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, format!("listen = '127.0.0.1:58834'\n{CONNECTIONS}")).unwrap();
        let config = Config::load(&crate::project_root(), Some(&path)).unwrap();
        assert_eq!(config.listen.port(), 58834);
        std::fs::write(&path, "listen_typo = '127.0.0.1:1'\n").unwrap();
        assert!(Config::load(&crate::project_root(), Some(&path)).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(Config::load(&crate::project_root(), Some(&path)).is_err());
    }
}
