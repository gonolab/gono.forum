//! Setup wizard: the one-time token, the configure stage on the fixture's empty database (either backend), refusing
//! foreign tables and finished installs, resuming after an interruption, the administrator stage and the optional sample
//! content.
use std::path::{Path, PathBuf};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    i18n::Lang,
    identity::Identity,
    setup::{App, Wizard},
    test_support::{TestFixture, detached_global},
};
use tower::ServiceExt;

const PASSWORD: &str = "Wizard-password-2026";

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gonof_setup_{}_{}",
            std::process::id(),
            &gono_forum_web::identity::random_token()[..24]
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn config(&self) -> PathBuf {
        self.0.join("config/gono-forum.toml")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a fresh instance has read before setup: the fixture's paths and pool limits, but no database, keys or Redis.
fn fresh(services: &TestFixture) -> Config {
    let mut config = services.config.clone();
    config.database.url.clear();
    config.identity.cookie.active_key.clear();
    config.identity.cookie.previous_keys.clear();
    config.identity.two_factor.active_key.clear();
    config.identity.two_factor.previous_keys.clear();
    config.common.encryption_key.clear();
    config.redis.enabled = false;
    config.redis.url.clear();
    config
}

/// Form fields that point the wizard at the fixture's own (empty) database.
fn database_fields(services: &TestFixture) -> Vec<(&'static str, String)> {
    let url = &services.config.database.url;
    match url.strip_prefix("sqlite://") {
        Some(path) => vec![
            ("backend", "sqlite".into()),
            ("sqlite_path", path.split('?').next().unwrap().into()),
        ],
        None => vec![("backend", "mysql".into()), ("mysql_url", url.clone())],
    }
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

async fn get(router: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    send(
        router,
        Request::get(uri)
            .header(header::HOST, "forum.example.com")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post(router: &Router, fields: &[(&str, String)]) -> (StatusCode, HeaderMap, String) {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().map(|(key, value)| (*key, value.as_str())))
        .finish();
    send(
        router,
        Request::post("/setup")
            .header(header::HOST, "forum.example.com")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

fn install_fields(services: &TestFixture, app: &App) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("token", app.token.clone()),
        ("site_url", "https://forum.example.com/bbs".into()),
        ("username", "Owner".into()),
        ("email", "owner@example.com".into()),
        ("password", PASSWORD.into()),
        ("password_confirm", PASSWORD.into()),
        ("redis_enabled", "1".into()),
        ("redis_url", services.config.redis.url.clone()),
        ("redis_namespace", services.config.redis.namespace.clone()),
        ("offset_minutes", "-300".into()),
    ];
    fields.extend(database_fields(services));
    fields
}

async fn tables(services: &TestFixture) -> i64 {
    services.db.table_census().await.unwrap().0
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn configure_stage_installs_on_an_empty_database() {
    let services = TestFixture::new().await.unwrap();
    let root = Root::new();
    let app = Wizard::configure(root.0.clone(), root.config(), fresh(&services))
        .app()
        .unwrap();
    let router = app.router.clone();
    assert_eq!(app.token_path, root.0.join(".local/setup-token"));
    assert_eq!(
        std::fs::read_to_string(&app.token_path).unwrap().trim(),
        app.token
    );
    #[cfg(unix)]
    assert_eq!(mode(&app.token_path), 0o600);

    let (status, _, body) = get(&router, "/healthz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, "setup required\n");
    // Every path shows the wizard; without the token only the token prompt.
    let (status, headers, body) = get(&router, "/forum/thread/1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Setup token") && !body.contains("name=\"backend\""));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("default-src 'none'")
    );
    let (_, _, body) = get(&router, "/setup?token=wrong").await;
    assert!(body.contains("That token is wrong"), "{body}");
    // With the token: defaults from the instance and from the address the browser used.
    let (_, _, body) = get(&router, &format!("/bbs/setup?token={}", app.token)).await;
    assert!(body.contains("name=\"backend\""), "{body}");
    assert!(
        body.contains("value=\"https://forum.example.com/bbs\""),
        "{body}"
    );
    assert!(body.contains(&format!(
        "value=\"{}\"",
        root.0.join(".local/db/gonof.db").display()
    )));
    // The time zone dropdown starts at the seeded UTC+8; the inline script that preselects the browser's time zone is
    // allowed by its hash only.
    assert!(
        body.contains("name=\"offset_minutes\" data-browser-zone"),
        "{body}"
    );
    assert!(body.contains("<option value=\"480\" selected>"), "{body}");
    assert!(body.contains("<script>(function(){"), "{body}");
    let (_, headers, page) = get(&router, &format!("/setup?token={}", app.token)).await;
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    let script = page
        .split_once("<script>")
        .and_then(|(_, rest)| rest.split_once("</script>"))
        .map(|(script, _)| script)
        .unwrap();
    let hash = {
        use base64::Engine as _;
        use sha2::Digest as _;
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(script.as_bytes()))
    };
    assert!(
        csp.contains(&format!("'sha256-{hash}'")),
        "the CSP allows exactly the script on the page: {csp}"
    );
    let scripts = csp
        .split(';')
        .find(|directive| directive.trim().starts_with("script-src"))
        .unwrap_or_default();
    assert!(
        scripts.contains("'sha256-") && !scripts.contains("unsafe"),
        "{csp}"
    );

    // A wrong token or mismatched passwords write nothing.
    let mut fields = install_fields(&services, &app);
    fields[0].1 = "wrong".into();
    let (_, _, body) = post(&router, &fields).await;
    assert!(body.contains("That token is wrong"), "{body}");
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| *key != "password_confirm");
    fields.push(("password_confirm", "something-else-entirely".into()));
    let (_, _, body) = post(&router, &fields).await;
    assert!(
        body.contains("The two administrator passwords do not match"),
        "{body}"
    );
    assert!(
        !body.contains(PASSWORD),
        "the password is not filled back in"
    );
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| *key != "offset_minutes");
    fields.push(("offset_minutes", "9999".into()));
    let (_, _, body) = post(&router, &fields).await;
    assert!(body.contains("Choose a valid site time zone"), "{body}");
    assert!(
        !body.contains("data-browser-zone"),
        "the chosen time zone is filled back in, not preselected from the browser"
    );
    assert!(!root.config().exists());
    assert_eq!(tables(&services).await, 0);
    assert!(!app.done.is_cancelled());

    let (status, _, body) = post(&router, &install_fields(&services, &app)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Installation finished"), "{body}");
    assert!(
        body.contains("url=https://forum.example.com/bbs/login"),
        "{body}"
    );
    assert!(app.done.is_cancelled());
    assert!(!app.token_path.exists(), "the token is deleted once used");

    // The written configuration loads through the normal loader with three fresh, distinct keys.
    #[cfg(unix)]
    assert_eq!(mode(&root.config()), 0o600);
    let text = std::fs::read_to_string(root.config()).unwrap();
    assert!(text.starts_with("# Gono Forum private configuration"));
    let config = Config::load(&root.0, Some(&root.config())).unwrap();
    assert_eq!(config.database.url, services.config.database.url);
    assert!(config.redis.enabled);
    assert_eq!(config.redis.namespace, services.config.redis.namespace);
    let keys = [
        config.identity.cookie.active_key.clone(),
        config.identity.two_factor.active_key.clone(),
        config.common.encryption_key.clone(),
    ];
    for key in &keys {
        assert_eq!(key.len(), 64);
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    assert!(keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2]);
    assert_eq!(config.listen, services.config.listen);
    assert_eq!(config.identity.local_directory, Path::new(".local"));

    // The administrator, the forum URL and a working login.
    let row = gono_forum_web::db::query(
        "SELECT username, email, emailstatus, adminid FROM gonof_common_member",
    )
    .fetch_all(services.db.executor())
    .await
    .unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(
        gono_forum_web::db::text(&row[0], "username").unwrap(),
        "owner"
    );
    let site = gono_forum_web::runtime::read(&services.db).await.unwrap();
    assert_eq!(site.site.url, "https://forum.example.com/bbs");
    assert_eq!(
        site.time.offset_minutes, -300,
        "the site time zone chosen in the wizard"
    );
    let identity = Identity::new(
        services.db.clone(),
        config.identity.clone(),
        root.0.clone(),
        detached_global(),
    );
    assert!(!identity.needs_admin().await.unwrap());
    let grant = identity.login("owner@example.com", PASSWORD).await.unwrap();
    assert!(grant.member.is_admin);
    assert!(grant.member.email_verified);

    // Running the wizard again on the finished install refuses before writing anything.
    let other = Root::new();
    let again = Wizard::configure(other.0.clone(), other.config(), fresh(&services))
        .app()
        .unwrap();
    let (_, _, body) = post(&again.router, &install_fields(&services, &again)).await;
    assert!(
        body.contains("Gono Forum is already installed in this database"),
        "{body}"
    );
    assert!(!other.config().exists());
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn configure_stage_refuses_foreign_tables_and_resumes_an_interrupted_install() {
    let services = TestFixture::new().await.unwrap();
    let root = Root::new();
    let app = Wizard::configure(root.0.clone(), root.config(), fresh(&services))
        .app()
        .unwrap();
    gono_forum_web::db::raw_sql("CREATE TABLE other_application (id INTEGER PRIMARY KEY)")
        .execute(services.db.executor())
        .await
        .unwrap();
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| *key != "redis_enabled");
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(
        body.contains("This database already has other tables"),
        "{body}"
    );
    assert!(!root.config().exists());
    gono_forum_web::db::raw_sql("DROP TABLE other_application")
        .execute(services.db.executor())
        .await
        .unwrap();
    // An earlier attempt migrated, saved the forum address and then stopped (say, the configuration could not be
    // written): the same database is accepted, because it is migrated and still has no administrator.
    services.db.migrate().await.unwrap();
    gono_forum_web::test_support::set_site_url(&services.db, "https://forum.example.com/bbs")
        .await
        .unwrap();
    // Neither the site URL nor Redis is required; the address saved before still leads to the sign-in page.
    fields.retain(|(key, _)| *key != "site_url");
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    assert!(
        body.contains("url=https://forum.example.com/bbs/login"),
        "{body}"
    );
    let config = Config::load(&root.0, Some(&root.config())).unwrap();
    assert!(!config.redis.enabled);
    services.cleanup().await.unwrap();
}

/// A database whose site is already installed is refused before the wizard migrates it: picking a live site's database
/// by mistake leaves its schema as it was.
#[tokio::test]
async fn configure_stage_refuses_an_installed_site_before_migrating_it() {
    let services = TestFixture::new().await.unwrap();
    // A site set up on an older version: migrated to 4 of 5, with its administrator created.
    services.db.migrate_until(4).await.unwrap();
    gono_forum_web::db::query(
        "UPDATE gonof_common_settings SET svalue='complete' WHERE skey='identity.bootstrap_admin'",
    )
    .execute(services.db.executor())
    .await
    .unwrap();
    let root = Root::new();
    let app = Wizard::configure(root.0.clone(), root.config(), fresh(&services))
        .app()
        .unwrap();
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| *key != "redis_enabled");
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(
        body.contains("Gono Forum is already installed in this database"),
        "{body}"
    );
    assert_eq!(
        count(&services, "SELECT MAX(version) FROM _sqlx_migrations").await,
        4,
        "the refused database is not migrated"
    );
    assert!(!root.config().exists());
    services.cleanup().await.unwrap();
}

/// The SQLite path is the file name as typed, even with characters that mean something in a URL.
#[tokio::test]
async fn configure_stage_creates_the_sqlite_file_it_was_given() {
    if !gono_forum_web::test_support::sqlite_tests() {
        return;
    }
    let services = TestFixture::new().await.unwrap();
    let root = Root::new();
    let app = Wizard::configure(root.0.clone(), root.config(), fresh(&services))
        .app()
        .unwrap();
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| !matches!(*key, "redis_enabled" | "sqlite_path"));
    fields.push(("sqlite_path", "db/community%20archive #1?.db".into()));
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    let file = root.0.join("db/community%20archive #1?.db");
    assert!(file.is_file(), "{}", file.display());
    assert!(!root.0.join("db/community archive #1").exists());
    let config = Config::load(&root.0, Some(&root.config())).unwrap();
    assert_eq!(
        gono_forum_web::db::sqlite::file_of(&config.database.url).unwrap(),
        file
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn administrator_stage_only_creates_the_administrator() {
    let services = TestFixture::migrated().await.unwrap();
    // The interrupted install saved the forum address under a sub-path; the finished page leads to its sign-in page.
    gono_forum_web::test_support::set_site_url(&services.db, "https://forum.example.com/bbs")
        .await
        .unwrap();
    let login = "url=https://forum.example.com/bbs/login";
    let root = Root::new();
    let identity = Identity::new(
        services.db.clone(),
        services.config.identity.clone(),
        root.0.clone(),
        detached_global(),
    );
    let app = Wizard::administrator(
        root.0.clone(),
        root.config(),
        services.config.clone(),
        identity.clone(),
    )
    .app()
    .unwrap();
    let (_, _, body) = get(&app.router, &format!("/setup?token={}", app.token)).await;
    assert!(
        body.contains("Create administrator") && !body.contains("name=\"backend\""),
        "{body}"
    );
    let mut fields = vec![
        ("token", app.token.clone()),
        ("username", "admin".to_owned()),
        ("password", "too-short".to_owned()),
        ("password_confirm", "too-short".to_owned()),
    ];
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(
        body.contains("The password length does not meet the requirement"),
        "{body}"
    );
    assert!(identity.needs_admin().await.unwrap());
    fields[2].1 = PASSWORD.into();
    fields[3].1 = PASSWORD.into();
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    assert!(body.contains(login), "{body}");
    assert!(app.done.is_cancelled());
    assert!(!identity.needs_admin().await.unwrap());
    // Submitted again, or opened once the administrator exists: the same page and address.
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains(login), "{body}");
    let (_, _, body) = get(&app.router, &format!("/setup?token={}", app.token)).await;
    assert!(body.contains(login), "{body}");
    assert!(identity.login("admin", PASSWORD).await.is_ok());
    // The administrator stage never writes the configuration.
    assert!(!root.config().exists());
    services.cleanup().await.unwrap();
}

async fn count(services: &TestFixture, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(services.db.executor())
        .await
        .unwrap()
}

#[tokio::test]
async fn configure_stage_adds_the_sample_content_when_ticked() {
    let services = TestFixture::new().await.unwrap();
    let root = Root::new();
    let app = Wizard::configure(root.0.clone(), root.config(), fresh(&services))
        .app()
        .unwrap();
    // Offered, off by default.
    let (_, _, body) = get(&app.router, &format!("/setup?token={}", app.token)).await;
    assert!(
        body.contains("<input type=\"checkbox\" name=\"sample_content\" value=\"1\"> Fill the forum with sample content"),
        "{body}"
    );
    assert!(
        body.contains("Adds 10 members, 5 forums, 100 threads and 1,000 replies"),
        "{body}"
    );
    assert!(
        body.contains("<select id=\"sample_language\" name=\"sample_language\"")
            && body.contains("<option value=\"en\" selected lang=\"en\">English</option>"),
        "the sample language is offered, the base language first: {body}"
    );
    let mut fields = install_fields(&services, &app);
    fields.retain(|(key, _)| *key != "redis_enabled");
    fields.push(("sample_content", "1".into()));
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    assert_eq!(
        count(
            &services,
            "SELECT uid FROM gonof_common_member WHERE adminid = 1"
        )
        .await,
        1,
        "the administrator is created first and keeps uid 1"
    );
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM gonof_common_member").await,
        11
    );
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM gonof_forum_forum").await,
        5
    );
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        100
    );
    assert_eq!(
        count(
            &services,
            "SELECT COUNT(*) FROM gonof_forum_post WHERE first = 0"
        )
        .await,
        1000
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn administrator_stage_refuses_sample_content_on_a_database_with_content() {
    let services = TestFixture::migrated().await.unwrap();
    gono_forum_web::content::seed_fixture(&services.db)
        .await
        .unwrap();
    let root = Root::new();
    let identity = Identity::new(
        services.db.clone(),
        services.config.identity.clone(),
        root.0.clone(),
        detached_global(),
    );
    let app = Wizard::administrator(
        root.0.clone(),
        root.config(),
        services.config.clone(),
        identity.clone(),
    )
    .app()
    .unwrap();
    let mut fields = vec![
        ("token", app.token.clone()),
        ("username", "admin".to_owned()),
        ("password", PASSWORD.to_owned()),
        ("password_confirm", PASSWORD.to_owned()),
        ("sample_content", "1".to_owned()),
    ];
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(
        body.contains("This database already has forums or posts"),
        "{body}"
    );
    assert!(
        body.contains("name=\"sample_content\" value=\"1\" checked"),
        "the choice is kept on the form: {body}"
    );
    assert!(identity.needs_admin().await.unwrap(), "nothing was created");
    fields.retain(|(key, _)| *key != "sample_content");
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    assert_eq!(
        count(&services, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        24,
        "the existing content is untouched"
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn administrator_stage_writes_the_sample_in_the_chosen_language() {
    let services = TestFixture::migrated().await.unwrap();
    let root = Root::new();
    let identity = Identity::new(
        services.db.clone(),
        services.config.identity.clone(),
        root.0.clone(),
        detached_global(),
    );
    let app = Wizard::administrator(
        root.0.clone(),
        root.config(),
        services.config.clone(),
        identity.clone(),
    )
    .app()
    .unwrap();
    let mut fields = vec![
        ("token", app.token.clone()),
        ("username", "admin".to_owned()),
        ("password", PASSWORD.to_owned()),
        ("password_confirm", PASSWORD.to_owned()),
        ("sample_content", "1".to_owned()),
        ("sample_language", "xx".to_owned()),
    ];
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(
        body.contains(Lang::EN.setup_error_sample_language()),
        "a language without a sample is refused: {body}"
    );
    assert!(identity.needs_admin().await.unwrap(), "nothing was created");

    let chosen = gono_forum_web::content::sample::languages()
        .find(|lang| *lang != Lang::EN)
        .expect("a language other than the base has a sample");
    fields.last_mut().unwrap().1 = chosen.tag().to_owned();
    let (_, _, body) = post(&app.router, &fields).await;
    assert!(body.contains("Installation finished"), "{body}");
    let subjects: Vec<String> =
        gono_forum_web::db::query_scalar("SELECT subject FROM gonof_forum_thread")
            .fetch_all(services.db.executor())
            .await
            .unwrap();
    assert_eq!(subjects.len(), 100);
    assert!(
        subjects.iter().all(|subject| !subject.is_ascii()),
        "the threads are in {chosen}, not in the plain-ASCII base: {subjects:?}"
    );
    services.cleanup().await.unwrap();
}
