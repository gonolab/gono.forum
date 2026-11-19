//! Native and HTMX contracts around the shared content transaction service.
use std::{collections::BTreeMap, path::PathBuf};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content::{
        self,
        writes::{ContentWriter, NewReply},
    },
    db::Database,
    identity::{self, Identity},
    router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const HX: &[(&str, &str)] = &[("HX-Request", "true")];
/// An HTMX request from the thread page's reply box, which sends this header (`topic/detail.html`).
const HX_THREAD: &[(&str, &str)] = &[("HX-Request", "true"), ("X-Composer-Context", "thread")];

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    root: PathBuf,
    db: Database,
    app: Router,
    identity: Identity,
    admin: Cookies,
    fid: String,
    slug: String,
}

impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let grant = identity.login("admin", &admin_password).await.unwrap();
        let admin = Cookies(BTreeMap::from([(
            "gonof_session__port58833".into(),
            grant.session_token,
        )]));
        let forum = gono_forum_web::db::query("SELECT fid,slug FROM gonof_forum_forum WHERE visibility='public' AND status=1 ORDER BY fid LIMIT 1")
            .fetch_one(db.executor()).await.unwrap();
        let fid: i64 = forum.try_get("fid").unwrap();
        let slug: String = forum.try_get("slug").unwrap();
        let app = router::build(config, &root, db.clone());
        Self {
            _services: services,
            root,
            db,
            app,
            identity,
            admin,
            fid: fid.to_string(),
            slug,
        }
    }

    async fn scalar(&self, sql: &str) -> i64 {
        gono_forum_web::db::query_scalar(sql)
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
    async fn execute(&self, sql: &str) {
        gono_forum_web::db::query(sql)
            .execute(&self.db.pool)
            .await
            .unwrap();
    }
    async fn topic(&self, cookies: &mut Cookies) -> i64 {
        let response = post(
            self,
            "/new",
            cookies,
            &[
                ("fid", &self.fid),
                ("title", "HTTP thread"),
                ("body", "First post **content**"),
                ("request_key", &identity::random_token()),
            ],
            &[],
        )
        .await;
        response.status(StatusCode::SEE_OTHER);
        self.scalar("SELECT MAX(tid) FROM gonof_forum_thread").await
    }
    fn writer(&self) -> ContentWriter {
        ContentWriter::new(self.db.clone(), Default::default())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Default)]
struct Cookies(BTreeMap<String, String>);
impl Cookies {
    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
    fn update(&mut self, headers: &HeaderMap) {
        for value in headers.get_all(header::SET_COOKIE) {
            let (key, value) = value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap();
            if value.is_empty() {
                self.0.remove(key);
            } else {
                self.0.insert(key.into(), value.into());
            }
        }
    }
    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self
            .0
            .get("gonof_session__port58833")
            .or_else(|| self.0.get("__Host-gonof_session__port58833"))
        {
            identity
                .session(raw)
                .await
                .unwrap()
                .map(|session| session.csrf_token)
                .unwrap_or_default()
        } else {
            self.0
                .get("gonof_csrf__port58833")
                .or_else(|| self.0.get("__Host-gonof_csrf__port58833"))
                .cloned()
                .unwrap_or_default()
        }
    }
}
struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}
impl Response {
    fn status(&self, expected: StatusCode) {
        assert_eq!(self.status, expected, "{}", self.html);
    }
    fn redirect(&self, expected: &str) {
        self.status(StatusCode::SEE_OTHER);
        assert_eq!(self.headers[header::LOCATION], expected);
    }
    fn retained_error(&self, expected: StatusCode, body: &str) {
        self.status(expected);
        assert!(self.html.contains(body));
        assert_eq!(self.headers["HX-Retarget"], "body");
        assert_eq!(self.headers["HX-Reswap"], "innerHTML");
        assert_eq!(self.headers["X-Composer-Fragment"], "true");
    }
}
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if !cookies.0.is_empty() {
        request = request.header(header::COOKIE, cookies.header());
    }
    if method == "POST" {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if !extra
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("origin"))
        {
            request = request
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
        }
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let response = f
        .app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    cookies.update(&headers);
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
    Response {
        status,
        headers,
        html,
    }
}
async fn get(f: &Fixture, path: &str, cookies: &mut Cookies) -> Response {
    request(f, "GET", path, cookies, &[], &[]).await
}
async fn post(
    f: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Response {
    let csrf = cookies.csrf(&f.identity).await;
    let mut fields = fields.to_vec();
    fields.push(("_csrf", &csrf));
    request(f, "POST", path, cookies, &fields, extra).await
}

#[tokio::test]
async fn editing_replays_success_and_keeps_stale_native_and_htmx_input() {
    let f = Fixture::new().await;
    let mut cookies = f.admin.clone();
    let tid = f.topic(&mut cookies).await;
    let path = format!("/edit/thread-{tid}");
    let form = get(&f, &path, &mut cookies).await;
    form.status(StatusCode::OK);
    assert!(
        form.html
            .contains("name=\"expected_post_version\" value=\"1\"")
    );
    let key = identity::random_token();
    let fields = [
        ("title", "Updated title"),
        ("body", "Updated **body**"),
        ("request_key", key.as_str()),
        ("expected_post_version", "1"),
        ("expected_thread_version", "1"),
        ("reason", "HTTP edit regression"),
    ];
    post(&f, &path, &mut cookies, &fields, &[])
        .await
        .redirect(&format!("/thread-{tid}#floor-1"));
    post(&f, &path, &mut cookies, &fields, &[])
        .await
        .status(StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_post_revision")
            .await,
        1
    );
    let stale_key = identity::random_token();
    let stale = [
        ("title", "Stale title"),
        ("body", "Conflicting input stays"),
        ("request_key", stale_key.as_str()),
        ("expected_post_version", "1"),
        ("expected_thread_version", "1"),
        ("reason", "Old form"),
    ];
    let response = post(&f, &path, &mut cookies, &stale, HX).await;
    response.retained_error(StatusCode::CONFLICT, "Conflicting input stays");
    assert!(response.html.contains("Updated **body**"));
    let response = post(&f, &path, &mut cookies, &stale, &[]).await;
    response.status(StatusCode::CONFLICT);
    assert!(response.html.contains("Conflicting input stays"));
    let mut guest = Cookies::default();
    let public = get(&f, &format!("/thread-{tid}"), &mut guest).await;
    assert!(public.html.contains("Updated title"));
    assert!(!public.html.contains("HTTP thread"));
    assert!(!public.html.contains("Conflicting input stays"));
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn forms_require_csrf_verified_unfrozen_members_and_stable_request_keys() {
    let f = Fixture::new().await;
    let mut guest = Cookies::default();
    get(&f, "/new", &mut guest)
        .await
        .redirect("/login?return_to=%2Fnew");
    let mut admin = f.admin.clone();
    let form = get(&f, &format!("/new/{}", f.slug), &mut admin).await;
    form.status(StatusCode::OK);
    assert!(form.html.contains("name=\"body\""));
    assert!(!form.html.contains("Activity or news settings"));
    assert!(
        form.html.find("name=\"body\"").unwrap() < form.html.find("name=\"category\"").unwrap(),
        "the thread category comes after the body"
    );
    for marker in [
        "<label class=\"field\" data-categories=\"1 2 3\"><span class=\"field__label\">Total budget",
        "<label class=\"field\" data-categories=\"2 3\"><span class=\"field__label\">Red packet shares",
        "<fieldset class=\"option-group\" data-categories=\"2\">",
        "data-categories=\"1 2 3 4\" data-activity-deadline",
        "name=\"deadline_preset\" value=\"7\"",
        "data-categories=\"4\" data-poll",
        "name=\"poll_mode\" value=\"multiple\"",
        "name=\"options\" rows=\"4\"",
        "Options (one per line, 2 to 20)",
        "<div class=\"field\" data-categories=\"5\">\n      <label class=\"field__label\" for=\"editor-source_name\">Source name",
        "<label class=\"field__label\" for=\"editor-source_url\">Source link",
        "<label class=\"field__label\" for=\"editor-source_author\">Original author",
    ] {
        assert!(
            form.html.contains(marker),
            "missing activity field category marker: {marker}"
        );
    }
    let deadline = form
        .html
        .split("name=\"deadline\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let deadline_ts = chrono::NaiveDateTime::parse_from_str(deadline, "%Y-%m-%dT%H:%M")
        .unwrap()
        .and_utc()
        .timestamp()
        - 8 * 3600;
    let expected = identity::now() + 3 * 86_400;
    assert!(
        (deadline_ts - expected).abs() < 120,
        "the default deadline is three days from now in the site time zone (UTC+8), got {deadline}"
    );
    // The editor fills the default from the site time zone on the input instead of hard-coding UTC+8.
    assert!(
        form.html
            .contains("Activity deadline (site time zone UTC+8)")
    );
    assert!(form.html.contains("data-offset-minutes=\"480\""));
    assert!(form.html.contains(&format!("value=\"{}\" selected", f.fid)));
    let marker = "name=\"request_key\" value=\"";
    let key = form
        .html
        .split(marker)
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));

    let fields = [
        ("fid", f.fid.as_str()),
        ("title", "Kept title"),
        ("body", "Kept body & <content>"),
    ];
    let missing = post(&f, "/new", &mut admin, &fields, HX).await;
    missing.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(missing.html.contains("Kept body"));
    assert!(missing.html.contains("name=\"request_key\" value=\"\""));
    assert!(missing.html.contains("name=\"deadline\" value=\"\""));
    assert_eq!(missing.headers["X-Composer-Fragment"], "true");
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await, 0);
    // Missing CSRF and foreign origins are rejected before writing, with input intact.
    let mut valid = fields.to_vec();
    valid.push(("request_key", key));
    let csrf_failure = request(&f, "POST", "/new", &mut admin, &valid, HX).await;
    csrf_failure.status(StatusCode::FORBIDDEN);
    assert!(csrf_failure.html.contains("Kept body"));
    assert!(!csrf_failure.html.contains("Kept body & <content>"));
    assert_eq!(csrf_failure.headers["X-Composer-Fragment"], "true");
    post(
        &f,
        "/new",
        &mut admin,
        &valid,
        &[
            ("Origin", "https://foreign.invalid"),
            ("HX-Request", "true"),
        ],
    )
    .await
    .retained_error(StatusCode::FORBIDDEN, "Kept title");
    post(&f, "/new", &mut guest, &valid, HX)
        .await
        .retained_error(StatusCode::UNAUTHORIZED, "Kept title");

    let first = post(&f, "/new", &mut admin, &valid, &[]).await;
    first.redirect("/thread-1#floor-1");
    post(&f, "/new", &mut admin, &valid, &[])
        .await
        .redirect("/thread-1#floor-1");
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_post").await, 1);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_post_request")
            .await,
        1
    );
    let mut changed = valid.clone();
    changed[1] = ("title", "Same key with another title");
    post(&f, "/new", &mut admin, &changed, HX)
        .await
        .retained_error(StatusCode::CONFLICT, "Same key with another title");
    let mut invalid_refs = valid.clone();
    invalid_refs[2] = ("body", "[Bad reference](attachment:-1)");
    post(&f, "/new", &mut admin, &invalid_refs, HX)
        .await
        .retained_error(StatusCode::UNPROCESSABLE_ENTITY, "Bad reference");

    f.execute("UPDATE gonof_common_member SET adminid=0,emailstatus=0,email='admin-test@example.test' WHERE uid=1")
        .await;
    post(&f, "/new", &mut admin, &valid, HX)
        .await
        .retained_error(StatusCode::FORBIDDEN, "Kept title");
    get(&f, "/new", &mut admin)
        .await
        .redirect("/verify-email?return_to=%2Fnew");
    f.execute("UPDATE gonof_common_member SET emailstatus=1 WHERE uid=1")
        .await;
    post(&f, "/new", &mut admin, &valid, &[])
        .await
        .redirect("/thread-1#floor-1");
    let quota_key = identity::random_token();
    let mut exhausted = valid.clone();
    exhausted[3] = ("request_key", &quota_key);
    post(&f, "/new", &mut admin, &exhausted, HX)
        .await
        .retained_error(StatusCode::TOO_MANY_REQUESTS, "Kept title");
    f.execute("UPDATE gonof_common_member SET adminid=1,freeze=1 WHERE uid=1")
        .await;
    post(&f, "/new", &mut admin, &valid, HX)
        .await
        .retained_error(StatusCode::FORBIDDEN, "Kept title");
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_post").await, 1);
    f._services.cleanup_ref().await.unwrap();
}

/// A member who cannot take part yet is sent to the step that meets this site's requirement: the email verification
/// page while an email counts and theirs waits, otherwise linking a phone number or a third-party account. A member
/// whose email is verified but who still has to meet another requirement finds the same actions as the banner on the
/// verification page, not the invite page.
#[tokio::test]
async fn the_posting_gate_leads_to_the_missing_requirement() {
    for (participation, email_verified, expected) in [
        ("mobile", true, "/my/account#mobile"),
        ("github", true, "/my/account#external"),
        ("email,mobile", false, "/verify-email?return_to=%2Fnew"),
    ] {
        let f = Fixture::new().await;
        gono_forum_web::test_support::set_setting(
            &f.db,
            "settings.login.participation",
            participation,
        )
        .await
        .unwrap();
        f.execute(&format!(
            "UPDATE gonof_common_member SET adminid=0,emailstatus={},email='member@example.test' WHERE uid=1",
            i64::from(email_verified)
        ))
        .await;
        let mut member = f.admin.clone();
        get(&f, "/new", &mut member).await.redirect(expected);
        if email_verified {
            let page = get(&f, "/verify-email", &mut member).await;
            page.status(StatusCode::OK);
            let panel = page
                .html
                .split_once("panel--auth")
                .and_then(|(_, rest)| rest.split_once("</section>"))
                .unwrap()
                .0;
            assert!(
                panel.contains(&format!("href=\"{expected}\"")),
                "{participation}: {panel}"
            );
            assert!(!panel.contains("/my/invites"), "{participation}: {panel}");
        }
    }
}

/// The mention picker normalizes what is typed the way usernames are stored, like the member directory: full-width
/// letters find the member too.
#[tokio::test]
async fn mention_search_matches_full_width_spellings() {
    let f = Fixture::new().await;
    f.execute("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(40,'camellia','x','camellia@example.test',1,0,1)")
        .await;
    let mut admin = f.admin.clone();
    // "camellia" in full-width Latin letters (U+FF43 and so on).
    let full_width: String = "camellia"
        .chars()
        .map(|c| char::from_u32(c as u32 - 'a' as u32 + 0xff41).unwrap())
        .collect();
    for typed in ["cam", "CAM", full_width.as_str()] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", typed)
            .finish();
        let found = get(&f, &format!("/api/mentions?{query}"), &mut admin).await;
        found.status(StatusCode::OK);
        assert!(
            found.html.contains("data-username=\"camellia\""),
            "{typed}: {}",
            found.html
        );
    }
}

#[tokio::test]
async fn activity_posts_require_the_submitted_deadline() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    let key = identity::random_token();
    let mut fields = vec![
        ("fid", f.fid.as_str()),
        ("title", "The deadline is not filled in on submit"),
        ("body", "Keep the activity settings as submitted"),
        ("request_key", key.as_str()),
        ("category", "4"),
        ("options", "Option one\nOption two"),
        ("max_choices", "1"),
        ("deadline", ""),
    ];
    for headers in [&[][..], HX] {
        let rejected = post(&f, "/new", &mut admin, &fields, headers).await;
        rejected.status(StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            rejected
                .html
                .contains("The activity deadline must be in the future")
        );
        assert!(rejected.html.contains("name=\"deadline\" value=\"\""));
        assert!(
            rejected
                .html
                .contains("Keep the activity settings as submitted")
        );
    }
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await, 0);

    let timestamp = (identity::now() + 5 * 86_400) / 60 * 60;
    let deadline = chrono::DateTime::from_timestamp(timestamp, 0)
        .unwrap()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    fields.last_mut().unwrap().1 = &deadline;
    let published = post(&f, "/new", &mut admin, &fields, &[]).await;
    published.status(StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar("SELECT deadline FROM gonof_forum_activity").await,
        timestamp
    );
    let replay = post(&f, "/new", &mut admin, &fields, &[]).await;
    replay.redirect(published.headers[header::LOCATION].to_str().unwrap());
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await, 1);
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn preview_and_published_markdown_share_sanitization_and_live_member_names() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    f.execute("INSERT INTO gonof_common_member (uid,username,email,password,emailstatus,adminid,regdate) VALUES (100,'mentioned-user','private@example.test','unused',1,0,1)").await;
    let body = "[@old-name](member:100)\n\n**Bold text** ~~Struck text~~\n\n| Column |\n|---|\n| Table |\n\n<spoiler>\n\nHidden🍓\n\n</spoiler>\n\n<details class=\"write-fullscreen\" open><summary>Fake notice</summary>\n\nCovers the whole page\n\n</details>\n\n<script>window.attack=1</script>\n\n[Bad link](javascript:alert) <img src=\"https://foreign.invalid/a.png\" onerror=\"alert(1)\">\n\n```rust\nlet n = 1;\n```";
    let preview = post(&f, "/api/preview", &mut admin, &[("body", body)], HX).await;
    preview.status(StatusCode::OK);
    for wanted in [
        "<strong>Bold text</strong>",
        "<del>Struck text</del>",
        "<table>",
        "<details",
        "Hidden🍓",
        "/space/100",
        "@mentioned-user",
        "<pre",
    ] {
        assert!(
            preview.html.contains(wanted),
            "missing {wanted}: {}",
            preview.html
        );
    }
    for forbidden in [
        "<script",
        "window.attack",
        "javascript:",
        "onerror=",
        "https://foreign.invalid/a.png",
        "@old-name",
        // User-written classes keep only those the renderer itself produces, so they cannot borrow the full-screen
        // editor's fixed overlay.
        "write-fullscreen",
    ] {
        assert!(
            !preview.html.contains(forbidden),
            "unsafe preview {forbidden}"
        );
    }
    assert!(
        preview
            .html
            .contains("<details class=\"reveal\"><summary>Show spoiler</summary>")
    );
    assert!(
        preview
            .html
            .contains("<details><summary>Fake notice</summary>")
    );
    let key = identity::random_token();
    let native_preview = post(
        &f,
        "/api/preview",
        &mut admin,
        &[
            ("title", "Preview before publishing"),
            ("fid", ""),
            ("body", "**Native preview**"),
            ("request_key", &key),
        ],
        &[],
    )
    .await;
    native_preview.status(StatusCode::OK);
    assert!(
        native_preview
            .html
            .contains("<strong>Native preview</strong>")
    );
    // The preview node comes from the template and must carry write-preview for the preview box styles to apply.
    assert!(
        native_preview
            .html
            .contains("<div data-editor-preview class=\"prose write-preview\">")
    );
    assert!(
        native_preview
            .html
            .contains("**Native preview**</textarea>")
    );
    assert!(native_preview.html.contains("name=\"deadline\" value=\"\""));
    assert!(
        native_preview
            .html
            .contains(&format!("name=\"request_key\" value=\"{key}\""))
    );
    request(
        &f,
        "POST",
        "/api/preview",
        &mut admin,
        &[("body", "CSRF")],
        HX,
    )
    .await
    .status(StatusCode::FORBIDDEN);
    post(
        &f,
        "/new",
        &mut admin,
        &[
            ("fid", &f.fid),
            ("title", "Shared Markdown rendering"),
            ("body", body),
            ("request_key", &key),
        ],
        &[],
    )
    .await
    .redirect("/thread-1#floor-1");
    let page = get(&f, "/thread-1", &mut admin).await;
    page.status(StatusCode::OK);
    assert!(page.html.contains("@mentioned-user"));
    // The opening floor runs from its id to the replies section.
    let opening = page
        .html
        .split("id=\"floor-1\"")
        .nth(1)
        .unwrap()
        .split("id=\"topic-replies\"")
        .next()
        .unwrap();
    assert!(!opening.contains("window.attack"));
    assert!(!opening.contains("write-fullscreen"));
    assert!(!page.html.contains("<script>window.attack"));
    f.execute("UPDATE gonof_common_member SET username='renamed-user' WHERE uid=100")
        .await;
    let renamed = get(&f, "/thread-1", &mut admin).await;
    assert!(renamed.html.contains("@renamed-user"));
    // Space URLs are fixed by uid; renaming only changes the link text.
    assert!(renamed.html.contains("/space/100"));
    let source: String =
        gono_forum_web::db::query_scalar("SELECT message FROM gonof_forum_post WHERE first=1")
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
    assert_eq!(source, body);
    let suggestions = get(&f, "/api/mentions?q=renamed", &mut admin).await;
    suggestions.status(StatusCode::OK);
    assert!(suggestions.html.contains("data-uid=\"100\""));
    assert!(suggestions.html.contains("data-username=\"renamed-user\""));
    assert!(!suggestions.html.contains("private@example.test"));
    assert!(
        get(&f, "/api/mentions?q=%25", &mut admin)
            .await
            .html
            .is_empty()
    );
    get(&f, "/api/mentions?q=admin", &mut Cookies::default())
        .await
        .status(StatusCode::UNAUTHORIZED);
    let profile = get(&f, "/space/1", &mut admin).await;
    profile.status(StatusCode::OK);
    assert!(profile.html.contains("Shared Markdown rendering"));
    assert!(profile.html.contains("/thread-1#floor-1"));
    let canonical = get(&f, "/space/001", &mut admin).await;
    canonical.status(StatusCode::MOVED_PERMANENTLY);
    assert_eq!(canonical.headers[header::LOCATION], "/space/1");
    for missing in ["/space/999999", "/space/0", "/space/admin"] {
        get(&f, missing, &mut admin)
            .await
            .status(StatusCode::NOT_FOUND);
    }
    f.execute("UPDATE gonof_forum_thread SET readperm=1 WHERE tid=1")
        .await;
    // Read paths judge by the sign of displayorder: after raw SQL changes the read permission, recompute by the
    // official rules.
    gono_forum_web::test_support::refresh_listing(&f.db)
        .await
        .unwrap();
    assert!(
        !get(&f, "/space/1", &mut admin)
            .await
            .html
            .contains("Shared Markdown rendering")
    );
    f._services.cleanup_ref().await.unwrap();
}

/// A failed HTMX reply puts the editor page in place of the thread (`posting::failure`). That page's form has no thread
/// title or opening post around its `#topic-replies`, so a reply that succeeds from there goes back to the thread page
/// instead of swapping in the reply list alone.
#[tokio::test]
async fn a_reply_retried_from_the_editor_page_goes_back_to_the_thread() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    let tid = f.topic(&mut admin).await;
    let path = format!("/thread-{tid}");
    let page = get(&f, &path, &mut admin).await;
    assert!(
        page.html
            .contains(r#"id="reply-editor" aria-labelledby="reply-editor-title" hx-headers='{"X-Composer-Context": "thread"}'"#),
        "{}",
        page.html
    );
    let endpoint = format!("/api/thread-{tid}/comments");
    let key = identity::random_token();
    let failed = post(
        &f,
        &endpoint,
        &mut admin,
        &[("body", "   "), ("request_key", &key), ("location", &path)],
        HX_THREAD,
    )
    .await;
    failed.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(failed.headers["HX-Retarget"], "body");
    assert!(failed.html.contains("id=\"topic-replies\""));
    assert!(!failed.html.contains("X-Composer-Context"));
    // The editor page's form sends no thread-page header, and the address bar still shows the thread: going there only
    // moves to the reply's fragment unless the page is reloaded too.
    let current = format!("{ORIGIN}{path}");
    let retried = post(
        &f,
        &endpoint,
        &mut admin,
        &[
            ("body", "Corrected reply"),
            ("request_key", &key),
            ("location", &path),
        ],
        &[("HX-Request", "true"), ("HX-Current-URL", &current)],
    )
    .await;
    retried.status(StatusCode::OK);
    assert_eq!(retried.headers["HX-Redirect"], format!("{path}#floor-2"));
    assert_eq!(retried.headers["HX-Refresh"], "true");
    assert!(!retried.html.contains("id=\"topic-replies\""));
    assert!(
        admin.0.contains_key("gonof_published__port58833"),
        "the redirected page clears the draft"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE tid={tid} AND first=0"
        ))
        .await,
        1
    );
    f._services.cleanup_ref().await.unwrap();
}

/// The form shown again after a failed save asks for a moderation reason only when the edit page would: a staff
/// member correcting their own post within the edit window edits it as its author.
#[tokio::test]
async fn a_failed_save_asks_for_a_moderation_reason_only_as_the_edit_page_does() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    let tid = f.topic(&mut admin).await;
    let path = format!("/edit/thread-{tid}");
    let form = get(&f, &path, &mut admin).await;
    form.status(StatusCode::OK);
    assert!(!form.html.contains("name=\"reason\""));
    let key = identity::random_token();
    let mut fields = vec![
        ("title", "Corrected title"),
        ("body", "   "),
        ("request_key", key.as_str()),
        ("expected_post_version", "1"),
        ("expected_thread_version", "1"),
    ];
    let failed = post(&f, &path, &mut admin, &fields, &[]).await;
    failed.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(failed.html.contains("Corrected title"));
    assert!(!failed.html.contains("name=\"reason\""), "{}", failed.html);
    fields[1] = ("body", "Corrected body");
    post(&f, &path, &mut admin, &fields, &[])
        .await
        .redirect(&format!("/thread-{tid}#floor-1"));
    // Recorded as the author's own edit: no moderation reason in the action log.
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='content.edit'")
            .await,
        1
    );
    assert_eq!(
        f.scalar(
            r#"SELECT COUNT(*) FROM gonof_common_log WHERE type='content.edit' AND data LIKE '%"reason":""%'"#
        )
        .await,
        1
    );

    // Past the edit window the same member can only edit it as staff: then the retry form asks for a reason.
    f.execute(&format!(
        "UPDATE gonof_forum_post SET dateline=dateline-100000000 WHERE tid={tid}"
    ))
    .await;
    let key = identity::random_token();
    let failed = post(
        &f,
        &path,
        &mut admin,
        &[
            ("title", "Moderated title"),
            ("body", "   "),
            ("request_key", key.as_str()),
            ("expected_post_version", "2"),
            ("expected_thread_version", "2"),
        ],
        &[],
    )
    .await;
    failed.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(failed.html.contains("name=\"reason\""), "{}", failed.html);
    f._services.cleanup_ref().await.unwrap();
}

/// A check whose retries ran out leaves the attachment `pending` with the detail "paused" until an administrator
/// schedules it again. The status the editor polls marks it (`data-check-paused`) beside the explanation, so the editor
/// stops polling and offers no Retry; a check that will retry by itself is not marked.
#[tokio::test]
async fn the_attachment_status_marks_a_paused_check() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    // The rows an upload leaves once its check stopped retrying (media/worker.rs, `finish_claim`).
    let uid = f
        .scalar("SELECT uid FROM gonof_common_member WHERE username='admin'")
        .await;
    f.execute(&format!(
        "INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES({uid},'{}','{}')",
        identity::random_token(),
        identity::random_token()
    ))
    .await;
    let aid = f
        .scalar("SELECT MAX(aid) FROM gonof_forum_attachment")
        .await;
    f.execute(&format!(
        "INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES({aid},'paused.png',10,'image/png','{}','paused','test','','',1,1,'pending','error','paused')",
        identity::random_token()
    ))
    .await;
    let status = format!("/api/media/{aid}");
    let paused = get(&f, &status, &mut admin).await;
    paused.status(StatusCode::OK);
    assert!(paused.html.contains("data-check-paused"), "{}", paused.html);
    assert!(
        paused
            .html
            .contains("paused until an administrator schedules a retry")
    );
    f.execute(&format!(
        "UPDATE gonof_forum_attachment_0 SET scan_detail='retrying' WHERE aid={aid}"
    ))
    .await;
    let retrying = get(&f, &status, &mut admin).await;
    retrying.status(StatusCode::OK);
    assert!(retrying.html.contains("it will retry automatically"));
    assert!(!retrying.html.contains("data-check-paused"));
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn replies_resolve_root_and_child_pages_with_native_and_htmx_navigation() {
    let f = Fixture::new().await;
    let mut admin = f.admin.clone();
    let tid = f.topic(&mut admin).await;
    let path = format!("/thread-{tid}");
    let endpoint = format!("/api/thread-{tid}/comments");
    let root_key = identity::random_token();
    let root = post(
        &f,
        &endpoint,
        &mut admin,
        &[
            ("body", "Top-level reply over HTTP"),
            ("request_key", &root_key),
            ("location", &path),
            ("tid", "999"),
            ("root_pid", "999"),
            ("reply_to_pid", "999"),
        ],
        HX_THREAD,
    )
    .await;
    root.status(StatusCode::OK);
    assert_eq!(root.headers["HX-Push-Url"], format!("{path}#floor-2"));
    assert!(root.html.contains("id=\"floor-2\""));
    // The swapped replies section says the thread's new highest floor (docs/skins.md, "Behavior modules").
    assert!(root.html.contains("data-last-position=\"2\">"));
    assert!(
        root.html
            .contains("id=\"topic-reply-count\" hx-swap-oob=\"true\">1 reply</span>")
    );
    assert!(!root.html.contains("<!doctype html>"));
    let root_pid = f
        .scalar("SELECT pid FROM gonof_forum_post WHERE position=2")
        .await;
    let child_endpoint = format!("/api/posts/{root_pid}/sub-comments");
    let child1 = post(
        &f,
        &child_endpoint,
        &mut admin,
        &[
            ("body", "First nested reply"),
            ("request_key", &identity::random_token()),
            ("location", &path),
            ("tid", "999"),
        ],
        HX_THREAD,
    )
    .await;
    child1.status(StatusCode::OK);
    assert_eq!(
        child1.headers["HX-Push-Url"],
        format!("{path}?sub_root={root_pid}&sub_page=1#floor-3")
    );
    assert!(child1.html.contains("id=\"floor-3\""));
    assert!(child1.html.contains("data-last-position=\"3\">"));
    let child_pid = f
        .scalar("SELECT pid FROM gonof_forum_post WHERE position=3")
        .await;
    let child2 = post(
        &f,
        &child_endpoint,
        &mut admin,
        &[
            ("body", "Answer to the nested reply"),
            ("request_key", &identity::random_token()),
            ("reply_to_pid", &child_pid.to_string()),
        ],
        &[],
    )
    .await;
    child2.redirect(&format!("{path}?sub_root={root_pid}&sub_page=1#floor-4"));
    assert_eq!(
        f.scalar("SELECT reply_to_pid FROM gonof_forum_post WHERE position=4")
            .await,
        child_pid
    );
    assert_eq!(
        f.scalar("SELECT root_pid FROM gonof_forum_post WHERE position=4")
            .await,
        root_pid
    );
    post(
        &f,
        &format!("/api/posts/{child_pid}/sub-comments"),
        &mut admin,
        &[
            ("body", "No third level"),
            ("request_key", &identity::random_token()),
        ],
        &[],
    )
    .await
    .status(StatusCode::NOT_FOUND);

    let writer = f.writer();
    for _ in 0..18 {
        writer
            .reply(
                1,
                NewReply {
                    tid,
                    root_pid: Some(root_pid),
                    reply_to_pid: Some(root_pid),
                    body: "Filling the nested page".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
    }
    let child_key = identity::random_token();
    let child_fields = [
        ("body", "Nested reply on page two"),
        ("request_key", child_key.as_str()),
        ("location", path.as_str()),
    ];
    let cross_child = post(&f, &child_endpoint, &mut admin, &child_fields, HX_THREAD).await;
    cross_child.status(StatusCode::OK);
    let child_location = format!("{path}?sub_root={root_pid}&sub_page=2#floor-23");
    assert_eq!(cross_child.headers["HX-Redirect"], child_location);
    let marker = admin
        .0
        .get("gonof_published__port58833")
        .expect("a cross-page success keeps the new confirmation; clearing the old marker must not overwrite it")
        .clone();
    assert!(marker.contains(&child_key));
    get(&f, "/api/mentions?q=admin", &mut admin)
        .await
        .status(StatusCode::OK);
    assert_eq!(
        admin.0.get("gonof_published__port58833"),
        Some(&marker),
        "API polling must not consume the full-page draft confirmation"
    );
    let page = get(&f, child_location.split('#').next().unwrap(), &mut admin).await;
    page.status(StatusCode::OK);
    assert!(page.html.contains("id=\"floor-23\""));
    assert!(page.html.contains(&child_key));
    assert!(!admin.0.contains_key("gonof_published__port58833"));
    assert!(!page.html.contains("id=\"floor-3\""));
    let mut replay = child_fields;
    replay[2] = ("location", child_location.split('#').next().unwrap());
    let same_child = post(&f, &child_endpoint, &mut admin, &replay, HX_THREAD).await;
    assert_eq!(same_child.headers["HX-Push-Url"], child_location);
    assert!(same_child.html.contains("id=\"floor-23\""));
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_post WHERE root_pid IS NOT NULL")
            .await,
        21
    );

    for _ in 0..49 {
        writer
            .reply(
                1,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: "Filling the top-level page".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
    }
    let cross = post(
        &f,
        &endpoint,
        &mut admin,
        &[
            ("body", "Top-level reply on page two"),
            ("request_key", &identity::random_token()),
            ("location", &path),
        ],
        HX_THREAD,
    )
    .await;
    cross.status(StatusCode::OK);
    assert_eq!(cross.headers["HX-Redirect"], format!("{path}-2#floor-73"));
    assert!(!cross.headers.contains_key("HX-Refresh"));
    let page2 = get(&f, &format!("{path}-2"), &mut admin).await;
    page2.status(StatusCode::OK);
    assert!(page2.html.contains("id=\"floor-73\""));
    let flat = post(
        &f,
        &endpoint,
        &mut admin,
        &[
            ("body", "Flat view position"),
            ("request_key", &identity::random_token()),
            ("view", "flat"),
        ],
        &[],
    )
    .await;
    flat.redirect(&format!("{path}-2?view=flat#floor-74"));
    let displayed = get(&f, &format!("{path}-2?view=flat"), &mut admin).await;
    assert!(displayed.html.contains("id=\"floor-74\""));

    f.execute(&format!(
        "UPDATE gonof_forum_thread SET closed=1 WHERE tid={tid}"
    ))
    .await;
    post(
        &f,
        &endpoint,
        &mut admin,
        &[
            ("body", "This reply is kept after closing"),
            ("request_key", &identity::random_token()),
            ("location", &path),
        ],
        HX,
    )
    .await
    .retained_error(StatusCode::FORBIDDEN, "This reply is kept after closing");
    assert_eq!(
        f.scalar("SELECT MAX(position) FROM gonof_forum_post").await,
        74
    );
    f._services.cleanup_ref().await.unwrap();
}
