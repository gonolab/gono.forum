//! Skins (docs/skins.md) through the real application: the three sources and their priority, the stylesheet
//! endpoint, importing with confirmed warnings, choosing for the site and for a domain, deleting, falling back to the
//! built-in default when a choice cannot be used, the administrator's preview and what it keeps out of shared caches,
//! the member's personal style in its cascade layer, and who may do what.
//!
//! The imported and instance skins are the Ledger test fixture (tests/fixtures/skins/ledger), which passes the safety
//! rules and breaks several style rules, so it imports with warnings.
#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    db::{self, Database},
    identity, project_root, router, skins,
    test_support::{self, TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use std::path::PathBuf;
use tower::ServiceExt;

const OTHER: &str = "http://other.test:58833";

struct Site {
    services: TestFixture,
    app: Router,
    root: PathBuf,
    admin: (String, String),
    moderator: (String, String),
    member: (String, String),
}

struct Answer {
    status: StatusCode,
    html: String,
    location: String,
    cache: String,
    cookies: Vec<String>,
}

fn ledger_dir() -> PathBuf {
    project_root().join("tests/fixtures/skins/ledger")
}

fn ledger_pack() -> String {
    let (draft, _) = skins::read_dir(&ledger_dir()).expect("the fixture reads");
    skins::pack(&draft)
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn ledger_column(db: &Database, column: &str) -> String {
    db::query_scalar(&format!(
        "SELECT {column} FROM gonof_common_skin WHERE skin_id='ledger'"
    ))
    .fetch_one(db.executor())
    .await
    .unwrap()
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

impl Site {
    /// A site with an administrator (uid 1), a moderator (uid 2) and a member (uid 3). `instance` lists skin
    /// directories to put under `$GONOF_HOME/skins/` before the application reads them.
    async fn new(instance: &[(&str, &std::path::Path)]) -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        for (id, from) in instance {
            copy_dir(from, &root.join("skins").join(id));
        }
        for (uid, role) in [(1, 1), (2, 3), (3, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'password-hash',?,1,?,1)")
                .bind(uid)
                .bind(format!("skinner-{uid}"))
                .bind(format!("skinner-{uid}@example.test"))
                .bind(role)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let identity = identity::Identity::new(
            db.clone(),
            services.config.identity.clone(),
            root.clone(),
            test_support::detached_global(),
        );
        let mut grants = Vec::new();
        for uid in [1, 2, 3] {
            let grant = http_auth::login(&identity, &db, uid).await;
            grants.push((grant.session_token, grant.csrf_token));
        }
        let app = router::build_with_global(
            services.config.clone(),
            &root,
            db,
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            root,
            member: grants.pop().unwrap(),
            moderator: grants.pop().unwrap(),
            admin: grants.pop().unwrap(),
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn send(&self, origin: &str, request: Request<Body>) -> Answer {
        let _ = origin;
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let header_text = |name| {
            response
                .headers()
                .get(name)
                .map(|value: &header::HeaderValue| value.to_str().unwrap().to_owned())
                .unwrap_or_default()
        };
        let location = header_text(header::LOCATION);
        let cache = header_text(header::CACHE_CONTROL);
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect();
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        Answer {
            status,
            html,
            location,
            cache,
            cookies,
        }
    }

    fn base(
        origin: &str,
        method: &str,
        path: &str,
        cookies: &[String],
    ) -> axum::http::request::Builder {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, test_support::host_of(origin));
        if !cookies.is_empty() {
            request = request.header(header::COOKIE, cookies.join("; "));
        }
        if method == "POST" {
            request = request.header(header::ORIGIN, origin);
        }
        request
    }

    async fn get_on(&self, origin: &str, path: &str, cookies: &[String]) -> Answer {
        let request = Self::base(origin, "GET", path, cookies)
            .body(Body::empty())
            .unwrap();
        self.send(origin, request).await
    }

    async fn get(&self, path: &str, session: Option<&(String, String)>) -> Answer {
        let cookies: Vec<String> = session
            .map(|(token, _)| vec![format!("gonof_session={token}")])
            .unwrap_or_default();
        self.get_on(TEST_SITE_URL, path, &cookies).await
    }

    async fn post_with(
        &self,
        path: &str,
        session: &(String, String),
        extra_cookies: &[String],
        fields: &[(&str, &str)],
    ) -> Answer {
        let mut cookies = vec![format!("gonof_session={}", session.0)];
        cookies.extend(extra_cookies.iter().cloned());
        let mut all: Vec<(&str, &str)> = vec![("_csrf", session.1.as_str())];
        all.extend_from_slice(fields);
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(all)
            .finish();
        let request = Self::base(TEST_SITE_URL, "POST", path, &cookies)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.send(TEST_SITE_URL, request).await
    }

    async fn post(
        &self,
        path: &str,
        session: &(String, String),
        fields: &[(&str, &str)],
    ) -> Answer {
        self.post_with(path, session, &[], fields).await
    }

    /// Uploads a pack the way the import form does: the token first, then the file.
    async fn upload(&self, session: &(String, String), pack: &str) -> Answer {
        let boundary = "gonof-skin-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"pack\"; filename=\"ledger.skin.json\"\r\nContent-Type: application/json\r\n\r\n{pack}\r\n--{boundary}--\r\n",
            session.1
        );
        let request = Self::base(
            TEST_SITE_URL,
            "POST",
            "/manage/skins/import",
            &[format!("gonof_session={}", session.0)],
        )
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
        self.send(TEST_SITE_URL, request).await
    }

    /// Imports the Ledger fixture through the upload and the confirmation of its warnings.
    async fn import_ledger(&self) {
        let answer = self.upload(&self.admin, &ledger_pack()).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.html);
        assert!(answer.html.contains("data-skin-confirm"), "{}", answer.html);
        let replaces = hidden(&answer.html, "replaces");
        let digest = hidden(&answer.html, "digest");
        let pack = hidden(&answer.html, "pack");
        let done = self
            .post(
                "/manage/skins/import/confirm",
                &self.admin,
                &[
                    ("replaces", &replaces),
                    ("digest", &digest),
                    ("pack", &pack),
                ],
            )
            .await;
        assert_eq!(
            done.status,
            StatusCode::SEE_OTHER,
            "{}",
            done.html
                .find("data-skin-refused")
                .map_or("", |at| &done.html[at..(at + 600).min(done.html.len())])
        );
        assert_eq!(done.location, "/manage/skins?saved=imported");
    }
}

/// Undoes HTML escaping: the named entities and every numeric one (Askama writes `&#34;`, `&#62;` and the like).
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after.find(';').unwrap();
        let entity = &after[..end];
        let decoded = match entity {
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            _ => entity
                .strip_prefix('#')
                .and_then(|code| code.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        out.push(decoded.unwrap_or_else(|| panic!("entity &{entity};")));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The value of a hidden input, unescaped.
fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("{name} in the page"))
        + marker.len();
    let end = start + html[start..].find('"').unwrap();
    unescape(&html[start..end])
}

/// The `<html>` start tag.
fn html_tag(html: &str) -> &str {
    let start = html.find("<html").unwrap();
    &html[start..start + html[start..].find('>').unwrap()]
}

/// The skin stylesheet link of a page.
fn skin_href(html: &str) -> String {
    let marker = "href=\"";
    let at = html
        .find("data-skin-style")
        .expect("the skin stylesheet link");
    let start = html[..at].rfind(marker).unwrap() + marker.len();
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

/// One row of the skins table.
fn row<'a>(html: &'a str, id: &str, source: &str) -> &'a str {
    let mut rest = html;
    loop {
        let start = rest
            .find(&format!("data-skin-row=\"{id}\""))
            .unwrap_or_else(|| panic!("{id} from {source} is listed"));
        let end = start + rest[start..].find("</tr>").unwrap();
        if rest[start..end].contains(&format!("<td>{source}</td>")) {
            return &rest[start..end];
        }
        rest = &rest[end..];
    }
}

#[tokio::test]
async fn the_default_skin_serves_its_palette_and_guards_without_a_session() {
    let site = Site::new(&[]).await;
    let page = site.get("/", None).await;
    assert_eq!(page.status, StatusCode::OK);
    let tag = html_tag(&page.html);
    for attribute in [
        "data-skin=\"default\"",
        "data-skin-source=\"builtin\"",
        "data-palette=\"default\"",
        "data-nav=\"top\"",
        "data-header-boards=\"true\"",
    ] {
        assert!(tag.contains(attribute), "{tag}");
    }
    assert!(!tag.contains("data-avatar-shape"), "{tag}");
    // The skin comes after forum.css and before the personal style; the footer line is not translated.
    let href = skin_href(&page.html);
    assert_eq!(href, skins::DEFAULT_SKIN.href());
    assert!(page.html.find("/static/css/forum.css").unwrap() < page.html.find(&href).unwrap());
    assert!(
        page.html
            .contains("<p class=\"colophon__legal\" translate=\"no\">Built on")
    );

    let css = site.get(&href, None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.cache, "public, max-age=31536000, immutable");
    assert!(css.cookies.is_empty(), "no session work for a stylesheet");
    assert!(css.html.contains("@layer guards {"));
    assert!(css.html.contains(
        ":root[data-palette=\"default\"], :root:not([data-palette]) {\n  color-scheme: light;"
    ));
    assert!(css.html.contains("--input-border: #87909e;"));
    let stale = site.get("/skin/default.css?v=0", None).await;
    assert_eq!(stale.cache, "no-cache");
    assert_eq!(stale.html, css.html);
    assert_eq!(
        site.get("/skin/ledger.css", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        site.get("/skin/default", None).await.status,
        StatusCode::NOT_FOUND
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn sources_are_listed_by_priority_with_reasons_and_warnings() {
    let broken = std::env::temp_dir().join(format!("gonof-broken-skin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&broken);
    copy_dir(&ledger_dir(), &broken);
    std::fs::write(
        broken.join("skin.toml"),
        std::fs::read_to_string(ledger_dir().join("skin.toml"))
            .unwrap()
            .replace("\"ledger\"", "\"broken\""),
    )
    .unwrap();
    std::fs::write(
        broken.join("skin.css"),
        ".pane { background: url(https://evil.example/x.png); }\n",
    )
    .unwrap();
    let default_copy =
        std::env::temp_dir().join(format!("gonof-default-skin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&default_copy);
    copy_dir(
        &project_root().join("apps/web/skins/default"),
        &default_copy,
    );
    let site = Site::new(&[
        ("ledger", &ledger_dir()),
        ("broken", &broken),
        ("default", &default_copy),
    ])
    .await;
    let page = site.get("/manage/skins", Some(&site.admin)).await;
    assert_eq!(page.status, StatusCode::OK);
    let t = gono_forum_web::i18n::current();
    // The instance Ledger is usable, with its warnings listed.
    let ledger = row(&page.html, "ledger", t.skin_source_instance());
    assert!(ledger.contains("data-skin-usable=\"true\""), "{ledger}");
    assert!(ledger.contains("data-skin-warnings"), "{ledger}");
    assert!(
        ledger.contains("--masthead-bg is not a color token of the core"),
        "{ledger}"
    );
    // The broken one says why it cannot be used.
    let broken_row = row(&page.html, "broken", t.skin_source_instance());
    assert!(broken_row.contains("data-skin-usable=\"false\""));
    assert!(broken_row.contains(t.skin_state_invalid()));
    assert!(broken_row.contains("url() is not allowed"), "{broken_row}");
    // An instance skin named like a built-in one is shadowed, and says by which source.
    let shadowed = row(&page.html, "default", t.skin_source_instance());
    assert!(
        unescape(shadowed).contains(&t.skin_state_shadowed(t.skin_source_builtin())),
        "{shadowed}"
    );
    let builtin = row(&page.html, "default", t.skin_source_builtin());
    assert!(builtin.contains(t.skin_state_current()));

    // Importing Ledger shadows the instance copy.
    site.import_ledger().await;
    let page = site.get("/manage/skins", Some(&site.admin)).await;
    let instance = row(&page.html, "ledger", t.skin_source_instance());
    assert!(
        unescape(instance).contains(&t.skin_state_shadowed(t.skin_source_imported())),
        "{instance}"
    );
    assert!(
        row(&page.html, "ledger", t.skin_source_imported()).contains("data-skin-usable=\"true\"")
    );

    // A directory added later is only read after a rescan.
    copy_dir(&ledger_dir(), &site.root.join("skins").join("later"));
    std::fs::write(
        site.root.join("skins/later/skin.toml"),
        std::fs::read_to_string(ledger_dir().join("skin.toml"))
            .unwrap()
            .replace("\"ledger\"", "\"later\""),
    )
    .unwrap();
    assert!(
        !site
            .get("/manage/skins", Some(&site.admin))
            .await
            .html
            .contains("data-skin-row=\"later\"")
    );
    let rescan = site.post("/manage/skins/rescan", &site.admin, &[]).await;
    assert_eq!(rescan.location, "/manage/skins?saved=rescanned");
    assert!(
        site.get("/manage/skins", Some(&site.admin))
            .await
            .html
            .contains("data-skin-row=\"later\"")
    );
    std::fs::remove_dir_all(&broken).unwrap();
    std::fs::remove_dir_all(&default_copy).unwrap();
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn importing_lists_warnings_refuses_errors_and_records_what_was_confirmed() {
    let site = Site::new(&[]).await;
    // A pack that breaks a safety rule is refused with every problem listed, and nothing is stored.
    let bad = ledger_pack().replace(
        ".pane {",
        ".pane { background: url(x.png); z-index: 99; }\\n.x {",
    );
    let refused = site.upload(&site.admin, &bad).await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.html
    );
    assert!(refused.html.contains("data-skin-refused"));
    assert!(refused.html.contains("url() is not allowed") && refused.html.contains("z-index"));
    assert_eq!(
        scalar(site.db(), "SELECT COUNT(*) FROM gonof_common_skin").await,
        0
    );
    let not_a_pack = site.upload(&site.admin, "{}").await;
    assert_eq!(not_a_pack.status, StatusCode::UNPROCESSABLE_ENTITY);

    site.import_ledger().await;
    let version = ledger_column(site.db(), "skin_version").await;
    let warnings = ledger_column(site.db(), "warnings").await;
    let hash = ledger_column(site.db(), "content_hash").await;
    assert_eq!(version, "1.0.0");
    let (draft, _) = skins::read_dir(&ledger_dir()).unwrap();
    assert_eq!(hash, skins::content_hash(&draft));
    let confirmed: Vec<gono_forum_web::css_check::Finding> =
        serde_json::from_str(&warnings).unwrap();
    assert!(confirmed.len() > 5);
    assert!(
        confirmed
            .iter()
            .all(|f| f.severity == gono_forum_web::css_check::Severity::Warning)
    );
    // Stored as codes, worded when shown.
    assert!(warnings.contains("\"code\":\"palette-unknown\""));
    assert_eq!(
        scalar(
            site.db(),
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='skin.import' AND uid=1"
        )
        .await,
        1
    );

    // Importing the same id again asks first, and a confirmation for a row that changed meanwhile is refused.
    let again = site.upload(&site.admin, &ledger_pack()).await;
    assert!(
        again
            .html
            .contains(&gono_forum_web::i18n::current().skin_import_replaces("1.0.0"))
    );
    let stale = site
        .post(
            "/manage/skins/import/confirm",
            &site.admin,
            &[
                ("replaces", "0"),
                ("digest", &hidden(&again.html, "digest")),
                ("pack", &hidden(&again.html, "pack")),
            ],
        )
        .await;
    assert_eq!(stale.status, StatusCode::OK, "it asks again");
    assert!(stale.html.contains("data-skin-confirm"));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn choosing_a_skin_changes_every_page_and_a_domain_can_choose_its_own() {
    let site = Site::new(&[]).await;
    site.import_ledger().await;
    let chosen = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "ledger:main")],
        )
        .await;
    assert_eq!(
        chosen.location, "/manage/skins?saved=selected",
        "{}",
        chosen.html
    );
    let body: String = db::query_scalar(
        "SELECT svalue FROM gonof_common_settings WHERE skey='settings.layout.body'",
    )
    .fetch_one(site.db().executor())
    .await
    .unwrap();
    assert_eq!(
        body,
        r#"{"v":1,"skin":"ledger","palette":"main","based_on":{"skin":"ledger","version":"1.0.0"},"places":null}"#
    );
    assert_eq!(
        scalar(
            site.db(),
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='skin.select'"
        )
        .await,
        1
    );
    let page = site.get("/", None).await;
    let tag = html_tag(&page.html);
    assert!(
        tag.contains("data-skin=\"ledger\" data-skin-source=\"imported\" data-palette=\"main\""),
        "{tag}"
    );
    // The skin fixes round avatars for everyone.
    assert!(tag.contains("data-avatar-shape=\"circle\""), "{tag}");
    let css = site.get(&skin_href(&page.html), None).await;
    assert_eq!(css.cache, "public, max-age=31536000, immutable");
    assert!(css.html.contains("--masthead-h: 52px;"));
    assert!(css.html.contains(".pane {"));
    // The member's settings say the skin fixes the shape.
    let reading = site.get("/my/reading", Some(&site.member)).await;
    assert!(
        reading.html.contains("data-skin-avatar"),
        "{}",
        reading.html
    );
    // /manage/skins keeps the built-in default, so a skin that breaks the page can be switched back there.
    let manage = site.get("/manage/skins", Some(&site.admin)).await;
    assert!(html_tag(&manage.html).contains("data-skin=\"default\""));
    // An unknown choice is refused.
    let unknown = site
        .post(
            "/manage/skins/select",
            &site.admin,
            &[("choice", "nothing:main")],
        )
        .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);

    // A second domain overrides the skin; the main domain keeps the site's.
    for sql in [
        format!(
            "INSERT INTO gonof_common_site_domain(site_url,dateline) VALUES('{TEST_SITE_URL}',1)"
        ),
        format!(
            "INSERT INTO gonof_common_site_domain(site_url,site_name,dateline) VALUES('{OTHER}','Other',1)"
        ),
    ] {
        execute(site.db(), &sql).await;
    }
    test_support::set_setting(site.db(), "settings.domains.enabled", "true")
        .await
        .unwrap();
    site.services.global.clear_local();
    let other: i64 = scalar(
        site.db(),
        &format!("SELECT domain_id FROM gonof_common_site_domain WHERE site_url='{OTHER}'"),
    )
    .await;
    let form = site
        .get(&format!("/manage/domains/{other}"), Some(&site.admin))
        .await;
    assert!(form.html.contains("data-domain-skin"), "{}", form.html);
    let saved = site
        .post(
            &format!("/manage/domains/{other}/skin"),
            &site.admin,
            &[("choice", "default:default")],
        )
        .await;
    assert_eq!(
        saved.location,
        format!("/manage/domains/{other}?skin=saved"),
        "{}",
        saved.html
    );
    let there = site.get_on(OTHER, "/", &[]).await;
    assert!(
        html_tag(&there.html).contains("data-skin=\"default\""),
        "{}",
        html_tag(&there.html)
    );
    assert!(html_tag(&site.get("/", None).await.html).contains("data-skin=\"ledger\""));
    // Following the site again removes the override.
    site.post(
        &format!("/manage/domains/{other}/skin"),
        &site.admin,
        &[("choice", "")],
    )
    .await;
    assert!(html_tag(&site.get_on(OTHER, "/", &[]).await.html).contains("data-skin=\"ledger\""));
    // Deleting the domain deletes its document.
    site.post(
        &format!("/manage/domains/{other}/skin"),
        &site.admin,
        &[("choice", "default:default")],
    )
    .await;
    let key = format!("settings.domain_layout.{other}.body");
    assert_eq!(
        scalar(
            site.db(),
            &format!("SELECT COUNT(*) FROM gonof_common_settings WHERE skey='{key}'")
        )
        .await,
        1
    );
    let version = scalar(
        site.db(),
        &format!("SELECT version FROM gonof_common_site_domain WHERE domain_id={other}"),
    )
    .await;
    let deleted = site
        .post(
            &format!("/manage/domains/{other}/delete"),
            &site.admin,
            &[("version", &version.to_string())],
        )
        .await;
    assert_eq!(deleted.status, StatusCode::SEE_OTHER, "{}", deleted.html);
    assert_eq!(
        scalar(
            site.db(),
            &format!("SELECT COUNT(*) FROM gonof_common_settings WHERE skey='{key}'")
        )
        .await,
        0
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_skin_in_use_cannot_be_deleted_and_a_broken_choice_falls_back_to_the_default() {
    let site = Site::new(&[]).await;
    site.import_ledger().await;
    site.post(
        "/manage/skins/select",
        &site.admin,
        &[("choice", "ledger:main")],
    )
    .await;
    let version = scalar(
        site.db(),
        "SELECT version FROM gonof_common_skin WHERE skin_id='ledger'",
    )
    .await
    .to_string();
    let refused = site
        .post(
            "/manage/skins/ledger/delete",
            &site.admin,
            &[("version", &version)],
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .html
            .contains(gono_forum_web::i18n::current().skin_error_in_use())
    );

    // A row edited by hand so that it breaks a safety rule is no longer usable: pages fall back to the default.
    execute(site.db(), "UPDATE gonof_common_skin SET css='.pane { background: url(x.png); }' WHERE skin_id='ledger'").await;
    site.services.global.clear_local();
    let page = site.get("/", None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(html_tag(&page.html).contains("data-skin=\"default\""));
    // So does a choice naming a skin that does not exist.
    execute(site.db(), r#"UPDATE gonof_common_settings SET svalue='{"v":1,"skin":"gone","palette":"main","based_on":null,"places":null}' WHERE skey='settings.layout.body'"#).await;
    site.services.global.clear_local();
    assert!(html_tag(&site.get("/", None).await.html).contains("data-skin=\"default\""));
    // And a document that does not parse.
    execute(
        site.db(),
        "UPDATE gonof_common_settings SET svalue='{broken' WHERE skey='settings.layout.body'",
    )
    .await;
    site.services.global.clear_local();
    assert_eq!(site.get("/", None).await.status, StatusCode::OK);

    // Not in use any more: it can be deleted.
    site.post(
        "/manage/skins/select",
        &site.admin,
        &[("choice", "default:default")],
    )
    .await;
    let deleted = site
        .post(
            "/manage/skins/ledger/delete",
            &site.admin,
            &[("version", &version)],
        )
        .await;
    assert_eq!(
        deleted.location, "/manage/skins?saved=deleted",
        "{}",
        deleted.html
    );
    assert_eq!(
        scalar(site.db(), "SELECT COUNT(*) FROM gonof_common_skin").await,
        0
    );
    assert_eq!(
        scalar(
            site.db(),
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='skin.delete'"
        )
        .await,
        1
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_preview_is_only_seen_by_the_administrator_who_started_it() {
    let site = Site::new(&[("ledger", &ledger_dir())]).await;
    let started = site
        .post(
            "/manage/skins/preview",
            &site.admin,
            &[("choice", "ledger:")],
        )
        .await;
    assert_eq!(started.status, StatusCode::SEE_OTHER, "{}", started.html);
    assert_eq!(started.location, "/");
    let cookie = started
        .cookies
        .iter()
        .find(|cookie| cookie.starts_with("gonof_skin_preview="))
        .expect("the preview cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let admin_cookies = vec![format!("gonof_session={}", site.admin.0), cookie.clone()];
    let previewed = site.get_on(TEST_SITE_URL, "/", &admin_cookies).await;
    assert!(
        html_tag(&previewed.html).contains("data-skin=\"ledger\" data-skin-source=\"instance\"")
    );
    assert!(previewed.html.contains("data-skin-preview"));
    // Pages of a signed-in member are never stored by shared caches, and a skin no page uses is private.
    assert_eq!(previewed.cache, "private, no-store");
    let css = site.get(&skin_href(&previewed.html), None).await;
    assert_eq!(css.cache, "private, max-age=31536000, immutable");
    // Nobody else sees it: visitors, other members with the same cookie, the site's own settings.
    assert!(html_tag(&site.get("/", None).await.html).contains("data-skin=\"default\""));
    let member = site
        .get_on(
            TEST_SITE_URL,
            "/",
            &[format!("gonof_session={}", site.member.0), cookie.clone()],
        )
        .await;
    assert!(html_tag(&member.html).contains("data-skin=\"default\""));
    assert!(!member.html.contains("data-skin-preview"));
    assert!(
        html_tag(&site.get("/", Some(&site.admin)).await.html).contains("data-skin=\"default\"")
    );
    assert_eq!(
        scalar(
            site.db(),
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='skin.select'"
        )
        .await,
        0
    );

    // Exit clears the cookie; publish makes the previewed skin the site's.
    let exit = site
        .post_with(
            "/manage/skins/preview/exit",
            &site.admin,
            &[cookie.clone()],
            &[],
        )
        .await;
    assert!(
        exit.cookies
            .iter()
            .any(|c| c.starts_with("gonof_skin_preview=;") && c.contains("Max-Age=0"))
    );
    let published = site
        .post_with("/manage/skins/preview/publish", &site.admin, &[cookie], &[])
        .await;
    assert_eq!(
        published.location, "/manage/skins?saved=selected",
        "{}",
        published.html
    );
    assert!(html_tag(&site.get("/", None).await.html).contains("data-skin=\"ledger\""));
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn only_administrators_manage_skins() {
    let site = Site::new(&[]).await;
    let guest = site.get("/manage/skins", None).await;
    assert_eq!(guest.status, StatusCode::SEE_OTHER);
    for session in [&site.moderator, &site.member] {
        assert_eq!(
            site.get("/manage/skins", Some(session)).await.status,
            StatusCode::FORBIDDEN
        );
        for (path, fields) in [
            ("/manage/skins/select", vec![("choice", "default:default")]),
            ("/manage/skins/rescan", vec![]),
            ("/manage/skins/preview", vec![("choice", "default:")]),
            ("/manage/skins/default/delete", vec![("version", "1")]),
        ] {
            assert_eq!(
                site.post(path, session, &fields).await.status,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
    }
    let upload = site.upload(&site.member, &ledger_pack()).await;
    assert_eq!(upload.status, StatusCode::FORBIDDEN);
    assert_eq!(
        scalar(site.db(), "SELECT COUNT(*) FROM gonof_common_skin").await,
        0
    );
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn personal_styles_sit_in_the_member_layer_after_the_skin() {
    let site = Site::new(&[]).await;
    let css = ".row-title { color: #8b5cf6 !important; }\n@media (max-width: 760px) { .pane { border: 0; } }";
    let saved = site
        .post("/my/style", &site.member, &[("css", css), ("version", "0")])
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.html);
    let page = site.get("/", Some(&site.member)).await;
    let skin = page.html.find("data-skin-style").unwrap();
    let personal = page
        .html
        .find("data-custom-style")
        .expect("the personal style link");
    assert!(skin < personal, "the personal style comes after the skin");
    let start = page.html[..personal].rfind("href=\"").unwrap() + 6;
    let href =
        page.html[start..start + page.html[start..].find('"').unwrap()].replace("&amp;", "&");
    let served = site.get(&href, Some(&site.member)).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.html, format!("@layer member {{\n{css}\n}}\n"));
    site.services.cleanup().await.unwrap();
}
