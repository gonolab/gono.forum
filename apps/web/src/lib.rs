pub mod activities;
pub mod administration;
pub mod bio;
pub mod cache;
pub mod clock;
pub mod config;
pub mod content;
pub mod css_check;
pub mod custom_css;
pub mod data;
pub mod db;
pub mod debug;
pub mod directory;
pub mod domains;
pub mod growth;
pub mod honors;
pub mod i18n;
pub mod identity;
pub mod interaction;
pub mod links;
pub mod login;
pub mod mail;
pub mod maintenance;
pub mod manage;
pub mod markdown;
pub mod media;
pub mod messages;
pub mod metrics;
pub mod moderation;
pub mod notifications;
pub mod oauth;
pub mod organization;
pub mod outbound;
pub mod outbox;
pub mod privacy;
pub mod ranks;
pub mod rate_limit;
pub mod reading;
pub(crate) mod registration_sources;
pub mod router;
pub mod runtime;
pub mod secrets;
pub mod setup;
pub mod site;
pub mod skins;
pub mod sms;
pub mod static_files;
pub mod systemd;
#[doc(hidden)]
pub mod test_support;
pub mod updates;
pub mod urls;

/// The source tree this crate was compiled from. Tests and source scans read files from it; the running service
/// uses [`instance_root`] instead.
pub fn project_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Version reported by `--version`, the footer of the update page and the update check. Release packages may set
/// `GONOF_BUILD_VERSION` at build time (for example `0.1.1-dev.1` for a test package); otherwise it is the crate
/// version.
pub const VERSION: &str = match option_env!("GONOF_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Whether static assets and the default configuration are compiled into the binary (release packages, cargo
/// feature `bundled`). Source builds read static assets from disk so styles can change without recompiling.
pub const BUNDLED: bool = cfg!(feature = "bundled");

/// Root directory of this instance at run time: relative paths in the configuration (`identity.local_directory`,
/// `media.local_directory`, relative SQLite files) and the default configuration file resolve against it.
///
/// `GONOF_HOME` wins when set. Otherwise a release package uses the directory holding the binary (an install is
/// `/opt/gono-forum-<instance>/` with the binaries, `config/` and `data/`), and a source build uses the source tree.
pub fn instance_root() -> anyhow::Result<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("GONOF_HOME").filter(|home| !home.is_empty()) {
        return Ok(std::path::PathBuf::from(home));
    }
    if BUNDLED {
        let executable = std::env::current_exe()
            .map_err(|error| anyhow::anyhow!("cannot locate the running binary: {error}"))?;
        return executable
            .parent()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("the running binary has no parent directory"));
    }
    Ok(project_root())
}

/// Installs ring as the process-wide default rustls crypto provider.
///
/// Both the ring and the aws-lc-rs backends are in the dependency tree (the S3 client brings aws-lc-rs and names it
/// itself), so rustls cannot infer a default from its crate features, and `rustls::ClientConfig::builder()` panics
/// when none is installed. The Redis client builds its `rediss://` configuration that way. sqlx, lettre and the
/// outbound HTTPS client name ring explicitly; this makes the default the same.
///
/// Both binaries call it before reading their configuration, and so does [`cache::Cache::new`], which every Redis
/// client goes through (the setup wizard and the tests included). Calling it again keeps the provider already
/// installed.
pub fn install_crypto_provider() {
    // An error only means a default is already in place, from an earlier call or a concurrent one.
    let _ = rustls::crypto::ring::default_provider().install_default();
}
