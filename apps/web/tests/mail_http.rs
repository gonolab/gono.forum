use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db,
    identity::{self, Identity},
    mail::{MailInput, MailSettings, Mailer},
    moderation::{self, SanctionRequest},
    notifications::{self, preferences},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use std::time::Duration;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const SECRET: &str = "mail-test-authorization-secret";

struct Fixture {
    services: TestFixture,
    app: Router,
    mail: Mailer,
    identity: Identity,
    admin: identity::SessionGrant,
    member: identity::SessionGrant,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let mut config = services.config.clone();
        config.common.encryption_key = "23".repeat(32);
        content::seed_forums(&services.db).await.unwrap();
        let identity = Identity::new(
            services.db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let invite = identity
            .generate_invite(admin.member.uid, "mail-settings-member")
            .await
            .unwrap();
        let member = identity
            .register(
                "mailmember",
                "mailmember@example.test",
                "Mail-member-password-2026!",
                &invite.code,
            )
            .await
            .unwrap();
        let mail = Mailer::new(
            services.db.clone(),
            config.common.clone(),
            services.root.join(".local/mail"),
            gono_forum_web::test_support::detached_global(),
        );
        let app = router::build(config, &services.root, services.db.clone());
        Self {
            services,
            app,
            mail,
            identity,
            admin,
            member,
        }
    }

    async fn request(
        &self,
        method: &str,
        session: Option<&str>,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri("/manage/mail");
        if let Some(session) = session {
            request = request.header(
                header::COOKIE,
                format!("gonof_session__port58833={session}"),
            );
        }
        let body = if method == "POST" {
            request = request
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            let mut params = url::form_urlencoded::Serializer::new(String::new());
            params.extend_pairs(fields.iter().copied());
            Body::from(params.finish())
        } else {
            Body::empty()
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        assert!(
            response.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
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
        assert!(!html.contains(SECRET));
        assert!(!html.contains(&"23".repeat(32)));
        (status, html)
    }
    fn settings() -> MailSettings {
        MailSettings {
            transport: "smtp".into(),
            host: "127.0.0.1".into(),
            port: 1,
            username: "sender@example.test".into(),
            from_address: "sender@example.test".into(),
            test_to: "recipient@example.test".into(),
            incoming_host: "pop.example.test".into(),
            ..MailSettings::default()
        }
    }
}

#[tokio::test]
async fn mail_admin_form_persists_encrypted_configuration_and_checks_csrf_and_revisions() {
    let f = Fixture::new().await;
    assert_eq!(f.request("GET", None, &[]).await.0, StatusCode::SEE_OTHER);
    assert_eq!(
        f.request("GET", Some(&f.member.session_token), &[]).await.0,
        StatusCode::FORBIDDEN
    );
    let admin = Some(f.admin.session_token.as_str());
    assert_eq!(f.request("GET", admin, &[]).await.0, StatusCode::OK);
    let csrf = f.admin.csrf_token.clone();
    let mut fields = vec![
        ("_csrf", "bad"),
        ("revision", "0"),
        ("transport", "smtp"),
        ("host", "smtp.example.test"),
        ("port", "465"),
        ("security", "tls"),
        ("username", "sender@example.test"),
        ("password", SECRET),
        ("from_address", "sender@example.test"),
        ("from_name", "Sample Forum"),
        ("incoming_host", "pop.example.test"),
        ("incoming_port", "995"),
        ("test_to", "recipient@example.test"),
        ("operation", "save"),
    ];
    assert_eq!(
        f.request("POST", admin, &fields).await.0,
        StatusCode::FORBIDDEN
    );
    fields[0].1 = &csrf;
    let (status, body) = f.request("POST", admin, &fields).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Mail settings saved; account mail from now on uses them."));
    assert!(body.contains("An authorization code is saved"));
    let row = db::query("SELECT svalue FROM gonof_common_settings WHERE skey='mail.transport'")
        .fetch_one(f.services.db.executor())
        .await
        .unwrap();
    let json = db::text(&row, "svalue").unwrap();
    assert!(!json.contains(SECRET));
    assert!(json.contains("password_encrypted"));
    assert!(json.contains("pop.example.test"));
    assert_eq!(
        f.request("POST", admin, &fields).await.0,
        StatusCode::CONFLICT
    );
    fields[1].1 = "1";
    fields[7].1 = "";
    assert_eq!(f.request("POST", admin, &fields).await.0, StatusCode::OK);
    assert!(
        f.mail
            .settings(f.admin.member.actor())
            .await
            .unwrap()
            .has_password
    );
    fields[1].1 = "2";
    fields[3].1 = "smtp.other.test";
    assert_eq!(
        f.request("POST", admin, &fields).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let logs: Vec<String> =
        db::query("SELECT data FROM gonof_common_log WHERE type='mail.settings.save'")
            .fetch_all(f.services.db.executor())
            .await
            .unwrap()
            .iter()
            .map(|r| db::text(r, "data").unwrap())
            .collect();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|s| !s.contains(SECRET)));
    db::query("UPDATE gonof_common_member SET adminid=2,emailstatus=1 WHERE uid=?")
        .bind(f.member.member.uid)
        .execute(f.services.db.executor())
        .await
        .unwrap();
    assert_eq!(
        f.request("GET", Some(&f.member.session_token), &[]).await.0,
        StatusCode::FORBIDDEN
    );
}

/// The mail configuration uses the same administrator re-check as other admin pages: an administrator whose account
/// is being deleted can no longer read or write with an old session.
#[tokio::test]
async fn mail_settings_reject_an_admin_whose_account_is_closing() {
    let f = Fixture::new().await;
    let actor = f.admin.member.actor();
    assert!(f.mail.settings(actor).await.is_ok());
    db::query("UPDATE gonof_common_member SET account_state='deleting' WHERE uid=?")
        .bind(actor.uid)
        .execute(f.services.db.executor())
        .await
        .unwrap();
    let error = f.mail.settings(actor).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<identity::IdentityError>(),
        Some(identity::IdentityError::Unauthorized)
    ));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn smtp_failure_stays_retryable_and_revoked_tokens_never_send() {
    let f = Fixture::new().await;
    let settings = f
        .mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings: Fixture::settings(),
                password: SECRET.into(),
            },
        )
        .await
        .unwrap();
    let report = f.mail.deliver_pending(identity::now()).await.unwrap();
    assert_eq!((report.delivered, report.failed), (0, 1));
    let row=db::query("SELECT status,last_error,attempts FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification'").bind(f.member.member.uid).fetch_one(f.services.db.executor()).await.unwrap();
    assert_eq!(db::text(&row, "status").unwrap(), "pending");
    assert!(!db::text(&row, "last_error").unwrap().contains(SECRET));
    assert!(
        std::fs::read_dir(f.services.root.join(".local/mail"))
            .unwrap()
            .map(Result::unwrap)
            .all(|entry| entry.file_name() == "prepared"),
        "SMTP failure must not silently become a local delivery"
    );
    assert!(
        f.mail
            .test_settings(
                f.admin.member.actor(),
                MailInput {
                    settings: settings.clone(),
                    password: String::new()
                }
            )
            .await
            .is_err()
    );
    let error = f
        .mail
        .test_settings(
            f.admin.member.actor(),
            MailInput {
                settings: settings.clone(),
                password: String::new(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Wait at least a minute before sending another")
    );
    db::query(
        "UPDATE gonof_common_member_token SET revoked_at=? WHERE uid=? AND revoked_at IS NULL",
    )
    .bind(identity::now())
    .bind(f.member.member.uid)
    .execute(f.services.db.executor())
    .await
    .unwrap();
    let report = f.mail.deliver_pending(identity::now() + 30).await.unwrap();
    assert_eq!((report.delivered, report.skipped), (0, 1));
    let reason: String = db::query_scalar("SELECT last_error FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification' AND status='failed'")
        .bind(f.member.member.uid)
        .fetch_one(f.services.db.executor())
        .await
        .unwrap();
    assert_eq!(reason, "the mail token was used, revoked or issued again");
    let mut local = settings;
    local.transport = "local".into();
    f.mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings: local,
                password: String::new(),
            },
        )
        .await
        .unwrap();
    f.identity
        .request_password_reset("mailmember@example.test")
        .await
        .unwrap();
    let report = f.mail.deliver_pending(identity::now()).await.unwrap();
    assert_eq!(report.delivered, 1);
}

/// Automatic retries stop after 20 consecutive temporary failures; after the admin area
/// schedules a retry, counting restarts and the mail can be sent.
#[tokio::test]
async fn temporary_failures_stop_at_the_limit_and_an_admin_retry_sends_the_mail() {
    let f = Fixture::new().await;
    let settings = f
        .mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings: Fixture::settings(),
                password: SECRET.into(),
            },
        )
        .await
        .unwrap();
    let uid = f.member.member.uid;
    let db = &f.services.db;
    db::query("UPDATE gonof_common_outbox SET attempts=19 WHERE aggregate_id=? AND event_type='email_verification'")
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
    let report = f.mail.deliver_pending(identity::now()).await.unwrap();
    assert_eq!((report.delivered, report.failed), (0, 1));
    let row = db::query("SELECT eventid,status,payload,attempts,available_at,lease_until,last_error FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification'")
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(
        db::text(&row, "status").unwrap(),
        "failed",
        "automatic retries stop after the 20th failure"
    );
    let error = db::text(&row, "last_error").unwrap();
    assert!(
        gono_forum_web::outbox::is_exhausted(Some(&error)),
        "{error}"
    );
    assert!(!error.contains(SECRET));
    assert_ne!(
        db::text(&row, "payload").unwrap(),
        "{}",
        "it will be retried, so the token payload must stay"
    );
    gono_forum_web::administration::retry_task(
        db,
        f.admin.member.actor(),
        row.try_get("eventid").unwrap(),
        row.try_get("attempts").unwrap(),
        row.try_get("available_at").unwrap(),
        row.try_get("lease_until").unwrap(),
        "failed",
        "SMTP fixed",
    )
    .await
    .unwrap();
    let mut local = settings;
    local.transport = "local".into();
    f.mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings: local,
                password: String::new(),
            },
        )
        .await
        .unwrap();
    let report = f.mail.deliver_pending(identity::now()).await.unwrap();
    assert_eq!(
        report.delivered, 1,
        "after the admin retry the mail is sent as usual"
    );
}

/// Mail that still cannot be sent on retry must be terminated, not backed off forever.
///
/// An invalid recipient address belongs to this kind; failing to connect to the SMTP server is still a temporary
/// failure (see the previous test).
#[tokio::test]
async fn undeliverable_recipient_ends_the_mail_instead_of_retrying_forever() {
    let f = Fixture::new().await;
    f.mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings: Fixture::settings(),
                password: SECRET.into(),
            },
        )
        .await
        .unwrap();
    let uid = f.member.member.uid;
    let db = &f.services.db;
    let invalid = "no-at-sign.example.test";
    db::query("UPDATE gonof_common_member SET email=? WHERE uid=?")
        .bind(invalid)
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
    db::query(
        "UPDATE gonof_common_member_token SET email=? WHERE uid=? AND purpose='email_verification'",
    )
    .bind(invalid)
    .bind(uid)
    .execute(db.executor())
    .await
    .unwrap();
    let payload: String = db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification'")
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    let mut payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    payload["to"] = invalid.into();
    db::query("UPDATE gonof_common_outbox SET payload=? WHERE aggregate_id=? AND event_type='email_verification'")
        .bind(payload.to_string())
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
    let report = f.mail.deliver_pending(identity::now()).await.unwrap();
    assert_eq!((report.delivered, report.failed), (0, 1));
    let row = db::query("SELECT status,payload,last_error FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification'")
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(
        db::text(&row, "status").unwrap(),
        "failed",
        "an invalid recipient address must not go back to the retry queue"
    );
    assert_eq!(
        db::text(&row, "payload").unwrap(),
        "{}",
        "ended account mail must drop the raw token"
    );
    assert_eq!(
        db::text(&row, "last_error").unwrap(),
        "the recipient address is not valid; not retrying"
    );
    assert!(
        !f.services.root.join(".local/mail/prepared").exists()
            || std::fs::read_dir(f.services.root.join(".local/mail/prepared"))
                .unwrap()
                .next()
                .is_none(),
        "ended mail leaves no prepared draft"
    );
}

/// Links in mail bodies take the site URL, the same source as canonical.
///
/// The site settings page lets administrators change this URL, and when it is empty it must fall back to the
/// deployment config — this test watches exactly that fallback: the URL `Mailer` received at construction appears
/// as is in the delivered mail.
#[tokio::test]
async fn account_mail_links_use_the_site_address_it_was_given() {
    let f = Fixture::new().await;
    let mailbox = f.services.root.join(".local/mail");
    // The verification mail was queued at registration; read the body back after delivery.
    let report = f.mail.deliver_pending(identity::now() + 1).await.unwrap();
    assert!(
        report.claimed > 0,
        "registration must queue a verification mail"
    );

    let body = std::fs::read_dir(&mailbox)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "eml"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .find(|text| text.contains("/verify-email"))
        .expect("there must be a mail with a verification link");

    assert!(
        body.contains(&format!("{ORIGIN}/verify-email")),
        "mail links must point to the site address {ORIGIN}, body: {body}"
    );
    f.services.cleanup_ref().await.unwrap();
}

/// Takes an exclusive lock on this row from another connection; not getting it within 1 second counts as held by
/// another transaction.
async fn lockable(db: &db::Database, sql: &str, id: i64) -> bool {
    let mut connection = db.pool.acquire().await.unwrap();
    // The session-level lock wait was changed, so the connection is not returned to the pool.
    connection.close_on_drop();
    for statement in [
        "SET SESSION innodb_lock_wait_timeout=1",
        "START TRANSACTION",
    ] {
        gono_forum_web::db::raw_sql(statement)
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    let locked = gono_forum_web::db::query(sql)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await;
    gono_forum_web::db::raw_sql("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    match locked {
        Ok(row) => {
            assert!(row.is_some(), "{sql} found no row {id}");
            true
        }
        Err(error) => {
            assert!(error.to_string().contains("Lock wait timeout"), "{error}");
            false
        }
    }
}

// One SMTP send takes up to 15 seconds, during which neither the recipient's member row nor the row of the forum
// holding the content may be locked; otherwise that account's actions and the posts and replies in that forum and its
// child forums would all queue behind this mail. The fake SMTP server only accepts connections and
// never responds, so sending hangs in the handshake; meanwhile, lock from another connection.
#[tokio::test]
async fn smtp_submission_holds_no_member_or_forum_locks() {
    // Only checks MySQL row locks (SQLite queues writes as a whole).
    gono_forum_web::mysql_only!();
    let f = Fixture::new().await;
    let db = &f.services.db;
    let uid = f.member.member.uid;
    let now = identity::now();
    db::query("UPDATE gonof_common_member SET emailstatus=1,regdate=1 WHERE uid=?")
        .bind(uid)
        .execute(db.executor())
        .await
        .unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(9301,'mail-replier','mail-replier@example.test','fixture-hash',1,0,1)")
        .execute(db.executor())
        .await
        .unwrap();
    let mut wanted = preferences::get(db, f.member.member.actor()).await.unwrap();
    wanted.email[0] = true;
    preferences::save(db, f.member.member.actor(), &wanted)
        .await
        .unwrap();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let thread = writer
        .publish_at(
            uid,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Thread awaiting a reply mail".into(),
                body: "First post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now,
        )
        .await
        .unwrap();
    writer
        .reply_at(
            9301,
            NewReply {
                tid: thread.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Triggers a reply mail".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now + 1,
        )
        .await
        .unwrap();
    notifications::deliver_pending(db, now + 2).await.unwrap();
    let version: i64 =
        db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(db.executor())
            .await
            .unwrap();
    moderation::issue_sanction(
        db,
        f.admin.member.actor(),
        &SanctionRequest {
            uid,
            expected_version: version,
            kind: "warning".into(),
            hours: 0,
            reason: "Triggers a governance mail".into(),
            request_key: identity::random_token(),
        },
        now + 3,
    )
    .await
    .unwrap();
    let mut queued: Vec<String> = db::query_scalar("SELECT event_type FROM gonof_common_outbox WHERE aggregate_id=? AND status='pending' AND event_type IN ('email_verification','notification.email','sanction_issued')")
        .bind(uid)
        .fetch_all(db.executor())
        .await
        .unwrap();
    queued.sort();
    assert_eq!(
        queued,
        [
            "email_verification",
            "notification.email",
            "sanction_issued"
        ]
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = Fixture::settings();
    settings.port = listener.local_addr().unwrap().port();
    f.mail
        .save_settings(
            f.admin.member.actor(),
            MailInput {
                settings,
                password: SECRET.into(),
            },
        )
        .await
        .unwrap();
    let probe = async {
        let mut free = Vec::new();
        for _ in 0..3 {
            let (socket, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
                .await
                .expect("every mail must connect to SMTP")
                .unwrap();
            free.push((
                lockable(
                    db,
                    "SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE",
                    uid,
                )
                .await,
                lockable(
                    db,
                    "SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE",
                    fid,
                )
                .await,
            ));
            // Drop the connection so this send fails and is left for a retry.
            drop(socket);
        }
        free
    };
    let (report, free) = tokio::join!(f.mail.deliver_pending(now + 10), probe);
    let report = report.unwrap();
    assert_eq!((report.claimed, report.delivered, report.failed), (3, 0, 3));
    assert_eq!(
        free,
        [(true, true); 3],
        "while sending, (member row, forum row) must both be lockable at once"
    );
    f.services.cleanup_ref().await.unwrap();
}
