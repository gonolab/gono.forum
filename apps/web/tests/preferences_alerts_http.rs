//! Private message mail reminders, new device login alerts and reading preferences.
//!
//! Mail always goes to the local delivery directory and is never sent out.
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::OnceLock};

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionMetadata},
    messages,
    notifications::preferences,
    router,
    test_support::{self, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const PASSWORD: &str = "Prefs-alerts-password-2026!";
const CHROME_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

struct Fixture {
    services: TestFixture,
    app: Router,
    identity: Identity,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        content::seed_forums(&services.db).await.unwrap();
        let identity = Identity::new(
            services.db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let app = router::build(services.config.clone(), &services.root, services.db.clone());
        Self {
            services,
            app,
            identity,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    fn mail_dir(&self) -> PathBuf {
        self.services.root.join(".local/mail")
    }

    async fn deliver(&self, now: i64) -> gono_forum_web::mail::DeliveryReport {
        test_support::deliver_mail(self.db(), &self.mail_dir(), ORIGIN, now)
            .await
            .unwrap()
    }

    fn mail(&self, eventid: i64) -> String {
        std::fs::read_to_string(self.mail_dir().join(format!("{eventid}.eml"))).unwrap()
    }

    /// Mail events of one kind: (eventid, status, last_error, payload, dateline, available_at).
    async fn events(
        &self,
        kind: &str,
        uid: i64,
    ) -> Vec<(i64, String, Option<String>, String, i64, i64)> {
        db::query("SELECT eventid,status,last_error,payload,dateline,available_at FROM gonof_common_outbox WHERE event_type=? AND aggregate_id=? ORDER BY eventid")
            .bind(kind)
            .bind(uid)
            .fetch_all(self.db().executor())
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    row.try_get("eventid").unwrap(),
                    db::text(&row, "status").unwrap(),
                    row.try_get("last_error").unwrap(),
                    db::text(&row, "payload").unwrap(),
                    row.try_get("dateline").unwrap(),
                    row.try_get("available_at").unwrap(),
                )
            })
            .collect()
    }

    async fn exec(&self, sql: &str) {
        db::query(sql).execute(self.db().executor()).await.unwrap();
    }
}

fn password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let salt = SaltString::encode_b64(b"prefs-alerts-salt").unwrap();
        Argon2::default()
            .hash_password(PASSWORD.as_bytes(), &salt)
            .unwrap()
            .to_string()
    })
}

/// A synthetic member with the verified e-mail `{username}@example.test` and password [`PASSWORD`].
async fn member(db: &Database, uid: i64, username: &str) {
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,?,?,1,1)")
        .bind(uid)
        .bind(username)
        .bind(password_hash())
        .bind(format!("{username}@example.test"))
        .execute(db.executor())
        .await
        .unwrap();
}

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn set_preferences(
    db: &Database,
    uid: i64,
    change: impl FnOnce(&mut preferences::Preferences),
) {
    let mut wanted = preferences::get(db, actor(uid)).await.unwrap();
    change(&mut wanted);
    preferences::save(db, actor(uid), &wanted).await.unwrap();
}

fn key() -> String {
    identity::random_token()
}

// ---------------------------------------------------------------------------
// Private message mail reminders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn message_mail_is_opt_in_once_per_unread_streak_delayed_and_carries_no_body() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9401, "dm-sender").await;
    member(db, 9402, "dm-reader").await;
    // Off by default: sending a message queues nothing.
    let thread = messages::send(
        db,
        actor(9401),
        9402,
        "A message while alerts are off by default",
        &key(),
    )
    .await
    .unwrap();
    assert!(f.events("message.email", 9402).await.is_empty());
    messages::conversation(db, 9402, thread, Some(1), 20)
        .await
        .unwrap();

    set_preferences(db, 9402, |p| p.email_message = true).await;
    messages::send(db, actor(9401), 9402, "Second secret body", &key())
        .await
        .unwrap();
    messages::send(db, actor(9401), 9402, "Third secret body", &key())
        .await
        .unwrap();
    let events = f.events("message.email", 9402).await;
    assert_eq!(
        events.len(),
        1,
        "more messages during the unread run queue nothing more"
    );
    let (eventid, _, _, payload, dateline, available_at) = events[0].clone();
    assert_eq!(
        available_at,
        dateline + 600,
        "delivery is delayed by 10 minutes"
    );
    assert!(!payload.contains("secret body") && !payload.contains("@example.test"));

    // Not due yet: not claimed.
    let early = f.deliver(dateline + 1).await;
    assert_eq!(early.claimed, 0);
    let sent = f.deliver(dateline + 601).await;
    assert_eq!((sent.delivered, sent.skipped), (1, 0));
    let mail = f.mail(eventid);
    assert!(mail.contains("To: dm-reader@example.test"));
    assert!(mail.contains("Subject: [gonof] New private message"));
    assert!(mail.contains(
        "dm-sender sent you a private message. This conversation has 2 unread messages:"
    ));
    assert!(mail.contains(&format!("{ORIGIN}/my/messages/{thread}")));
    assert!(
        !mail.contains("secret body"),
        "the mail carries no message body"
    );

    // The other side sends again after it was read: a new unread run starts and another mail is queued.
    messages::conversation(db, 9402, thread, Some(1), 20)
        .await
        .unwrap();
    messages::send(db, actor(9401), 9402, "A new message after reading", &key())
        .await
        .unwrap();
    let events = f.events("message.email", 9402).await;
    assert_eq!(events.len(), 2);
    // Read on the site within 10 minutes: abandoned when due, with the reason "read".
    messages::conversation(db, 9402, thread, Some(1), 20)
        .await
        .unwrap();
    let report = f.deliver(events[1].4 + 601).await;
    assert_eq!((report.delivered, report.skipped), (0, 1));
    let events = f.events("message.email", 9402).await;
    assert_eq!(events[1].1, "failed");
    assert_eq!(events[1].2.as_deref(), Some("the recipient has read it"));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn message_mail_rechecks_blocks_freezes_preferences_and_email_before_sending() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9411, "dm-a").await;
    member(db, 9412, "dm-b").await;
    set_preferences(db, 9412, |p| p.email_message = true).await;

    // Each case opens a new unread run: send one to queue it, change the state, abandon at delivery.
    let cases: [(&str, &str, &str); 5] = [
        (
            "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES(9412,9411,1)",
            "DELETE FROM gonof_home_member_block WHERE uid=9412",
            "the recipient blocked the sender",
        ),
        (
            "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=9411",
            "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=9411",
            "the sender account is frozen, deleted or inactive",
        ),
        (
            "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=9412",
            "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=9412",
            "the recipient account is frozen, deleted or inactive",
        ),
        (
            "UPDATE gonof_home_notification_preference SET email_message=0 WHERE uid=9412",
            "UPDATE gonof_home_notification_preference SET email_message=1 WHERE uid=9412",
            "the recipient turned off message alerts by email",
        ),
        (
            "UPDATE gonof_common_member SET email='dm-b-new@example.test' WHERE uid=9412",
            "UPDATE gonof_common_member SET email='dm-b@example.test' WHERE uid=9412",
            "the recipient email address is unverified or changed",
        ),
    ];
    let mut thread = 0;
    for (index, (change, restore, reason)) in cases.into_iter().enumerate() {
        thread = messages::send(db, actor(9411), 9412, &format!("Round {index}"), &key())
            .await
            .unwrap();
        f.exec(change).await;
        let event = f.events("message.email", 9412).await.pop().unwrap();
        let report = f.deliver(event.5 + 1).await;
        assert_eq!((report.delivered, report.skipped), (0, 1), "{reason}");
        let event = f.events("message.email", 9412).await.pop().unwrap();
        assert_eq!(event.2.as_deref(), Some(reason));
        f.exec(restore).await;
        messages::conversation(db, 9412, thread, Some(1), 20)
            .await
            .unwrap();
    }
    assert_eq!(f.events("message.email", 9412).await.len(), 5);

    // When the recipient is frozen or the e-mail is unverified at send time: not queued at all.
    f.exec("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=9412")
        .await;
    messages::send(db, actor(9411), 9412, "While frozen", &key())
        .await
        .unwrap();
    f.exec("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=9412")
        .await;
    messages::conversation(db, 9412, thread, Some(1), 20)
        .await
        .unwrap();
    f.exec("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=9412")
        .await;
    messages::send(db, actor(9411), 9412, "While unverified", &key())
        .await
        .unwrap();
    assert_eq!(f.events("message.email", 9412).await.len(), 5);
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn message_mail_is_capped_per_recipient_per_hour() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9421, "dm-flood").await;
    member(db, 9422, "dm-target").await;
    set_preferences(db, 9422, |p| p.email_message = true).await;
    let now = identity::now();
    // 10 mails to 9422 within an hour already (from different conversations), so the 11th is not queued; other
    // people's mails do not count.
    for n in 0..10 {
        db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES(?,'message.email','member',9422,'{}',?,?,'sent')")
            .bind(format!("message.email:9422:{}", 900_000 + n))
            .bind(now - 60)
            .bind(now - 60)
            .execute(db.executor())
            .await
            .unwrap();
    }
    db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at,status) VALUES('message.email:94220:1','message.email','member',9421,'{}',?,?,'sent')")
        .bind(now - 60)
        .bind(now - 60)
        .execute(db.executor())
        .await
        .unwrap();
    messages::send(db, actor(9421), 9422, "The eleventh", &key())
        .await
        .unwrap();
    assert_eq!(f.events("message.email", 9422).await.len(), 10);
    // Ones older than an hour do not count: move them outside the window, and the next unread run is queued as usual.
    f.exec("UPDATE gonof_common_outbox SET dateline=dateline-3600,available_at=available_at-3600 WHERE event_key LIKE 'message.email:9422:9%'").await;
    let thread: i64 =
        db::query_scalar("SELECT id FROM gonof_home_message_thread WHERE pair_key='9421:9422'")
            .fetch_one(db.executor())
            .await
            .unwrap();
    messages::conversation(db, 9422, thread, Some(1), 20)
        .await
        .unwrap();
    messages::send(db, actor(9421), 9422, "After the window", &key())
        .await
        .unwrap();
    assert_eq!(f.events("message.email", 9422).await.len(), 11);
    f.services.cleanup_ref().await.unwrap();
}

// ---------------------------------------------------------------------------
// New device login alerts
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Jar(BTreeMap<String, String>);

impl Jar {
    fn update(&mut self, headers: &HeaderMap) {
        for cookie in headers.get_all(header::SET_COOKIE) {
            let (name, value) = cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap();
            if value.is_empty() {
                self.0.remove(name);
            } else {
                self.0.insert(name.into(), value.into());
            }
        }
    }
    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    jar: &mut Jar,
    fields: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, jar.header());
    if method == "POST" {
        builder = builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, test_support::host_of(ORIGIN));
    }
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut request = builder.body(Body::from(form)).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "198.51.100.23:51000".parse::<SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    jar.update(&headers);
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
    Reply {
        status,
        headers,
        html,
    }
}

/// Web login: get the anonymous CSRF first, then submit the form. The session cookie stays in the jar, and so does the
/// device marker.
async fn web_login(f: &Fixture, jar: &mut Jar, username: &str) -> Reply {
    jar.0.remove("gonof_session__port58833");
    send(&f.app, "GET", "/login", jar, &[], &[]).await;
    let csrf = jar.0.get("gonof_csrf__port58833").cloned().unwrap();
    send(
        &f.app,
        "POST",
        "/login",
        jar,
        &[
            ("_csrf", &csrf),
            ("username", username),
            ("password", PASSWORD),
        ],
        &[("user-agent", CHROME_MAC)],
    )
    .await
}

#[tokio::test]
async fn login_alert_fires_only_on_new_devices_and_leaks_no_secrets() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9501, "alert-member").await;
    let mut laptop = Jar::default();
    let first = web_login(&f, &mut laptop, "alert-member").await;
    assert_eq!(first.status, StatusCode::SEE_OTHER, "{}", first.html);
    let marker = laptop
        .0
        .get("gonof_device__port58833")
        .cloned()
        .expect("a device marker is issued");
    let cookie = first
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .find(|v| v.starts_with("gonof_device__port58833="))
        .unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
    assert!(cookie.contains(&format!("Max-Age={}", 400 * 86_400)));
    assert_eq!(
        f.events("login_alert", 9501).await.len(),
        1,
        "the first login is a new device"
    );

    // Logging in again in the same browser after logging out: it carries the device marker, so no alert.
    let session = laptop.0.get("gonof_session__port58833").cloned().unwrap();
    let csrf = f
        .identity
        .session(&session)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    send(
        &f.app,
        "POST",
        "/logout",
        &mut laptop,
        &[("_csrf", &csrf)],
        &[],
    )
    .await;
    assert_eq!(
        laptop.0.get("gonof_device__port58833"),
        Some(&marker),
        "logging out keeps the device marker"
    );
    web_login(&f, &mut laptop, "alert-member").await;
    assert_eq!(f.events("login_alert", 9501).await.len(), 1);

    // Another device: alert.
    let mut phone = Jar::default();
    web_login(&f, &mut phone, "alert-member").await;
    let events = f.events("login_alert", 9501).await;
    assert_eq!(events.len(), 2);
    for (_, _, _, payload, _, _) in &events {
        assert!(!payload.contains("198.51.100.23") && !payload.contains("Chrome"));
        assert!(!payload.contains(PASSWORD));
    }

    let report = f.deliver(identity::now() + 1).await;
    assert_eq!(report.delivered, 2);
    let mail = f.mail(events[1].0);
    assert!(mail.contains("To: alert-member@example.test"));
    assert!(mail.contains("Subject: [gonof] New sign-in to your account"));
    assert!(mail.contains("IP: 198.51.100.23"));
    assert!(mail.contains("Device: Chrome · macOS"));
    assert!(mail.contains("(UTC+8)"));
    assert!(mail.contains(&format!("{ORIGIN}/my/account")));
    // Contains no password, session, CSRF or device marker, and not the raw UA either.
    let phone_session = phone.0.get("gonof_session__port58833").cloned().unwrap();
    let phone_csrf = f
        .identity
        .session(&phone_session)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    for secret in [
        PASSWORD,
        phone_session.as_str(),
        phone_csrf.as_str(),
        phone.0["gonof_device__port58833"].as_str(),
        marker.as_str(),
        "AppleWebKit",
    ] {
        assert!(!mail.contains(secret), "the mail must not contain {secret}");
    }
    // The login log and the Outbox keep no password or token either.
    let logs: Vec<String> = db::query_scalar("SELECT data FROM gonof_common_log WHERE uid=9501")
        .fetch_all(db.executor())
        .await
        .unwrap();
    let outbox: Vec<String> =
        db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE aggregate_id=9501")
            .fetch_all(db.executor())
            .await
            .unwrap();
    for text in logs.iter().chain(&outbox) {
        for secret in [PASSWORD, phone_session.as_str(), phone_csrf.as_str()] {
            assert!(!text.contains(secret));
        }
    }
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn login_alert_respects_preference_verification_and_the_hourly_cap() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9511, "alert-capped").await;
    member(db, 9512, "alert-off").await;
    member(db, 9513, "alert-unverified").await;
    // A new unmarked browser every time: the first 5 alert, and from the 6th on nothing is queued.
    for _ in 0..6 {
        let reply = web_login(&f, &mut Jar::default(), "alert-capped").await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER);
    }
    assert_eq!(f.events("login_alert", 9511).await.len(), 5);
    // Turn off login alerts.
    set_preferences(db, 9512, |p| p.email_login = false).await;
    web_login(&f, &mut Jar::default(), "alert-off").await;
    assert!(f.events("login_alert", 9512).await.is_empty());
    // Not sent to an unverified e-mail.
    f.exec("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=9513")
        .await;
    web_login(&f, &mut Jar::default(), "alert-unverified").await;
    assert!(f.events("login_alert", 9513).await.is_empty());

    // Frozen after queuing, or the preference turned off: abandoned at delivery.
    let events = f.events("login_alert", 9511).await;
    f.exec("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=9511")
        .await;
    let report = f.deliver(identity::now() + 1).await;
    assert_eq!((report.delivered, report.skipped), (0, 5));
    let events_after = f.events("login_alert", 9511).await;
    assert_eq!(events_after.len(), events.len());
    assert!(
        events_after
            .iter()
            .all(|e| e.2.as_deref() == Some("the recipient account is frozen, deleted or inactive"))
    );
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn registering_marks_the_browser_and_programmatic_logins_send_nothing() {
    let f = Fixture::new().await;
    let db = f.db();
    let admin_password = gono_forum_web::test_support::create_admin(&f.identity).await;
    let admin = f.identity.login("admin", &admin_password).await.unwrap();
    assert!(
        admin.device_marker.is_none(),
        "a login outside the web issues no marker"
    );
    let invite = f
        .identity
        .generate_invite(admin.member.uid, "prefs-alerts-invite")
        .await
        .unwrap();
    let registered = f
        .identity
        .register_with_metadata(
            "fresh-member",
            "fresh-member@example.test",
            PASSWORD,
            &invite.code,
            &SessionMetadata::new(CHROME_MAC, "203.0.113.9").with_device(""),
        )
        .await
        .unwrap();
    let marker = registered
        .device_marker
        .clone()
        .expect("registration issues a device marker");
    let uid = registered.member.uid;
    db::query("UPDATE gonof_common_member SET emailstatus=1 WHERE uid=?")
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
    // The browser used at registration: a known device.
    f.identity
        .login_with_metadata(
            "fresh-member",
            PASSWORD,
            &SessionMetadata::new(CHROME_MAC, "203.0.113.9").with_device(&marker),
        )
        .await
        .unwrap();
    // Logins not through the web (tests, CLI) do not check the device.
    f.identity.login("fresh-member", PASSWORD).await.unwrap();
    assert!(f.events("login_alert", uid).await.is_empty());
    // Another browser: alert.
    f.identity
        .login_with_metadata(
            "fresh-member",
            PASSWORD,
            &SessionMetadata::new(CHROME_MAC, "203.0.113.9").with_device(""),
        )
        .await
        .unwrap();
    assert_eq!(f.events("login_alert", uid).await.len(), 1);
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn notification_preference_form_saves_the_two_new_switches() {
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9521, "prefs-form").await;
    let mut jar = Jar::default();
    web_login(&f, &mut jar, "prefs-form").await;
    let page = send(&f.app, "GET", "/my/preferences", &mut jar, &[], &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    // Defaults: private message mail off, login alerts on.
    assert!(page.html.contains("name=\"email_message\" value=\"true\">"));
    assert!(
        page.html
            .contains("name=\"email_login\" value=\"true\" checked>")
    );
    let session = jar.0["gonof_session__port58833"].clone();
    let csrf = f
        .identity
        .session(&session)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    let saved = send(
        &f.app,
        "POST",
        "/my/preferences",
        &mut jar,
        &[
            ("_csrf", &csrf),
            ("version", "0"),
            ("email_message", "true"),
        ],
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.html);
    let stored = preferences::get(db, actor(9521)).await.unwrap();
    assert!(stored.email_message && !stored.email_login);
    let bad = send(
        &f.app,
        "POST",
        "/my/preferences",
        &mut jar,
        &[("_csrf", &csrf), ("version", "1"), ("email_login", "yes")],
        &[],
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    f.services.cleanup_ref().await.unwrap();
}

// ---------------------------------------------------------------------------
// Reading preferences
// ---------------------------------------------------------------------------

fn canonical(html: &str) -> &str {
    let rest = &html[html.find("<link rel=\"canonical\" href=\"").unwrap() + 28..];
    &rest[..rest.find('"').unwrap()]
}

/// The order in which threads appear in the list (by `data-thread-id`).
fn order(html: &str) -> Vec<i64> {
    html.split("data-thread-id=\"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].parse().unwrap())
        .collect()
}

#[tokio::test]
async fn reading_preferences_drive_list_sort_title_links_and_permalink_view_with_and_without_htmx()
{
    let f = Fixture::new().await;
    let db = f.db();
    member(db, 9601, "reading-author").await;
    member(db, 9602, "reading-viewer").await;
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let now = identity::now() - 1000;
    let thread = |title: &str| NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: title.into(),
        body: "First post of the reading preference test".into(),
        attachment_ids: vec![],
        request_key: key(),
    };
    // `older` is posted first and later gets a reply; `newer` is posted later with no reply. Latest reply: older,
    // newer; latest thread: newer, older.
    let older = writer
        .publish_at(9601, thread("Reading preference A"), now)
        .await
        .unwrap();
    let newer = writer
        .publish_at(9601, thread("Reading preference B"), now + 50)
        .await
        .unwrap();
    let reply = writer
        .reply_at(
            9601,
            NewReply {
                tid: older.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A reply to A".into(),
                attachment_ids: vec![],
                request_key: key(),
            },
            now + 100,
        )
        .await
        .unwrap();
    let mut anonymous = Jar::default();
    let mut viewer = Jar::default();
    web_login(&f, &mut viewer, "reading-viewer").await;

    // Anonymous: the site default.
    let home = send(&f.app, "GET", "/", &mut anonymous, &[], &[]).await;
    assert_eq!(order(&home.html), [older.tid, newer.tid]);
    assert!(home.html.contains(&format!(
        "href=\"/thread-{}\">Reading preference A",
        older.tid
    )));
    assert!(
        !home.html.contains("rel=\"noopener\">Reading preference"),
        "anonymous title links do not open a new tab"
    );
    let anonymous_reading = send(&f.app, "GET", "/my/reading", &mut anonymous, &[], &[]).await;
    assert_eq!(anonymous_reading.status, StatusCode::SEE_OTHER);

    // Save: latest thread, flat, title jumps to the latest reply, new tab.
    let page = send(&f.app, "GET", "/my/reading", &mut viewer, &[], &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    // A member who never chose has "follow the site" for every item (the columns are NULL).
    assert!(page.html.contains("name=\"list_sort\" value=\"\" checked"));
    assert!(page.html.contains("Follow the site (now Latest replies)"));
    assert!(page.html.contains("data-device-settings hidden"));
    let session = viewer.0["gonof_session__port58833"].clone();
    let csrf = f
        .identity
        .session(&session)
        .await
        .unwrap()
        .unwrap()
        .csrf_token;
    let fields = [
        ("_csrf", csrf.as_str()),
        ("version", "0"),
        ("list_sort", "created"),
        ("thread_view", "flat"),
        ("title_latest", "true"),
        ("title_new_tab", "true"),
    ];
    let saved = send(&f.app, "POST", "/my/reading", &mut viewer, &fields, &[]).await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.html);
    assert_eq!(saved.headers[header::LOCATION], "/my/reading?saved=true");
    let stale = send(&f.app, "POST", "/my/reading", &mut viewer, &fields, &[]).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    let invalid = send(
        &f.app,
        "POST",
        "/my/reading",
        &mut viewer,
        &[
            ("_csrf", csrf.as_str()),
            ("version", "1"),
            ("list_sort", "pinned"),
            ("thread_view", "flat"),
        ],
        &[],
    )
    .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);

    // No-JS and HTMX requests give the same result.
    for extra in [&[][..], &[("HX-Request", "true")][..]] {
        let home = send(&f.app, "GET", "/", &mut viewer, &[], extra).await;
        assert_eq!(home.status, StatusCode::OK);
        assert_eq!(
            order(&home.html),
            [newer.tid, older.tid],
            "the bare address follows the default sort"
        );
        assert!(home.html.contains(&format!(
            "href=\"/posts/{}\" target=\"_blank\" rel=\"noopener\">Reading preference A",
            reply.pid
        )));
        assert!(home.html.contains(&format!(
            "href=\"/thread-{}?view=flat\" target=\"_blank\" rel=\"noopener\">Reading preference B",
            newer.tid
        )));
        // Switching back to latest reply needs an explicit parameter; canonical is still built from the actual sort,
        // independent of the visitor.
        assert!(home.html.contains("href=\"/?sort=latest\""));
        if extra.is_empty() {
            assert!(canonical(&home.html).ends_with("/?sort=created"));
        }
        let forum = send(&f.app, "GET", "/daily", &mut viewer, &[], extra).await;
        assert_eq!(
            order(&forum.html),
            [newer.tid, older.tid],
            "the forum follows the default sort too"
        );
        let latest = send(&f.app, "GET", "/?sort=latest", &mut viewer, &[], extra).await;
        assert_eq!(order(&latest.html), [older.tid, newer.tid]);
        // Normalizing the address keeps the sort it spelled out, so it still lands on latest replies.
        for (from, to) in [
            ("/page-1?sort=latest", "/?sort=latest"),
            ("/page-1", "/"),
            ("/daily-1?sort=latest", "/daily?sort=latest"),
        ] {
            let moved = send(&f.app, "GET", from, &mut viewer, &[], extra).await;
            assert_eq!(moved.status, StatusCode::MOVED_PERMANENTLY, "{from}");
            assert_eq!(moved.headers[header::LOCATION], to, "{from}");
        }
        // Search does not follow the default sort, but title links still follow the preference.
        let search = send(
            &f.app,
            "GET",
            "/search?q=Reading%20preference",
            &mut viewer,
            &[],
            extra,
        )
        .await;
        assert_eq!(order(&search.html), [older.tid, newer.tid]);
        assert!(
            search
                .html
                .contains(&format!("href=\"/posts/{}\"", reply.pid))
        );
    }

    // Permanent floor links redirect by the visitor's view preference; anonymous visitors always get two levels.
    let flat = send(
        &f.app,
        "GET",
        &format!("/posts/{}", reply.pid),
        &mut viewer,
        &[],
        &[],
    )
    .await;
    assert_eq!(flat.status, StatusCode::FOUND);
    assert!(
        flat.headers[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("view=flat")
    );
    let nested = send(
        &f.app,
        "GET",
        &format!("/posts/{}", reply.pid),
        &mut anonymous,
        &[],
        &[],
    )
    .await;
    assert_eq!(nested.status, StatusCode::FOUND);
    assert!(
        !nested.headers[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("view=flat")
    );
    // The bare thread URL is two levels for everyone: permanent floor links do not carry
    // view=flat.
    let detail = send(
        &f.app,
        "GET",
        &format!("/thread-{}", older.tid),
        &mut viewer,
        &[],
        &[],
    )
    .await;
    assert_eq!(detail.status, StatusCode::OK);
    assert!(
        detail
            .html
            .contains(&format!("href=\"/thread-{}#floor-2\"", older.tid))
    );

    // Anonymous is still the site default, unaffected by this member.
    let home = send(&f.app, "GET", "/", &mut anonymous, &[], &[]).await;
    assert_eq!(order(&home.html), [older.tid, newer.tid]);
    f.services.cleanup_ref().await.unwrap();
}
