//! Update check and one-click upgrade.
//!
//! A release is published as `gono-forum_<version>_linux_<amd64|arm64>.tar.gz` plus `checksums.txt` (SHA-256 lines as
//! written by `sha256sum`), built by `scripts/package-release.sh`. The update source is `[update] releases_url`: GitHub
//! Releases by default, or any mirror with the same paths, including a local directory (`file:///…`):
//!
//! ```text
//! <releases_url>/latest/download/checksums.txt
//! <releases_url>/download/v<version>/<package>
//! ```
//!
//! Upgrading downloads the package, refuses it unless `checksums.txt` lists it with a matching digest, unpacks the two
//! binaries next to the running ones, checks that the new web binary runs on this machine and reports the expected
//! version, then renames the current binaries to `*.previous`, moves the new ones in and asks the process to exit:
//! systemd (`Restart=always`) starts the new version. `data/upgrade/pending` marks the switch; the new version removes
//! it once it is serving ([`confirm`]), and the start guard forum.install.sh puts in the unit (`ExecStartPre`) moves
//! the previous binaries back when the new version fails to start twice. Database migrations get no special treatment
//! and the upgrade takes no backup: if the new version migrated before failing, the old one may refuse the database
//! until it is restored from a backup.
use std::{
    cmp::Ordering,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;

pub const DEFAULT_RELEASES_URL: &str = "https://github.com/gonolab/gono.forum/releases";

/// Package names start with this, followed by `_<version>_<platform>.tar.gz`.
pub const PACKAGE_PREFIX: &str = "gono-forum";

/// The two binaries a package carries and an upgrade replaces.
pub const BINARIES: [&str; 2] = ["gono-forum-web", "gono-forum-cli"];

/// Where the upgrade marker and results live, relative to the instance directory. Fixed rather than under
/// `identity.local_directory`: the start guard in the systemd unit is a shell script that cannot read the
/// configuration.
pub const STATE_DIRECTORY: &str = "data/upgrade";

/// Where `forum.install.sh` saves the binaries while it switches them (upgrade, rollback, the start guard), relative to
/// the instance directory. While it exists such a switch did not finish, and only `forum.install.sh upgrade` or
/// `rollback` may put it back, so a one-click upgrade waits until it is gone.
pub const SWITCH_BACKUP: &str = "switch-backup";

const CHECKSUMS_LIMIT: usize = 64 * 1024;
const PACKAGE_LIMIT: usize = 256 * 1024 * 1024;
const BINARY_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    /// `https://…` (GitHub Releases or a mirror with the same layout) or `file:///absolute/directory`.
    pub releases_url: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            releases_url: DEFAULT_RELEASES_URL.into(),
        }
    }
}

impl UpdateConfig {
    pub fn validate(&self) -> Result<()> {
        let url = self.releases_url.trim_end_matches('/');
        ensure!(
            url.starts_with("https://") || url.starts_with("file:///"),
            "[update] releases_url must be an https:// address or a file:/// absolute path"
        );
        ensure!(
            !url.contains(['?', '#']) && !url.split('/').any(|part| part == ".."),
            "[update] releases_url must not contain a query, a fragment or .."
        );
        Ok(())
    }

    fn base(&self) -> &str {
        self.releases_url.trim_end_matches('/')
    }
}

/// The package platform of this build: `linux_amd64` or `linux_arm64`; `None` elsewhere (no package is published).
pub fn platform() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match std::env::consts::ARCH {
        "x86_64" => Some("linux_amd64"),
        "aarch64" => Some("linux_arm64"),
        _ => None,
    }
}

pub fn package_name(version: &str, platform: &str) -> String {
    format!("{PACKAGE_PREFIX}_{version}_{platform}.tar.gz")
}

/// A semantic version (`MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]`), ordered by the SemVer 2.0 rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut numbers = [0_u64; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *number = part.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let identifiers: Vec<String> = pre.split('.').map(str::to_owned).collect();
                let valid = identifiers.iter().all(|identifier| {
                    !identifier.is_empty()
                        && identifier
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
                if !valid {
                    return None;
                }
                identifiers
            }
        };
        Some(Self { core: numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release is newer than its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (left, right) in self.pre.iter().zip(&other.pre) {
                        let order = match (left.parse::<u64>(), right.parse::<u64>()) {
                            (Ok(left), Ok(right)) => left.cmp(&right),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => left.cmp(right),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The newest package for this platform that `checksums.txt` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub package: String,
    /// Lowercase hex SHA-256 of the package.
    pub sha256: String,
}

impl Release {
    /// Whether this release is newer than the running build.
    pub fn is_newer(&self) -> bool {
        match (
            Version::parse(&self.version),
            Version::parse(crate::VERSION),
        ) {
            (Some(release), Some(current)) => release > current,
            _ => false,
        }
    }
}

/// Reads `sha256sum` output and picks the newest package for `platform`. Lines that are not a 64-digit digest
/// followed by a file name are ignored.
pub fn parse_checksums(text: &str, platform: &str) -> Option<Release> {
    let suffix = format!("_{platform}.tar.gz");
    let prefix = format!("{PACKAGE_PREFIX}_");
    text.lines()
        .filter_map(|line| {
            let (digest, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            let valid = digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit());
            let version = name.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
            let parsed = Version::parse(version)?;
            valid.then(|| {
                (
                    parsed,
                    Release {
                        version: version.to_owned(),
                        package: name.to_owned(),
                        sha256: digest.to_ascii_lowercase(),
                    },
                )
            })
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, release)| release)
}

fn user_agent() -> String {
    format!("gono-forum/{}", crate::VERSION)
}

/// Reads one file of the release source: a local file for `file://`, otherwise HTTPS with redirects followed (GitHub
/// sends release assets through two of them), at most `limit` bytes.
async fn fetch(url: &str, limit: usize, timeout: Duration) -> Result<bytes::Bytes> {
    if let Some(path) = url.strip_prefix("file://") {
        let path = PathBuf::from(path);
        let metadata = tokio::fs::metadata(&path).await.with_context(|| {
            crate::i18n::current().maintenance_updates_error_read(path.display().to_string())
        })?;
        ensure!(
            metadata.len() <= limit as u64,
            crate::i18n::current()
                .maintenance_updates_error_too_large(path.display().to_string(), limit)
        );
        return Ok(tokio::fs::read(&path).await?.into());
    }
    let mut current =
        url::Url::parse(url).context(crate::i18n::current().maintenance_updates_error_url())?;
    for _ in 0..6 {
        ensure!(
            current.scheme() == "https",
            crate::i18n::current().maintenance_updates_error_https(current.as_str())
        );
        let request = crate::outbound::Request::builder()
            .method(crate::outbound::Method::GET)
            .uri(current.as_str())
            .header(crate::outbound::header::USER_AGENT, user_agent())
            .header(crate::outbound::header::ACCEPT, "*/*")
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .context("the upgrade request could not be built")?;
        let response = crate::outbound::send(request, timeout, limit).await?;
        match response.status {
            200 => return Ok(response.body),
            301 | 302 | 303 | 307 | 308 => {
                let location = response.location.ok_or_else(|| {
                    anyhow!(
                        crate::i18n::current()
                            .maintenance_updates_error_no_location(current.as_str())
                    )
                })?;
                current = current
                    .join(&location)
                    .context(crate::i18n::current().maintenance_updates_error_location())?;
            }
            404 => {
                bail!(crate::i18n::current().maintenance_updates_error_not_found(current.as_str()))
            }
            status => bail!(
                crate::i18n::current().maintenance_updates_error_status(current.as_str(), status)
            ),
        }
    }
    bail!(crate::i18n::current().maintenance_updates_error_redirects(url))
}

/// Looks up the newest release for this platform. `Ok(None)`: the source lists no package for this platform.
pub async fn latest(config: &UpdateConfig) -> Result<Option<Release>> {
    let platform = platform()
        .ok_or_else(|| anyhow!(crate::i18n::current().maintenance_updates_error_platform()))?;
    let checksums = fetch(
        &format!("{}/latest/download/checksums.txt", config.base()),
        CHECKSUMS_LIMIT,
        Duration::from_secs(30),
    )
    .await
    .context(crate::i18n::current().maintenance_updates_error_checksums())?;
    let text = std::str::from_utf8(&checksums)
        .context(crate::i18n::current().maintenance_updates_error_checksums_utf8())?;
    Ok(parse_checksums(text, platform))
}

/// The two binaries taken out of a package.
pub struct Unpacked {
    pub web: Vec<u8>,
    pub cli: Vec<u8>,
}

/// Downloads the release's package and verifies its digest; returns the unpacked binaries.
pub async fn download(config: &UpdateConfig, release: &Release) -> Result<Unpacked> {
    let package = fetch(
        &format!(
            "{}/download/v{}/{}",
            config.base(),
            release.version,
            release.package
        ),
        PACKAGE_LIMIT,
        Duration::from_secs(600),
    )
    .await
    .context(crate::i18n::current().maintenance_updates_error_download())?;
    verify(&package, &release.sha256)?;
    tokio::task::spawn_blocking(move || unpack(&package))
        .await
        .context("the unpacking task failed")?
}

/// Compares the package digest with the one `checksums.txt` lists; a mismatch stops the upgrade.
pub fn verify(package: &[u8], expected: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let actual: String = Sha256::digest(package)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        crate::i18n::current().maintenance_updates_error_digest(expected, actual.as_str())
    );
    Ok(())
}

/// Takes the two binaries out of a gzip-compressed tar: regular files named `gono-forum-web` and `gono-forum-cli`,
/// at the top level or in one directory. Links, other entries and anything with `..` are ignored; entries over the
/// size limit are refused.
pub fn unpack(package: &[u8]) -> Result<Unpacked> {
    let mut archive =
        flate2::read::GzDecoder::new(package).take(BINARY_LIMIT * 2 + 64 * 1024 * 1024);
    let mut web = None;
    let mut cli = None;
    let mut header = [0_u8; 512];
    let mut long_name: Option<String> = None;
    loop {
        if read_block(&mut archive, &mut header)?.is_none() {
            break;
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = octal(&header[124..136])
            .context(crate::i18n::current().maintenance_updates_error_entry_size())?;
        ensure!(
            size <= BINARY_LIMIT,
            crate::i18n::current().maintenance_updates_error_entry_large()
        );
        let kind = header[156];
        let name = match long_name.take() {
            Some(name) => name,
            None => {
                let name = field(&header[0..100]);
                let prefix = if &header[257..262] == b"ustar" {
                    field(&header[345..500])
                } else {
                    String::new()
                };
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        let mut data = vec![0_u8; usize::try_from(size)?];
        archive
            .read_exact(&mut data)
            .context(crate::i18n::current().maintenance_updates_error_incomplete())?;
        let padding = (512 - (size % 512)) % 512;
        std::io::copy(&mut (&mut archive).take(padding), &mut std::io::sink())?;
        match kind {
            // GNU long name: the data is the next entry's name.
            b'L' => {
                long_name = Some(field(&data));
                continue;
            }
            b'0' | 0 => {}
            _ => continue,
        }
        let parts: Vec<&str> = name
            .trim_start_matches("./")
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if parts.contains(&"..") || parts.len() > 2 {
            continue;
        }
        match parts.last() {
            Some(&"gono-forum-web") => web = Some(data),
            Some(&"gono-forum-cli") => cli = Some(data),
            _ => {}
        }
    }
    Ok(Unpacked {
        web: web
            .context(crate::i18n::current().maintenance_updates_error_missing("gono-forum-web"))?,
        cli: cli
            .context(crate::i18n::current().maintenance_updates_error_missing("gono-forum-cli"))?,
    })
}

fn read_block(reader: &mut impl Read, block: &mut [u8; 512]) -> Result<Option<()>> {
    let mut filled = 0;
    while filled < block.len() {
        let read = reader
            .read(&mut block[filled..])
            .context(crate::i18n::current().maintenance_updates_error_read_package())?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(anyhow!(
                    crate::i18n::current().maintenance_updates_error_incomplete()
                ))
            };
        }
        filled += read;
    }
    Ok(Some(()))
}

fn field(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn octal(bytes: &[u8]) -> Option<u64> {
    let text = field(bytes);
    let text = text.trim_matches(|c: char| c == ' ' || c == '\0');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// File paths of one instance (the directory holding the running binaries).
#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The instance of this process: the directory of the running binary in a release package (an upgrade replaces
    /// the binaries there, wherever `GONOF_HOME` points), `root` in a source build.
    pub fn for_process(root: &Path) -> Self {
        let directory = crate::BUNDLED
            .then(|| std::env::current_exe().ok())
            .flatten()
            .and_then(|executable| executable.parent().map(Path::to_path_buf));
        Self::new(directory.unwrap_or_else(|| root.to_path_buf()))
    }

    pub fn binary(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn with_suffix(&self, name: &str, suffix: &str) -> PathBuf {
        self.root.join(format!("{name}{suffix}"))
    }

    pub fn state(&self) -> PathBuf {
        self.root.join(STATE_DIRECTORY)
    }

    pub fn pending(&self) -> PathBuf {
        self.state().join("pending")
    }

    pub fn last(&self) -> PathBuf {
        self.state().join("last")
    }

    pub fn previous_exists(&self) -> bool {
        self.with_suffix(BINARIES[0], ".previous").is_file()
    }
}

/// Puts the new binaries in place of the running ones (see the module notes). On return the new version is installed
/// and marked pending; the caller then asks the process to exit.
pub async fn install(paths: &Paths, release: &Release, unpacked: Unpacked) -> Result<()> {
    let staged: Vec<(PathBuf, PathBuf, PathBuf)> = BINARIES
        .iter()
        .map(|name| {
            (
                paths.with_suffix(name, ".new"),
                paths.binary(name),
                paths.with_suffix(name, ".previous"),
            )
        })
        .collect();
    for ((new, _, _), bytes) in staged.iter().zip([&unpacked.web, &unpacked.cli]) {
        write_executable(new, bytes).with_context(|| {
            crate::i18n::current().maintenance_updates_error_write(new.display().to_string())
        })?;
    }
    let expected = format!("{} {}", BINARIES[0], release.version);
    let reported = run_version(&staged[0].0).await;
    if reported.as_deref() != Some(expected.as_str()) {
        for (new, _, _) in &staged {
            let _ = std::fs::remove_file(new);
        }
        bail!(crate::i18n::current().maintenance_updates_error_version(
            expected.as_str(),
            reported.unwrap_or_else(|| {
                crate::i18n::current()
                    .maintenance_updates_no_output()
                    .into()
            })
        ));
    }
    std::fs::create_dir_all(paths.state())
        .context(crate::i18n::current().maintenance_updates_error_state_dir())?;
    write_record(
        &paths.pending(),
        &[
            ("from", crate::VERSION),
            ("to", &release.version),
            ("at", &crate::identity::now().to_string()),
        ],
    )?;
    let _ = std::fs::remove_file(paths.state().join("attempts"));
    // Each binary moves in two renames, current to previous and new to current. When one fails, the renames already
    // done are undone, last first, so that the binaries on disk are again the running version and not a new web
    // binary next to the old command-line tool. The marker goes only once that worked; otherwise the start guard
    // still has it.
    let mut done: Vec<(&Path, &Path)> = Vec::new();
    for (new, current, previous) in &staged {
        for (from, to) in [(current, previous), (new, current)] {
            if let Err(error) = std::fs::rename(from, to) {
                let restored = done
                    .iter()
                    .rev()
                    .all(|(from, to)| std::fs::rename(to, from).is_ok());
                if restored {
                    let _ = std::fs::remove_file(paths.pending());
                } else {
                    tracing::error!(
                        "could not put the running binaries back after a failed upgrade; the upgrade marker is kept"
                    );
                }
                sync_directory(&paths.root);
                return Err(anyhow!(error)
                    .context(crate::i18n::current().maintenance_updates_error_replace()));
            }
            done.push((from, to));
        }
    }
    sync_directory(&paths.root);
    Ok(())
}

fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = std::fs::File::open(path) {
        let _ = directory.sync_all();
    }
}

/// The first line `<binary> --version` prints, or `None` when it does not run within 15 seconds.
async fn run_version(binary: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()
        .map(|line| line.trim().to_owned())
}

/// `key=value` lines, the format the start guard (POSIX shell) also reads and writes.
fn write_record(path: &Path, fields: &[(&str, &str)]) -> Result<()> {
    let mut text = String::new();
    for (key, value) in fields {
        ensure!(
            !value.contains(['\n', '\r']),
            "upgrade record values must not contain line breaks"
        );
        text.push_str(&format!("{key}={value}\n"));
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn read_record(path: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect(),
    )
}

/// The outcome of the last upgrade, for the admin page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub from: String,
    pub to: String,
    /// `upgraded`, `rolled_back` (the new version did not start) or `switched` (`forum.install.sh rollback`).
    pub result: String,
    pub at: i64,
}

pub fn last(paths: &Paths) -> Option<Outcome> {
    let fields = read_record(&paths.last())?;
    let get = |key: &str| {
        fields
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    Some(Outcome {
        from: get("from"),
        to: get("to"),
        result: get("result"),
        at: get("finished")
            .parse()
            .or_else(|_| get("at").parse())
            .unwrap_or(0),
    })
}

/// Called by the new version once it is serving: the upgrade marked pending succeeded. Returns its outcome.
pub fn confirm(paths: &Paths) -> Option<Outcome> {
    let fields = read_record(&paths.pending())?;
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_default()
    };
    // A marker for another version (the guard already rolled back, or a manual swap) is not ours to confirm.
    if get("to") != crate::VERSION {
        return None;
    }
    let finished = crate::identity::now().to_string();
    let record = [
        ("from", get("from")),
        ("to", get("to")),
        ("at", get("at")),
        ("result", "upgraded"),
        ("finished", finished.as_str()),
    ];
    if let Err(error) = write_record(&paths.last(), &record) {
        tracing::warn!(%error, "could not record the finished upgrade");
    }
    let _ = std::fs::remove_file(paths.pending());
    let _ = std::fs::remove_file(paths.state().join("attempts"));
    last(paths)
}

/// Why this process cannot upgrade itself in place; empty when it can. Checking for updates works everywhere.
pub fn blockers(paths: &Paths) -> Vec<&'static str> {
    let t = crate::i18n::current();
    let mut reasons = Vec::new();
    if !crate::BUNDLED {
        reasons.push(t.maintenance_updates_blocker_source());
    }
    if platform().is_none() {
        reasons.push(t.maintenance_updates_error_platform());
    }
    if std::env::var_os("INVOCATION_ID").is_none() {
        reasons.push(t.maintenance_updates_blocker_systemd());
    }
    // The upgrade writes `*.new` next to the binaries and renames them over, so the directory has to be writable;
    // the running binaries themselves are never opened for writing (Linux refuses that with ETXTBSY).
    let writable =
        BINARIES.iter().all(|name| paths.binary(name).is_file()) && tempfile_ok(&paths.root);
    if !writable {
        reasons.push(t.maintenance_updates_blocker_writable());
    }
    // Replacing the binaries now would leave the saved ones behind, and the next forum.install.sh upgrade or rollback
    // would put back binaries older than this upgrade.
    if paths.root.join(SWITCH_BACKUP).exists() {
        reasons.push(t.maintenance_updates_blocker_switch());
    }
    reasons
}

fn tempfile_ok(directory: &Path) -> bool {
    let probe = directory.join(format!(".gonof-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

static RESTART: std::sync::OnceLock<tokio_util::sync::CancellationToken> =
    std::sync::OnceLock::new();

fn restart_token() -> &'static tokio_util::sync::CancellationToken {
    RESTART.get_or_init(tokio_util::sync::CancellationToken::new)
}

/// Asks `main` to shut down gracefully so systemd starts the newly installed binary.
pub fn request_restart() {
    restart_token().cancel();
}

/// Resolves once [`request_restart`] was called.
pub async fn restart_requested() {
    restart_token().cancelled().await;
}

/// What the admin page shows about checking and upgrading in this process.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub checked_at: Option<i64>,
    pub release: Option<Release>,
    pub check_error: String,
    /// Non-empty while an upgrade runs: the current step.
    pub running: String,
    pub failure: String,
}

/// Shared state of the update page: one check result and at most one upgrade at a time per process.
#[derive(Clone, Default)]
pub struct Updater {
    status: Arc<Mutex<Status>>,
}

impl Updater {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    fn update(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut status) = self.status.lock() {
            change(&mut status);
        }
    }

    pub async fn check(&self, config: &UpdateConfig) {
        let result = latest(config).await;
        self.update(|status| {
            status.checked_at = Some(crate::identity::now());
            match result {
                Ok(release) => {
                    status.release = release;
                    status.check_error.clear();
                }
                Err(error) => {
                    status.release = None;
                    status.check_error = crate::debug::visible_error(&error);
                }
            }
        });
    }

    /// Starts the upgrade to the release found by the last check, in the background. Errors when there is nothing to
    /// upgrade to, an upgrade is already running, or this process cannot upgrade itself.
    pub fn start(&self, config: UpdateConfig, paths: Paths) -> std::result::Result<(), String> {
        let blockers = blockers(&paths);
        if !blockers.is_empty() {
            return Err(
                blockers.join(crate::i18n::current().maintenance_updates_blocker_separator())
            );
        }
        let release = {
            let mut guard = self.status.lock().map_err(|_| {
                crate::i18n::current()
                    .maintenance_updates_error_status_lock()
                    .to_owned()
            })?;
            if !guard.running.is_empty() {
                return Err(crate::i18n::current()
                    .maintenance_updates_error_running()
                    .into());
            }
            let release = guard
                .release
                .clone()
                .filter(Release::is_newer)
                .ok_or_else(|| {
                    crate::i18n::current()
                        .maintenance_updates_error_no_release()
                        .to_owned()
                })?;
            guard.running =
                crate::i18n::current().maintenance_updates_downloading(&release.package);
            guard.failure.clear();
            release
        };
        let updater = self.clone();
        // The steps and failures below are shown on the update page in the language of the request that started it.
        let site = crate::site::current();
        tokio::spawn(crate::site::scope(site, async move {
            let outcome = async {
                let unpacked = download(&config, &release).await?;
                updater.update(|status| {
                    status.running =
                        crate::i18n::current().maintenance_updates_installing(&release.version);
                });
                install(&paths, &release, unpacked).await
            }
            .await;
            match outcome {
                Ok(()) => {
                    tracing::info!(from = crate::VERSION, to = %release.version, "upgrade installed; restarting");
                    updater.update(|status| {
                        status.running =
                            crate::i18n::current().maintenance_updates_restarting(&release.version);
                    });
                    request_restart();
                }
                Err(error) => {
                    tracing::warn!(error = %error, "upgrade failed");
                    updater.update(|status| {
                        status.running.clear();
                        status.failure = crate::debug::visible_error(&error);
                    });
                }
            }
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "gonof-update-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A ustar archive with the given regular files (and one symlink), gzip-compressed.
    fn package(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut tar = Vec::new();
        let mut entry = |name: &str, kind: u8, data: &[u8]| {
            let mut header = [0_u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..107].copy_from_slice(b"0000755");
            header[108..115].copy_from_slice(b"0000000");
            header[116..123].copy_from_slice(b"0000000");
            header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            header[136..147].copy_from_slice(b"00000000000");
            header[156] = kind;
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            header[148..156].copy_from_slice(b"        ");
            let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
            header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
            tar.extend_from_slice(&header);
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        };
        entry("gono-forum_9.9.9_linux_amd64/link", b'2', b"");
        for (name, data) in files {
            entry(name, b'0', data);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn versions_follow_semver_ordering() {
        let ordered = [
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.beta",
            "0.1.0-beta.2",
            "0.1.0-beta.11",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1-dev.1",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            let (left, right) = (
                Version::parse(pair[0]).unwrap(),
                Version::parse(pair[1]).unwrap(),
            );
            assert!(left < right, "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(Version::parse("v1.2.3+build.7"), Version::parse("1.2.3"));
        for invalid in ["1.2", "1.2.3.4", "1.x.3", "1.2.3-", "1.2.3-a..b", ""] {
            assert!(Version::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn checksums_pick_the_newest_package_for_the_platform() {
        let text = format!(
            "{a}  gono-forum_0.1.0_linux_amd64.tar.gz\n{b} *gono-forum_0.2.0_linux_amd64.tar.gz\n{c}  gono-forum_0.3.0_linux_arm64.tar.gz\nnot a line\n{short}  gono-forum_9.0.0_linux_amd64.tar.gz\n",
            a = "a".repeat(64),
            b = "B".repeat(64),
            c = "c".repeat(64),
            short = "d".repeat(63),
        );
        let release = parse_checksums(&text, "linux_amd64").unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.package, "gono-forum_0.2.0_linux_amd64.tar.gz");
        assert_eq!(release.sha256, "b".repeat(64));
        assert_eq!(
            parse_checksums(&text, "linux_arm64").unwrap().version,
            "0.3.0"
        );
        assert!(parse_checksums(&text, "linux_riscv64").is_none());
    }

    #[test]
    fn packages_are_verified_and_unpacked_strictly() {
        let bytes = package(&[
            ("gono-forum_9.9.9_linux_amd64/gono-forum-web", b"web"),
            ("gono-forum_9.9.9_linux_amd64/gono-forum-cli", b"cli"),
            (
                "gono-forum_9.9.9_linux_amd64/forum.install.sh",
                b"#!/bin/sh",
            ),
            ("../gono-forum-web", b"outside"),
            ("a/b/gono-forum-cli", b"too deep"),
        ]);
        use sha2::Digest;
        let digest: String = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        verify(&bytes, &digest).unwrap();
        assert!(verify(&bytes, &"0".repeat(64)).is_err());
        let unpacked = unpack(&bytes).unwrap();
        assert_eq!(unpacked.web, b"web");
        assert_eq!(unpacked.cli, b"cli");
        let missing = package(&[("gono-forum-web", b"web")]);
        assert!(unpack(&missing).is_err());
        assert!(unpack(b"not gzip").is_err());
    }

    #[test]
    fn sources_must_be_https_or_local_directories() {
        for valid in [
            DEFAULT_RELEASES_URL,
            "file:///srv/releases",
            "https://mirror.example.com/r/",
        ] {
            assert!(
                UpdateConfig {
                    releases_url: valid.into()
                }
                .validate()
                .is_ok(),
                "{valid}"
            );
        }
        for invalid in [
            "http://example.com",
            "file://relative",
            "https://x/../y",
            "https://x/?a=1",
            "",
        ] {
            assert!(
                UpdateConfig {
                    releases_url: invalid.into()
                }
                .validate()
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_release_installs_and_is_confirmed_by_the_new_version() {
        let source = temp_dir("source");
        let instance = temp_dir("instance");
        let version = "9.9.9";
        let script = |name: &str| format!("#!/bin/sh\necho \"{name} {version}\"\n").into_bytes();
        let bytes = package(&[
            (
                "gono-forum_9.9.9_linux_amd64/gono-forum-web",
                &script("gono-forum-web"),
            ),
            (
                "gono-forum_9.9.9_linux_amd64/gono-forum-cli",
                &script("gono-forum-cli"),
            ),
        ]);
        use sha2::Digest;
        let digest: String = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let name = package_name(version, "linux_amd64");
        std::fs::create_dir_all(source.join("latest/download")).unwrap();
        std::fs::create_dir_all(source.join("download/v9.9.9")).unwrap();
        std::fs::write(source.join("download/v9.9.9").join(&name), &bytes).unwrap();
        std::fs::write(
            source.join("latest/download/checksums.txt"),
            format!("{digest}  {name}\n"),
        )
        .unwrap();
        let config = UpdateConfig {
            releases_url: format!("file://{}", source.display()),
        };
        // The platform is fixed here so the test also runs on macOS.
        let checksums = fetch(
            &format!("{}/latest/download/checksums.txt", config.base()),
            CHECKSUMS_LIMIT,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let release =
            parse_checksums(std::str::from_utf8(&checksums).unwrap(), "linux_amd64").unwrap();
        assert!(release.is_newer());
        for name in BINARIES {
            std::fs::write(instance.join(name), format!("old {name}")).unwrap();
        }
        let paths = Paths::new(&instance);
        let unpacked = download(&config, &release).await.unwrap();
        install(&paths, &release, unpacked).await.unwrap();
        for name in BINARIES {
            assert_eq!(
                std::fs::read_to_string(instance.join(format!("{name}.previous"))).unwrap(),
                format!("old {name}")
            );
            assert!(!instance.join(format!("{name}.new")).exists());
        }
        assert_eq!(
            run_version(&instance.join("gono-forum-web"))
                .await
                .as_deref(),
            Some("gono-forum-web 9.9.9")
        );
        let pending = std::fs::read_to_string(paths.pending()).unwrap();
        assert!(
            pending.contains(&format!("from={}\n", crate::VERSION))
                && pending.contains("to=9.9.9\n")
        );
        // The running build is not 9.9.9, so it never confirms someone else's upgrade.
        assert!(confirm(&paths).is_none());
        assert!(paths.pending().exists());
        // What the 9.9.9 binary would do once serving: record the result and drop the marker.
        write_record(
            &paths.pending(),
            &[("from", "9.9.8"), ("to", crate::VERSION), ("at", "1")],
        )
        .unwrap();
        let outcome = confirm(&paths).unwrap();
        assert_eq!(outcome.result, "upgraded");
        assert_eq!(outcome.from, "9.9.8");
        assert!(!paths.pending().exists());
        assert_eq!(last(&paths), Some(outcome));

        // A package whose web binary reports another version is refused before anything is replaced.
        let wrong = package(&[
            ("gono-forum-web", &script("gono-forum-web")[..]),
            ("gono-forum-cli", &script("gono-forum-cli")[..]),
        ]);
        let release = Release {
            version: "9.9.10".into(),
            package: "unused".into(),
            sha256: String::new(),
        };
        let error = install(&paths, &release, unpack(&wrong).unwrap())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("reports another version"),
            "{error}"
        );
        assert!(!instance.join("gono-forum-web.new").exists());
        assert_eq!(
            run_version(&instance.join("gono-forum-web"))
                .await
                .as_deref(),
            Some("gono-forum-web 9.9.9")
        );
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(instance);
    }

    /// When the command-line tool cannot be moved into place, the web binary that already was goes back too, and the
    /// marker is removed only then: the next start runs the old version of both, not the new web binary with the old
    /// tool.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_swap_puts_both_binaries_back() {
        let instance = temp_dir("half-swapped");
        for name in BINARIES {
            std::fs::write(instance.join(name), format!("old {name}")).unwrap();
        }
        // A directory where the command-line tool's `.previous` goes makes its first rename fail.
        let blocker = instance.join("gono-forum-cli.previous");
        std::fs::create_dir_all(blocker.join("occupied")).unwrap();
        let version = "9.9.11";
        let script = |name: &str| format!("#!/bin/sh\necho \"{name} {version}\"\n").into_bytes();
        let release = Release {
            version: version.into(),
            package: "unused".into(),
            sha256: String::new(),
        };
        let unpacked = Unpacked {
            web: script("gono-forum-web"),
            cli: script("gono-forum-cli"),
        };
        let paths = Paths::new(&instance);
        assert!(install(&paths, &release, unpacked).await.is_err());
        for name in BINARIES {
            assert_eq!(
                std::fs::read_to_string(instance.join(name)).unwrap(),
                format!("old {name}"),
                "{name} is the running version again"
            );
        }
        assert!(!instance.join("gono-forum-web.previous").exists());
        assert!(!paths.pending().exists());
        let _ = std::fs::remove_dir_all(instance);
    }

    #[test]
    fn an_unfinished_switch_by_the_install_script_blocks_one_click_upgrades() {
        let instance = temp_dir("switch-backup");
        let paths = Paths::new(&instance);
        let reason = crate::i18n::current().maintenance_updates_blocker_switch();
        assert!(!blockers(&paths).contains(&reason));
        std::fs::create_dir(instance.join(SWITCH_BACKUP)).unwrap();
        assert!(blockers(&paths).contains(&reason));
        let _ = std::fs::remove_dir_all(instance);
    }
}
