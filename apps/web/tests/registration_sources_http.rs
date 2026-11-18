//! Walks from a landing page with a channel through registration, the admin summary and account deletion cleanup;
//! sub-path sites on the same host each count separately.
#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db, identity,
    media::Media,
    privacy::{self, Access, NewRequest},
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const OTHER_PORT: &str = "http://127.0.0.1:58844";
const ROOT_CHANNEL_COOKIE: &str = "gonof_registration_source__port58833";
const REPORT: &str = "/manage/registration-sources";
const PASSWORD: &str = "Source-password-2026!";

struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

impl Response {
    fn cookie(&self, name: &str) -> &str {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|v| v.split_once('=').is_some_and(|(k, _)| k == name))
            .unwrap_or_else(|| panic!("missing the {name} cookie"))
    }
}

async fn request(
    app: &Router,
    origin: &str,
    path: &str,
    cookies: &str,
    form: Option<&str>,
) -> Response {
    let mut request = Request::builder()
        .uri(path)
        .header(header::HOST, host_of(origin));
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, cookies);
    }
    if form.is_some() {
        request = request
            .method("POST")
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(form.unwrap_or_default().to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
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

fn pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().unwrap()
}

async fn register(
    app: &Router,
    origin: &str,
    path: &str,
    source: &str,
    username: &str,
    invite: &str,
) -> Response {
    let form = request(app, origin, path, source, None).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.html);
    let csrf = pair(form.cookie("gonof_csrf"));
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&form.html);
    let mut cookies = csrf.to_owned();
    if !source.is_empty() {
        cookies.push_str("; ");
        cookies.push_str(source);
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("_csrf", csrf.strip_prefix("gonof_csrf=").unwrap()),
            ("username", username),
            ("email", &format!("{username}@example.test")),
            ("password", PASSWORD),
            ("password2", PASSWORD),
            ("invite", invite),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ])
        .finish();
    request(app, origin, path, &cookies, Some(&body)).await
}

fn table<'a>(html: &'a str, id: &str) -> &'a str {
    html.split_once(&format!("id=\"{id}\""))
        .and_then(|(_, rest)| rest.split_once("</section>"))
        .expect("the report has the group table")
        .0
}

fn assert_group(table: &str, label: &str, count: usize) {
    assert!(
        table.contains(&format!("<th scope=\"row\">{label}</th><td>{count}</td>")),
        "no row with {count} registrations for {label}"
    );
}

async fn admin_app(f: &TestFixture) -> (Router, identity::Identity, String) {
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(1,'sourceadmin','fixture','admin@example.test',1,1,1)")
        .execute(f.db.executor()).await.unwrap();
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        f.global.clone(),
    );
    let admin = http_auth::login(&identity, &f.db, 1).await;
    let admin_cookie = format!("gonof_session={}", admin.session_token);
    let app = gono_forum_web::router::build_with_global(
        f.config.clone(),
        &f.root,
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    );

    (app, identity, admin_cookie)
}

#[tokio::test]
async fn registration_source_totals_follow_committed_signups_and_privacy_cleanup() {
    let f = TestFixture::migrated().await.unwrap();
    set_setting(&f.db, "settings.signup.mode", "open")
        .await
        .unwrap();
    set_setting(&f.db, "settings.invite.prefixes", "WX\nWX2026\nwx2026")
        .await
        .unwrap();
    set_setting(&f.db, "settings.domains.enabled", "true")
        .await
        .unwrap();
    content::seed_forums(&f.db).await.unwrap();
    db::query("INSERT INTO gonof_common_site_domain (site_url,site_name,dateline) VALUES (?,NULL,1),(?,'Sub Site',1),(?,'Other Port',1)")
        .bind(TEST_SITE_URL).bind(format!("{TEST_SITE_URL}/bbs")).bind(OTHER_PORT).execute(f.db.executor()).await.unwrap();
    db::query("INSERT INTO gonof_common_member (uid,username,password,email,emailstatus,adminid,regdate) VALUES (2,'sourcemod','fixture','mod@example.test',1,2,1)")
        .execute(f.db.executor()).await.unwrap();
    // A registration log row that records only the inviter, with no promotion channel, domain or fixed prefix.
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES (2,2,'identity.register',?,'web',1)")
        .bind(serde_json::json!({"inviter_uid": 1, "invite_id": 99}).to_string())
        .execute(f.db.executor()).await.unwrap();
    let (app, identity, admin_cookie) = admin_app(&f).await;
    // The matching registration source row: site and channel not recorded (NULL), inviter 1.
    db::query("INSERT INTO gonof_common_member_register(uid,site,channel,inviter_uid,invite_prefix) VALUES (2,NULL,NULL,1,NULL)")
        .execute(f.db.executor()).await.unwrap();
    let moderator = http_auth::login(&identity, &f.db, 2).await;
    let now = identity::now();
    db::query("INSERT INTO gonof_common_invite(uid,code,request_key,dateline,endtime) VALUES (1,'WX2026-once','source-invite',?,?)")
        .bind(now).bind(now + 3600).execute(f.db.executor()).await.unwrap();

    let landing = request(&app, TEST_SITE_URL, "/page-01?utm_source=old-tag", "", None).await;
    assert_eq!(landing.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(landing.headers[header::LOCATION], "/");
    assert!(
        landing.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let old_cookie = pair(landing.cookie(ROOT_CHANNEL_COOKIE));
    let landing = request(
        &app,
        TEST_SITE_URL,
        "/forums?utm_source=newsletter",
        old_cookie,
        None,
    )
    .await;
    assert_eq!(landing.status, StatusCode::OK);
    let root_cookie = landing.cookie(ROOT_CHANNEL_COOKIE);
    assert!(root_cookie.contains("Max-Age=2592000") && root_cookie.contains("Path=/register;"));
    let root_cookie = pair(root_cookie);
    // Browser cookies are not isolated by port, so registering on another port also sends the main site's marker.
    let cross_port = register(&app, OTHER_PORT, "/register", root_cookie, "sourceport", "").await;
    assert_eq!(
        cross_port.status,
        StatusCode::SEE_OTHER,
        "{}",
        cross_port.html
    );
    assert!(
        !cross_port
            .headers
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.to_str().unwrap().starts_with(ROOT_CHANNEL_COOKIE))
    );

    // The one-time code itself also matches a prefix; its first use belongs to the inviter, and only using the same
    // code again counts as the fixed prefix.
    let root_registration = register(
        &app,
        TEST_SITE_URL,
        "/register",
        root_cookie,
        "sourcenews",
        "WX2026-once",
    )
    .await;
    assert_eq!(
        root_registration.status,
        StatusCode::SEE_OTHER,
        "{}",
        root_registration.html
    );
    assert!(
        root_registration
            .cookie(ROOT_CHANNEL_COOKIE)
            .contains("Max-Age=0")
    );
    let member_cookie = pair(root_registration.cookie("gonof_session"));

    // Additionally attach the main site's channel cookie to simulate a wrong replay; the sub-site only accepts cookies
    // under its own path too.
    let unmarked = register(
        &app,
        TEST_SITE_URL,
        "/bbs/register",
        root_cookie,
        "sourceplain",
        "WX2026-once",
    )
    .await;
    assert_eq!(unmarked.status, StatusCode::SEE_OTHER, "{}", unmarked.html);
    let sub_landing = request(&app, TEST_SITE_URL, "/bbs/?utm_source=newsletter", "", None).await;
    let sub_cookie = sub_landing.cookie("gonof_registration_source_bbs__port58833");
    assert!(sub_cookie.contains("Path=/bbs/register;"));
    let sub_cookie = pair(sub_cookie);
    let sub = register(
        &app,
        TEST_SITE_URL,
        "/bbs/register",
        sub_cookie,
        "sourcesub",
        "WX2026-other",
    )
    .await;
    assert_eq!(sub.status, StatusCode::SEE_OTHER, "{}", sub.html);
    let failed = register(
        &app,
        TEST_SITE_URL,
        "/bbs/register",
        sub_cookie,
        "sourcesub",
        "WX2026-other",
    )
    .await;
    assert!(failed.status.is_client_error(), "{}", failed.html);

    let other = register(
        &app,
        "http://unregistered.test",
        "/register",
        "",
        "sourceother",
        "wx2026-share",
    )
    .await;
    assert_eq!(other.status, StatusCode::SEE_OTHER, "{}", other.html);
    assert_eq!(
        request(&app, TEST_SITE_URL, REPORT, "", None).await.status,
        StatusCode::SEE_OTHER
    );
    for cookie in [
        member_cookie.to_owned(),
        format!("gonof_session={}", moderator.session_token),
    ] {
        let denied = request(&app, TEST_SITE_URL, REPORT, &cookie, None).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        assert!(!denied.html.contains("newsletter"));
    }

    let report = request(&app, TEST_SITE_URL, REPORT, &admin_cookie, None).await;
    assert_eq!(report.status, StatusCode::OK, "{}", report.html);
    assert!(report.html.contains("holds <strong>6</strong> members"));
    let sites = table(&report.html, "sites");
    assert_group(sites, host_of(TEST_SITE_URL), 1);
    assert_group(sites, host_of(OTHER_PORT), 1);
    assert_group(sites, &format!("{}/bbs", host_of(TEST_SITE_URL)), 2);
    assert_group(sites, "Unlisted domain", 1);
    assert_group(sites, "Not recorded", 1);
    let channels = table(&report.html, "channels");
    assert_group(channels, "newsletter", 2);
    assert_group(channels, "No channel", 3);
    assert_group(channels, "Not recorded", 1);
    let inviters = table(&report.html, "inviters");
    assert_group(inviters, "sourceadmin (UID 1)", 2);
    assert_eq!(inviters.matches("scope=\"row\"").count(), 1);
    let prefixes = table(&report.html, "prefixes");
    assert_group(prefixes, "WX2026", 2);
    assert_group(prefixes, "wx2026", 1);
    assert_eq!(prefixes.matches("scope=\"row\"").count(), 2);
    assert!(!report.html.contains("old-tag") && !report.html.contains("unregistered.test"));

    let session = identity
        .session(member_cookie.strip_prefix("gonof_session=").unwrap())
        .await
        .unwrap()
        .unwrap();
    let now = identity::now();
    privacy::submit(
        &f.db,
        &Access::Session(session.member.actor()),
        NewRequest {
            kind: "close".into(),
            content_mode: "retain".into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        },
        now,
    )
    .await
    .unwrap();
    let media = Media::new(f.db.clone(), f.config.media.clone(), f.root.clone());
    privacy::process_pending(
        &f.db,
        &f.global,
        &media,
        &f.root,
        &f.config.identity.local_directory,
        90,
        now + privacy::COOLING_SECONDS,
    )
    .await
    .unwrap();
    let report = request(&app, TEST_SITE_URL, REPORT, &admin_cookie, None).await;
    assert_eq!(report.status, StatusCode::OK);
    assert!(report.html.contains("holds <strong>5</strong> members"));
    let channels = table(&report.html, "channels");
    assert_group(channels, "newsletter", 1);
    assert_group(channels, "No channel", 3);
    assert_group(table(&report.html, "inviters"), "sourceadmin (UID 1)", 1);
    assert_group(table(&report.html, "prefixes"), "WX2026", 2);
    f.cleanup().await.unwrap();
}

async fn insert_registration(f: &TestFixture, uid: i64, data: serde_json::Value) {
    db::query(
        "INSERT INTO gonof_common_member(uid,username,password,regdate) VALUES(?,?,'fixture',1)",
    )
    .bind(uid)
    .bind(format!("sample{uid}"))
    .execute(f.db.executor())
    .await
    .unwrap();
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'identity.register',?,'web',1)")
        .bind(uid).bind(uid).bind(data.to_string()).execute(f.db.executor()).await.unwrap();
    // Derives the registration source row from the log data: site and channel accept only strings, inviter only
    // integers, prefix only non-empty strings.
    let source = |key: &str| {
        data.get("registration_source")
            .and_then(|source| source.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let prefix = data
        .get("invite_prefix")
        .and_then(serde_json::Value::as_str)
        .filter(|prefix| !prefix.is_empty());
    db::query("INSERT INTO gonof_common_member_register(uid,site,channel,inviter_uid,invite_prefix) VALUES(?,?,?,?,?)")
        .bind(uid)
        .bind(source("site"))
        .bind(source("channel"))
        .bind(data.get("inviter_uid").and_then(serde_json::Value::as_i64))
        .bind(prefix)
        .execute(f.db.executor())
        .await
        .unwrap();
}

#[tokio::test]
async fn source_pages_preserve_case_and_separate_history_from_named_channels() {
    let f = TestFixture::migrated().await.unwrap();
    let (app, _, admin_cookie) = admin_app(&f).await;
    // The inviter has already deleted their account and cleared their own registration log; the invitee's
    // registration record still exists.
    db::query("INSERT INTO gonof_common_member(uid,username,account_state,regdate) VALUES(42,'deleted-42','deleted',1)")
        .execute(f.db.executor()).await.unwrap();
    // The two people of the same channel rank first. A row without a channel and a real channel with the same name as
    // the system label for it must each get their own group.
    let mut channels = vec![
        "Promo".to_owned(),
        "Promo".into(),
        "promo".into(),
        "Not recorded".into(),
    ];
    channels.extend((0..19).map(|i| format!("tag-{i:02}")));
    for (i, channel) in channels.iter().map(Some).chain([None]).enumerate() {
        let uid = i64::try_from(i).unwrap() + 2;
        let data = channel.map_or(serde_json::json!({}), |name| {
            serde_json::json!({
                "inviter_uid": if i == 0 { 42 } else { uid - 1 }, "invite_id": i + 1, "invite_prefix": null,
                "registration_source": {"site": "forum.test", "channel": name}
            })
        });
        insert_registration(&f, uid, data).await;
    }
    // Members registered with historical prefixes; the current settings no longer have these values. The string null
    // is a valid real prefix.
    let historical_prefixes = (0..19)
        .map(|i| format!("P{i:02}"))
        .chain(["null".to_owned(), "LongLegacyPrefix".to_owned()]);
    for (i, prefix) in historical_prefixes.enumerate() {
        insert_registration(
            &f,
            100 + i as i64,
            serde_json::json!({
                "invite_prefix": prefix, "inviter_uid": null, "invite_id": null
            }),
        )
        .await;
    }

    let first = request(&app, TEST_SITE_URL, REPORT, &admin_cookie, None).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_group(table(&first.html, "sites"), "forum.test", 23);
    let channels = table(&first.html, "channels");
    assert_eq!(channels.matches("scope=\"row\"").count(), 20);
    assert_group(channels, "Promo", 2);
    assert_group(channels, "promo", 1);
    assert_group(channels, "Not recorded", 22);
    // The real channel that reads like the label sorts before the tag-* channels, so it is on this page.
    assert_group(channels, "Not recorded (channel mark)", 1);
    assert!(!channels.contains("tag-16") && !channels.contains("tag-18"));
    let prefixes = table(&first.html, "prefixes");
    assert_eq!(prefixes.matches("scope=\"row\"").count(), 20);
    assert_group(prefixes, "LongLegacyPrefix", 1);
    assert!(!prefixes.contains("<th scope=\"row\">null</th>"));

    let next = request(
        &app,
        TEST_SITE_URL,
        &format!("{REPORT}?channel_page=2&inviter_page=2&prefix_page=2"),
        &admin_cookie,
        None,
    )
    .await;
    assert_eq!(next.status, StatusCode::OK);
    assert_group(table(&next.html, "sites"), "forum.test", 23);
    let channels = table(&next.html, "channels");
    assert_eq!(channels.matches("scope=\"row\"").count(), 3);
    assert_group(channels, "tag-16", 1);
    assert_group(channels, "tag-18", 1);
    assert!(!channels.contains("(channel mark)"));
    let inviters = table(&next.html, "inviters");
    assert_eq!(inviters.matches("scope=\"row\"").count(), 3);
    assert_group(inviters, "Deleted user (UID 42)", 1);
    let prefixes = table(&next.html, "prefixes");
    assert_eq!(prefixes.matches("scope=\"row\"").count(), 1);
    assert_group(prefixes, "null", 1);
    for (section, preserved) in [
        (channels, ["inviter_page=2", "prefix_page=2"]),
        (inviters, ["channel_page=2", "prefix_page=2"]),
        (prefixes, ["channel_page=2", "inviter_page=2"]),
    ] {
        let previous = section
            .split("href=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(
            preserved.iter().all(|param| previous.contains(param)),
            "{previous}"
        );
    }
    for (params, status) in [
        ("site_page=2", StatusCode::NOT_FOUND),
        ("channel_page=3", StatusCode::NOT_FOUND),
        ("channel_page=0", StatusCode::UNPROCESSABLE_ENTITY),
        ("prefix_page=3", StatusCode::NOT_FOUND),
        ("inviter_page=0", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        assert_eq!(
            request(
                &app,
                TEST_SITE_URL,
                &format!("{REPORT}?{params}"),
                &admin_cookie,
                None
            )
            .await
            .status,
            status
        );
    }
    f.cleanup().await.unwrap();
}
