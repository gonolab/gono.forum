//! First-run setup wizard (docs/install.md, "Setup wizard").
//!
//! An instance whose `database.url` is still empty, or whose database has no administrator yet, serves only this
//! wizard: every path shows the same page and nothing of the forum is reachable. Entering it takes the one-time token
//! in `<local_directory>/setup-token`, which forum.install.sh writes and prints (the process writes one itself when the
//! file is missing). The configure stage tests the database and Redis, migrates, writes the private configuration
//! with freshly generated keys and creates the administrator; the administrator stage (configuration written by
//! hand, database still without an administrator) only creates the administrator. Both stages can then add the
//! optional sample content (`content::sample`) when it was ticked. Afterwards the token file is
//! deleted and the process switches to the forum on the same listening socket, without a restart.
//!
//! The page has only inline styles and one inline script (allowed by its hash) that preselects the browser's time zone,
//! so it works under any path prefix a reverse proxy adds before the forum URL is known; without the script the site
//! time zone stays at the seeded value.
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use askama::Template;
use axum::{
    Form, Router,
    extract::{OriginalUri, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

use crate::{
    cache::{Cache, GlobalCache},
    config::{Config, DatabaseConfig, RedisConfig},
    db::Database,
    identity::Identity,
};

/// Name of the token file under the local directory.
pub const TOKEN_FILE: &str = "setup-token";

/// Preselects the browser's current UTC offset in the time zone dropdown on the first display, adding it as an option
/// when no listed zone has it. Allowed by its hash in [`csp`].
const ZONE_SCRIPT: &str = "(function(){var s=document.querySelector('[data-browser-zone]');if(!s)return;var o=-new Date().getTimezoneOffset();for(var i=0;i<s.options.length;i++){if(Number(s.options[i].value)===o){s.selectedIndex=i;return;}}var a=Math.abs(o),h=Math.floor(a/60),m=a%60,e=document.createElement('option');e.value=String(o);e.textContent=o===0?'UTC':'UTC'+(o<0?'-':'+')+h+(m?':'+String(m).padStart(2,'0'):'');e.selected=true;s.appendChild(e);})();";

/// The seeded `settings.time.offset_minutes` (UTC+8), preselected when the browser's offset is not known.
const SEEDED_OFFSET_MINUTES: i64 = 480;

fn csp() -> HeaderValue {
    static CSP: std::sync::LazyLock<HeaderValue> = std::sync::LazyLock::new(|| {
        use base64::Engine as _;
        use sha2::Digest as _;
        let hash = base64::engine::general_purpose::STANDARD
            .encode(sha2::Sha256::digest(ZONE_SCRIPT.as_bytes()));
        HeaderValue::from_str(&format!(
            "default-src 'none'; script-src 'sha256-{hash}'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
        ))
        .expect("the setup CSP is a valid header value")
    });
    CSP.clone()
}

/// Whether this configuration still needs the configure stage: nothing tells the process which database to use.
pub fn unconfigured(config: &Config) -> bool {
    config.database.url.trim().is_empty()
}

pub enum Finished {
    Completed,
    /// Stopped by a signal before the setup finished.
    Interrupted,
}

enum Stage {
    Configure,
    /// Boxed: `Identity` is much larger than the empty `Configure` variant.
    Administrator(Box<Identity>),
}

pub struct Wizard {
    root: PathBuf,
    config_path: PathBuf,
    config: Config,
    stage: Stage,
}

struct Shared {
    wizard: Wizard,
    token: String,
    token_path: PathBuf,
    done: CancellationToken,
    /// One submission at a time: the steps after the checks (migrating, writing the file, creating the account)
    /// must not interleave.
    busy: tokio::sync::Mutex<()>,
    /// The sign-in address the finished page sends to, kept from the submission that finished the setup.
    login: std::sync::OnceLock<String>,
}

impl Wizard {
    /// No database configured yet: the full wizard. `config_path` is the private configuration it writes (the
    /// `GONOF_CONFIG` file or [`Config::default_path`]); `config` is what was read so far (listen address, local
    /// directories, Redis namespace), kept in the written file.
    pub fn configure(root: PathBuf, config_path: PathBuf, config: Config) -> Self {
        Self {
            root,
            config_path,
            config,
            stage: Stage::Configure,
        }
    }

    /// Configured and migrated, but no administrator yet.
    pub fn administrator(
        root: PathBuf,
        config_path: PathBuf,
        config: Config,
        identity: Identity,
    ) -> Self {
        Self {
            root,
            config_path,
            config,
            stage: Stage::Administrator(Box::new(identity)),
        }
    }

    fn local_directory(&self) -> PathBuf {
        self.root.join(&self.config.identity.local_directory)
    }

    /// The wizard as a router, for [`Wizard::run`] and tests: `done` is cancelled once the setup has completed.
    pub fn app(self) -> Result<App> {
        let token_path = self.local_directory().join(TOKEN_FILE);
        let token = ensure_token(&token_path)?;
        let done = CancellationToken::new();
        let shared = Arc::new(Shared {
            wizard: self,
            token: token.clone(),
            token_path: token_path.clone(),
            done: done.clone(),
            busy: tokio::sync::Mutex::new(()),
            login: std::sync::OnceLock::new(),
        });
        Ok(App {
            router: router(shared),
            done,
            token,
            token_path,
        })
    }

    /// Serves the wizard on `listener` until the setup completes or `shutdown` resolves.
    pub async fn run(
        self,
        listener: std::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<Finished> {
        let App {
            router,
            done,
            token,
            token_path,
        } = self.app()?;
        let address = listener.local_addr()?;
        // The terminal and the journal get the address and the token; forum.install.sh prints the token as well.
        eprintln!(
            "Gono Forum is not set up yet. Open http://{address}/setup (or the forum's address behind your reverse proxy) and enter the setup token from {}: {token}",
            token_path.display()
        );
        let interrupted = CancellationToken::new();
        let stop = {
            let done = done.clone();
            let interrupted = interrupted.clone();
            async move {
                tokio::select! {
                    () = done.cancelled() => {}
                    () = shutdown => interrupted.cancel(),
                }
            }
        };
        crate::systemd::notify("READY=1\nSTATUS=Waiting for the setup wizard");
        axum::serve(tokio::net::TcpListener::from_std(listener)?, router)
            .with_graceful_shutdown(stop)
            .await?;
        Ok(if interrupted.is_cancelled() && !done.is_cancelled() {
            Finished::Interrupted
        } else {
            Finished::Completed
        })
    }
}

pub struct App {
    pub router: Router,
    pub done: CancellationToken,
    pub token: String,
    pub token_path: PathBuf,
}

/// Reads the token forum.install.sh wrote, or writes a new one (mode 0600) when there is none.
fn ensure_token(path: &Path) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let token = text.trim();
        if token.len() >= 16 {
            return Ok(token.to_owned());
        }
    }
    let token = crate::identity::random_token();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    write_private(path, format!("{token}\n").as_bytes())
        .with_context(|| format!("cannot write the setup token to {}", path.display()))?;
    Ok(token)
}

/// Writes a file readable only by the owner, replacing it atomically: a temporary file in the same directory, synced,
/// then renamed over the target.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    if let Ok(directory) = std::fs::File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .fallback_service(get(page).post(submit).with_state(shared))
}

fn secured(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, csp());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // The token travels in the address: never send it on in a Referer.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

async fn healthz() -> Response {
    secured((StatusCode::SERVICE_UNAVAILABLE, "setup required\n").into_response())
}

#[derive(Default, Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

#[derive(Clone, Default, Deserialize)]
struct SetupForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    backend: String,
    #[serde(default)]
    sqlite_path: String,
    #[serde(default)]
    mysql_url: String,
    #[serde(default)]
    redis_enabled: Option<String>,
    #[serde(default)]
    redis_url: String,
    #[serde(default)]
    redis_namespace: String,
    #[serde(default)]
    site_url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password_confirm: String,
    /// Site time zone as a UTC offset in minutes; empty on the first display.
    #[serde(default)]
    offset_minutes: String,
    /// Present when the optional sample content was ticked (`content::sample`).
    #[serde(default)]
    sample_content: Option<String>,
    /// The tag of the language the sample content is written in; empty means the base language.
    #[serde(default)]
    sample_language: String,
}

#[derive(Template)]
#[template(path = "setup/wizard.html")]
struct Page<'a> {
    /// The wizard runs before there is a site language: the process default (the base language in production).
    t: crate::i18n::Lang,
    configure: bool,
    /// The token checked out: show the form (the token rides along in a hidden field).
    authorized: bool,
    token_rejected: bool,
    token_path: &'a str,
    config_path: &'a str,
    error: &'a str,
    form: &'a SetupForm,
    sqlite: bool,
    redis: bool,
    /// The sample content checkbox is ticked.
    sample: bool,
    finished: bool,
    login_url: &'a str,
    zones: Vec<crate::clock::ZoneChoice>,
    /// The preselected time zone.
    offset: i64,
    /// First display: the script may preselect the browser's time zone.
    browser_zone: bool,
    zone_script: &'static str,
}

impl Page<'_> {
    /// The time zone dropdown for `form`: what was submitted, otherwise the seeded value.
    fn with_zone(mut self) -> Self {
        let submitted = self.form.offset_minutes.trim().parse::<i64>().ok();
        self.offset = submitted.unwrap_or(SEEDED_OFFSET_MINUTES);
        self.browser_zone = submitted.is_none();
        self.zones = crate::clock::zone_choices(self.offset);
        self
    }

    /// The languages the sample content can be written in, the base first.
    fn sample_languages(&self) -> Vec<crate::i18n::Lang> {
        crate::content::sample::languages().collect()
    }

    /// The tag of the sample language to preselect: what was submitted, otherwise the base language.
    fn sample_tag(&self) -> &str {
        match self.form.sample_language.trim() {
            "" => crate::i18n::Lang::EN.tag(),
            tag => tag,
        }
    }
}

fn valid_token(shared: &Shared, candidate: &str) -> bool {
    let candidate = candidate.trim();
    !candidate.is_empty() && bool::from(candidate.as_bytes().ct_eq(shared.token.as_bytes()))
}

fn render(page: Page<'_>) -> Response {
    match page.render() {
        Ok(html) => secured(Html(html).into_response()),
        Err(error) => {
            tracing::error!(%error, "setup page failed to render");
            secured(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

/// Values shown the first time: the instance's own paths and, for the forum URL, the address the browser used
/// (scheme from the reverse proxy's `X-Forwarded-Proto`, path prefix from where the wizard was opened).
fn defaults(shared: &Shared, headers: &HeaderMap, uri: &axum::http::Uri) -> SetupForm {
    let wizard = &shared.wizard;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| matches!(*value, "http" | "https"))
        .unwrap_or("http");
    let path = uri.path().trim_end_matches('/');
    let prefix = path.strip_suffix("/setup").unwrap_or(path);
    let site_url = if host.is_empty() {
        String::new()
    } else {
        crate::config::normalize_forum_url(&format!("{scheme}://{host}{prefix}"))
            .unwrap_or_default()
    };
    SetupForm {
        backend: "sqlite".into(),
        sqlite_path: wizard
            .local_directory()
            .join("db/gonof.db")
            .display()
            .to_string(),
        redis_namespace: wizard.config.redis.namespace.clone(),
        site_url,
        username: "admin".into(),
        ..SetupForm::default()
    }
}

async fn page(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<TokenQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    // Another process may have finished the administrator stage meanwhile (several processes on one database).
    if let Stage::Administrator(identity) = &shared.wizard.stage
        && matches!(identity.needs_admin().await, Ok(false))
    {
        shared.done.cancel();
        return finished(&shared, &saved_login(&shared).await);
    }
    let mut form = defaults(&shared, &headers, &uri);
    let authorized = valid_token(&shared, &query.token);
    if authorized {
        form.token = query.token.trim().to_owned();
    }
    let token_path = shared.token_path.display().to_string();
    render(
        Page {
            t: crate::i18n::current(),
            configure: matches!(shared.wizard.stage, Stage::Configure),
            authorized,
            token_rejected: !query.token.is_empty() && !authorized,
            token_path: &token_path,
            config_path: &shared.wizard.config_path.display().to_string(),
            error: "",
            sqlite: true,
            redis: false,
            sample: false,
            form: &form,
            finished: false,
            login_url: "",
            zones: Vec::new(),
            offset: SEEDED_OFFSET_MINUTES,
            browser_zone: false,
            zone_script: ZONE_SCRIPT,
        }
        .with_zone(),
    )
}

/// The forum's sign-in page under the forum address saved in the database: with the forum under a sub-path, `/login`
/// at the root of the host is not the forum's. The administrator stage never saves the address itself, an earlier
/// interrupted install did, and the finished page is also shown when another submission or process finished.
async fn saved_login(shared: &Shared) -> String {
    if let Some(login) = shared.login.get() {
        return login.clone();
    }
    let site = match &shared.wizard.stage {
        Stage::Administrator(identity) => match crate::runtime::read(identity.storage().0).await {
            Ok(settings) => settings.site.url,
            Err(error) => {
                tracing::warn!(%error, "could not read the forum address for the sign-in link");
                String::new()
            }
        },
        Stage::Configure => String::new(),
    };
    format!("{}/login", site.trim_end_matches('/'))
}

fn finished(shared: &Shared, login_url: &str) -> Response {
    let token_path = shared.token_path.display().to_string();
    render(
        Page {
            t: crate::i18n::current(),
            configure: matches!(shared.wizard.stage, Stage::Configure),
            authorized: true,
            token_rejected: false,
            token_path: &token_path,
            config_path: &shared.wizard.config_path.display().to_string(),
            error: "",
            sqlite: true,
            redis: false,
            sample: false,
            form: &SetupForm::default(),
            finished: true,
            login_url,
            zones: Vec::new(),
            offset: SEEDED_OFFSET_MINUTES,
            browser_zone: false,
            zone_script: ZONE_SCRIPT,
        }
        .with_zone(),
    )
}

async fn submit(State(shared): State<Arc<Shared>>, Form(form): Form<SetupForm>) -> Response {
    if !valid_token(&shared, &form.token) {
        let token_path = shared.token_path.display().to_string();
        return render(
            Page {
                t: crate::i18n::current(),
                configure: matches!(shared.wizard.stage, Stage::Configure),
                authorized: false,
                token_rejected: true,
                token_path: &token_path,
                config_path: &shared.wizard.config_path.display().to_string(),
                error: "",
                sqlite: true,
                redis: false,
                sample: false,
                form: &SetupForm::default(),
                finished: false,
                login_url: "",
                zones: Vec::new(),
                offset: SEEDED_OFFSET_MINUTES,
                browser_zone: false,
                zone_script: ZONE_SCRIPT,
            }
            .with_zone(),
        );
    }
    let _busy = shared.busy.lock().await;
    if shared.done.is_cancelled() {
        return finished(&shared, &saved_login(&shared).await);
    }
    let outcome = match &shared.wizard.stage {
        Stage::Configure => configure(&shared.wizard, &form).await,
        Stage::Administrator(identity) => create_admin(identity, &form).await.map(|()| None),
    };
    match outcome {
        Ok(site_url) => {
            if let Err(error) = std::fs::remove_file(&shared.token_path) {
                tracing::warn!(%error, path = %shared.token_path.display(), "could not delete the setup token");
            }
            let login_url = match site_url {
                Some(site_url) => format!("{site_url}/login"),
                None => saved_login(&shared).await,
            };
            let _ = shared.login.set(login_url.clone());
            tracing::info!("setup wizard finished; starting the forum");
            shared.done.cancel();
            finished(&shared, &login_url)
        }
        Err(message) => {
            let token_path = shared.token_path.display().to_string();
            let form = SetupForm {
                password: String::new(),
                password_confirm: String::new(),
                ..form
            };
            render(
                Page {
                    t: crate::i18n::current(),
                    configure: matches!(shared.wizard.stage, Stage::Configure),
                    authorized: true,
                    token_rejected: false,
                    token_path: &token_path,
                    config_path: &shared.wizard.config_path.display().to_string(),
                    error: &message,
                    sqlite: form.backend != "mysql",
                    redis: form.redis_enabled.is_some(),
                    sample: form.sample_content.is_some(),
                    form: &form,
                    finished: false,
                    login_url: "",
                    zones: Vec::new(),
                    offset: SEEDED_OFFSET_MINUTES,
                    browser_zone: false,
                    zone_script: ZONE_SCRIPT,
                }
                .with_zone(),
            )
        }
    }
}

fn shown(error: &anyhow::Error) -> String {
    crate::debug::visible_error(error)
}

fn admin_input(form: &SetupForm) -> std::result::Result<(), String> {
    if form.password != form.password_confirm {
        return Err(crate::i18n::current()
            .setup_error_password_mismatch()
            .into());
    }
    Ok(())
}

async fn create_admin(identity: &Identity, form: &SetupForm) -> std::result::Result<(), String> {
    admin_input(form)?;
    time_zone(form)?;
    let sample = sample_choice(form)?;
    let (db, global) = identity.storage();
    sample_fits(db, form).await?;
    let email = Some(form.email.trim()).filter(|email| !email.is_empty());
    let uid = identity
        .setup_admin(&form.username, email, &form.password)
        .await
        .map_err(|error| error.to_string())?;
    save_time_zone(db, global, uid, form).await;
    add_sample(db, sample).await;
    Ok(())
}

/// Before anything is written: the sample content, when ticked, needs a site without forums, threads or posts.
async fn sample_fits(db: &Database, form: &SetupForm) -> std::result::Result<(), String> {
    if form.sample_content.is_none() {
        return Ok(());
    }
    match crate::content::sample_fits(db).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(crate::i18n::current().setup_error_sample_content().into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The language of the ticked sample content: `None` when the box is not ticked, the base language when the form
/// names none (a form rendered before the choice existed), otherwise a language that has a sample.
fn sample_choice(form: &SetupForm) -> std::result::Result<Option<crate::i18n::Lang>, String> {
    if form.sample_content.is_none() {
        return Ok(None);
    }
    match form.sample_language.trim() {
        "" => Ok(Some(crate::i18n::Lang::EN)),
        tag => crate::content::sample::language(tag)
            .map(Some)
            .ok_or_else(|| crate::i18n::current().setup_error_sample_language().into()),
    }
}

/// Adds the sample content in the chosen language when it was ticked, after the administrator exists. Like the time
/// zone, a failure here no longer stops the setup: it is logged, the forum starts empty, and `gono-forum-cli
/// seed-sample` can add the content later (the import is one transaction, so nothing half-written stays behind).
async fn add_sample(db: &Database, sample: Option<crate::i18n::Lang>) {
    let Some(lang) = sample else {
        return;
    };
    if let Err(error) = crate::content::import_sample(db, crate::identity::now(), lang).await {
        tracing::warn!(
            error = %format_args!("{error:#}"),
            language = lang.tag(),
            "setup wizard could not add the sample content; gono-forum-cli seed-sample --language=<language> adds it later"
        );
    }
}

/// The submitted site time zone; `None` when the form had none (a form rendered before the dropdown existed).
fn time_zone(form: &SetupForm) -> std::result::Result<Option<i64>, String> {
    match form.offset_minutes.trim() {
        "" => Ok(None),
        value => value
            .parse::<i64>()
            .ok()
            .filter(|minutes| (-12 * 60..=14 * 60).contains(minutes))
            .map(Some)
            .ok_or_else(|| crate::i18n::current().setup_error_time_zone().into()),
    }
}

/// Saves the chosen site time zone as the new administrator. The account already exists by now, so a failure only
/// leaves the seeded time zone, which the administrator can change in the admin area; it is logged, not shown.
async fn save_time_zone(db: &Database, global: &GlobalCache, uid: i64, form: &SetupForm) {
    let Ok(Some(minutes)) = time_zone(form) else {
        return;
    };
    let saved = async {
        let credentials_version: i64 = crate::db::query_scalar(
            "SELECT credentials_version FROM gonof_common_member WHERE uid=?",
        )
        .bind(uid)
        .fetch_one(db.executor())
        .await?;
        let actor = crate::identity::Actor {
            uid,
            credentials_version,
            api_key: None,
        };
        crate::runtime::update(db, global, actor, "site.settings.save", |settings| {
            settings.time.offset_minutes = minutes;
        })
        .await
    }
    .await;
    if let Err(error) = saved {
        tracing::warn!(%error, "setup wizard could not save the site time zone");
    }
}

/// The configure stage. Returns the saved forum URL (`None` when left empty).
async fn configure(
    wizard: &Wizard,
    form: &SetupForm,
) -> std::result::Result<Option<String>, String> {
    admin_input(form)?;
    time_zone(form)?;
    let sample = sample_choice(form)?;
    let database = DatabaseConfig {
        url: match form.backend.as_str() {
            "sqlite" => {
                let path = form.sqlite_path.trim();
                if path.is_empty() {
                    return Err(crate::i18n::current().setup_error_sqlite_path().into());
                }
                // The path as typed: characters a URL reads differently (`%`, `?`, `#`) are escaped, so the
                // file created is the one named here.
                crate::db::sqlite::url_for(&wizard.root.join(path))
                    .map_err(|error| shown(&error))?
            }
            "mysql" => form.mysql_url.trim().to_owned(),
            _ => return Err(crate::i18n::current().setup_error_backend().into()),
        },
        ..wizard.config.database.clone()
    };
    database.validate().map_err(|error| shown(&error))?;
    let redis = RedisConfig {
        enabled: form.redis_enabled.is_some(),
        url: if form.redis_enabled.is_some() {
            form.redis_url.trim().to_owned()
        } else {
            String::new()
        },
        namespace: Some(form.redis_namespace.trim())
            .filter(|namespace| !namespace.is_empty())
            .unwrap_or(&wizard.config.redis.namespace)
            .to_owned(),
        ..wizard.config.redis.clone()
    };
    redis.validate().map_err(|error| shown(&error))?;
    let site_url = match form.site_url.trim() {
        "" => None,
        url => Some(
            crate::config::normalize_forum_url(url)
                .map_err(|error| crate::i18n::current().setup_error_site_url(shown(&error)))?,
        ),
    };
    if redis.enabled {
        Cache::connect(&redis)
            .await
            .map_err(|error| crate::i18n::current().setup_error_redis(shown(&error)))?;
    }
    if let Some(file) = crate::db::sqlite::file_of(&database.url)
        && let Some(parent) = file.parent()
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            crate::i18n::current()
                .setup_error_directory(parent.display().to_string(), error.to_string())
        })?;
    }
    let db = Database::connect_creating(&database)
        .await
        .map_err(|error| crate::i18n::current().setup_error_database(shown(&error)))?;
    let (tables, migrated) = db
        .table_census()
        .await
        .map_err(|error| crate::i18n::current().setup_error_tables(shown(&error)))?;
    if tables > 0 && !migrated {
        return Err(crate::i18n::current().setup_error_foreign_tables().into());
    }
    let detached = GlobalCache::new(
        Cache::new(&RedisConfig {
            enabled: false,
            ..RedisConfig::default()
        })
        .map_err(|error| shown(&error))?,
    );
    let probe = Identity::new(
        db.clone(),
        wizard.config.identity.clone(),
        wizard.root.clone(),
        detached.clone(),
    );
    // A site that is already installed is refused before its schema changes: the wizard pointed at a live database by
    // mistake must leave it as it was. A check that fails on an older schema is not a refusal; the database is
    // migrated as before and checked again after.
    if migrated && matches!(probe.needs_admin().await, Ok(false)) {
        return Err(crate::i18n::current().setup_error_installed().into());
    }
    crate::systemd::extend_start(600);
    db.migrate()
        .await
        .map_err(|error| crate::i18n::current().setup_error_migrate(shown(&error)))?;
    if !probe
        .needs_admin()
        .await
        .map_err(|error| error.to_string())?
    {
        return Err(crate::i18n::current().setup_error_installed().into());
    }
    sample_fits(&db, form).await?;
    let settings = crate::runtime::read(&db)
        .await
        .map_err(|error| error.to_string())?;
    let email = Some(form.email.trim()).filter(|email| !email.is_empty());
    crate::identity::check_new_admin(
        &form.username,
        email,
        &form.password,
        settings.account.password_min_length,
    )
    .map_err(|error| error.to_string())?;
    if let Some(url) = &site_url {
        crate::runtime::set_site_url(&db, &detached, url)
            .await
            .map_err(|error| crate::i18n::current().setup_error_site_url(error.to_string()))?;
    }
    let config = write_config(wizard, &database, &redis)
        .map_err(|error| crate::i18n::current().setup_error_config_file(shown(&error)))?;
    let identity = Identity::new(
        db.clone(),
        config.identity,
        wizard.root.clone(),
        detached.clone(),
    );
    let uid = identity
        .setup_admin(&form.username, email, &form.password)
        .await
        .map_err(|error| error.to_string())?;
    save_time_zone(&db, &detached, uid, form).await;
    add_sample(&db, sample).await;
    db.pool.close().await;
    // Left empty, the address an earlier interrupted attempt saved still holds.
    Ok(site_url.or(Some(settings.site.url).filter(|url| !url.is_empty())))
}

/// One section of the existing configuration, or an empty one.
fn section(table: &toml::Table, name: &str) -> Result<toml::Table> {
    match table.get(name) {
        Some(toml::Value::Table(inner)) => Ok(inner.clone()),
        Some(_) => anyhow::bail!("[{name}] in the existing configuration is not a table"),
        None => Ok(toml::Table::new()),
    }
}

/// Writes the private configuration: what the file already had (forum.install.sh writes the listen address and local
/// directories) plus the database, Redis and three freshly generated keys. Returns it re-read through the normal
/// loader, so a file the forum would refuse never counts as written.
fn write_config(wizard: &Wizard, database: &DatabaseConfig, redis: &RedisConfig) -> Result<Config> {
    let mut table: toml::Table = match std::fs::read_to_string(&wizard.config_path) {
        Ok(text) => {
            toml::from_str(&text).context("the existing configuration is not valid TOML")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(error) => return Err(error).context("cannot read the existing configuration"),
    };
    // The listen address and local directories the wizard ran with are written out, so the file says where the
    // instance keeps its data.
    let current = &wizard.config;
    table
        .entry("listen")
        .or_insert_with(|| current.listen.to_string().into());
    let mut database_table = section(&table, "database")?;
    database_table.insert("url".into(), database.url.clone().into());
    table.insert("database".into(), database_table.into());
    let mut redis_table = section(&table, "redis")?;
    redis_table.insert("enabled".into(), redis.enabled.into());
    redis_table.insert("url".into(), redis.url.clone().into());
    redis_table.insert("namespace".into(), redis.namespace.clone().into());
    table.insert("redis".into(), redis_table.into());
    // Keys already supplied (in the file or through GONOF_* variables) are kept; only missing ones are generated.
    let keys = &wizard.config;
    let mut identity_table = section(&table, "identity")?;
    identity_table.entry("local_directory").or_insert_with(|| {
        current
            .identity
            .local_directory
            .display()
            .to_string()
            .into()
    });
    let mut media_table = section(&table, "media")?;
    media_table
        .entry("local_directory")
        .or_insert_with(|| current.media.local_directory.display().to_string().into());
    table.insert("media".into(), media_table.into());
    if keys.identity.cookie.active_key.is_empty() {
        let mut cookie = section(&identity_table, "cookie")?;
        cookie.insert("active_key".into(), crate::identity::random_token().into());
        identity_table.insert("cookie".into(), cookie.into());
    }
    if keys.identity.two_factor.active_key.is_empty() {
        let mut two_factor = section(&identity_table, "two_factor")?;
        two_factor.insert("active_key".into(), crate::identity::random_token().into());
        identity_table.insert("two_factor".into(), two_factor.into());
    }
    table.insert("identity".into(), identity_table.into());
    if keys.common.encryption_key.is_empty() {
        let mut common = section(&table, "common")?;
        common.insert(
            "encryption_key".into(),
            crate::identity::random_token().into(),
        );
        table.insert("common".into(), common.into());
    }
    let body = toml::to_string(&table).context("cannot serialize the configuration")?;
    let text = format!(
        "# Gono Forum private configuration, written by the setup wizard.\n# Keep it private and back it up with the database: the keys encrypt sign-in cookies, two-step verification\n# secrets and the service passwords saved in the admin area. Losing them signs everyone out and locks members with\n# two-step verification out of their accounts.\n\n{body}"
    );
    if let Some(parent) = wizard.config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    write_private(&wizard.config_path, text.as_bytes())
        .with_context(|| format!("cannot write {}", wizard.config_path.display()))?;
    Config::load(&wizard.root, Some(&wizard.config_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wizard in English has no untranslated text in any of its stages.
    #[test]
    fn the_english_wizard_has_no_chinese() {
        use askama::Template;
        let form = SetupForm::default();
        for (configure, authorized, token_rejected, finished, error) in [
            (true, false, true, false, ""),
            (true, true, false, false, "x"),
            (false, true, false, false, ""),
            (true, true, false, true, ""),
        ] {
            let html = crate::i18n::in_language(crate::i18n::Lang::EN, || {
                Page {
                    t: crate::i18n::Lang::EN,
                    configure,
                    authorized,
                    token_rejected,
                    token_path: "/opt/gono-forum/data/setup-token",
                    config_path: "/opt/gono-forum/config/gono-forum.toml",
                    error,
                    form: &form,
                    sqlite: true,
                    redis: false,
                    sample: configure,
                    finished,
                    login_url: "https://forum.example.test/login",
                    zones: Vec::new(),
                    offset: SEEDED_OFFSET_MINUTES,
                    browser_zone: false,
                    zone_script: ZONE_SCRIPT,
                }
                .with_zone()
                .render()
                .unwrap()
            });
            // The only other language allowed is each sample language's own name, in its dropdown option.
            let mut rest = html.clone();
            for lang in crate::content::sample::languages() {
                rest = rest.replace(&format!("lang=\"{lang}\">{}</option>", lang.name()), "");
            }
            assert!(
                !rest.chars().any(|c| ('\u{3000}'..='\u{9fff}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{html}"
            );
            assert!(html.contains("<html lang=\"en\">"));
            if authorized && !finished {
                assert!(
                    html.contains("<option value=\"en\" selected lang=\"en\">English</option>"),
                    "the sample is in the base language unless another is chosen: {html}"
                );
            }
        }
    }

    #[test]
    fn tokens_are_created_private_and_reused() {
        let dir = std::env::temp_dir().join(format!(
            "gonof-setup-token-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("nested").join(TOKEN_FILE);
        let first = ensure_token(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(ensure_token(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // forum.install.sh writes its own token with a trailing newline.
        std::fs::write(&path, "abcdefabcdefabcdef0123\n").unwrap();
        assert_eq!(ensure_token(&path).unwrap(), "abcdefabcdefabcdef0123");
        // Too short to be a token: replaced.
        std::fs::write(&path, "short\n").unwrap();
        assert_ne!(ensure_token(&path).unwrap(), "short");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
