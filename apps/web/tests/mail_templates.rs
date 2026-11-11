use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use gono_forum_web::{
    db,
    identity::{self, Actor, Identity},
    mail::{
        Mailer,
        templates::{self, MailTemplate},
    },
    media::Media,
    moderation::{self, AppealAccess, SanctionRequest},
    privacy,
    test_support::TestFixture,
};
use std::collections::BTreeMap;

const ORIGIN: &str = "https://forum.example.test";
const PASSWORD: &str = "Mail-template-test-password";
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

struct Fixture {
    services: TestFixture,
    identity: Identity,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let password = Argon2::default()
            .hash_password(
                PASSWORD.as_bytes(),
                &SaltString::encode_b64(b"template-fixture-salt").unwrap(),
            )
            .unwrap()
            .to_string();
        for (uid, role) in [(8101, 1), (8102, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,?,?,0,?,1)")
                .bind(uid).bind(format!("mail-{uid}")).bind(&password).bind(format!("mail-{uid}@example.test")).bind(role)
                .execute(services.db.executor()).await.unwrap();
        }
        let identity = Identity::new(
            services.db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        Self { services, identity }
    }
    /// Mail links use the forum URL from the admin area: first write it as `origin`, then deliver with a freshly
    /// created cache handle, which reads exactly this value.
    async fn mailer(&self, origin: &str) -> Mailer {
        gono_forum_web::test_support::set_site_url(&self.services.db, origin)
            .await
            .unwrap();
        Mailer::new(
            self.services.db.clone(),
            Default::default(),
            self.services.root.join(".local/mail"),
            gono_forum_web::test_support::detached_global(),
        )
    }
    async fn template(&self, kind: &str, marker: &str) -> MailTemplate {
        templates::save(
            &self.services.db,
            actor(8101),
            kind,
            MailTemplate {
                subject: format!("{marker} {kind}"),
                body: format!("{marker} {{{{username}}}}\n{{{{link}}}}\n{{{{site_name}}}}"),
            },
            false,
        )
        .await
        .unwrap()
    }
    /// Data request e-mail tickets are sent only to frozen members: freeze 8102 first, then apply.
    async fn privacy_access(&self, now: i64) {
        let db = &self.services.db;
        db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=8102")
            .execute(db.executor())
            .await
            .unwrap();
        privacy::request_access(db, "mail-8102@example.test", now)
            .await
            .unwrap();
    }
    async fn events(&self) -> Vec<(i64, String, serde_json::Value)> {
        db::query("SELECT eventid,event_type,payload FROM gonof_common_outbox WHERE aggregate_type='member' AND aggregate_id=8102 ORDER BY eventid")
            .fetch_all(self.services.db.executor()).await.unwrap().into_iter().map(|row| {
                (row.try_get("eventid").unwrap(),db::text(&row,"event_type").unwrap(),serde_json::from_str(&db::text(&row,"payload").unwrap()).unwrap())
            }).collect()
    }
    /// Sets the events back to pending. `events` is a snapshot from before the first delivery: when delivery is
    /// abandoned the token in the payload has been cleared, so it is put back from the snapshot here, making every
    /// later round go through the full eligibility check again instead of skipping because the payload is empty.
    async fn requeue(&self, now: i64, events: &[(i64, String, serde_json::Value)]) {
        for (id, _, payload) in events {
            db::query("UPDATE gonof_common_outbox SET status='pending',payload=?,available_at=?,lease_token=NULL,lease_until=0 WHERE eventid=?")
                .bind(payload.to_string()).bind(now).bind(id).execute(self.services.db.executor()).await.unwrap();
        }
    }
    /// Simulates "sent, then crashed before confirming": the UPDATE that confirms the send fails and the transaction
    /// rolls back, so the draft and the payload with the token are both still there, just like being reclaimed after
    /// a real crash. CREATE TRIGGER cannot go through the prepared statement protocol.
    async fn crash_before_ack(&self, crash: bool) {
        gono_forum_web::test_support::drop_trigger_if_exists(
            &self.services.db,
            "gonof_test_crash_before_ack",
        )
        .await
        .unwrap();
        if crash {
            gono_forum_web::test_support::fail_when(
                &self.services.db,
                "gonof_test_crash_before_ack",
                "BEFORE UPDATE",
                "gonof_common_outbox",
                "NEW.status='sent'",
                "simulated crash before ack",
            )
            .await
            .unwrap();
        }
    }
    async fn payload(&self, id: i64) -> String {
        db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE eventid=?")
            .bind(id)
            .fetch_one(self.services.db.executor())
            .await
            .unwrap()
    }
    fn draft_exists(&self, id: i64) -> bool {
        self.services
            .root
            .join(format!(".local/mail/prepared/{id}.eml"))
            .exists()
    }
}

#[tokio::test]
async fn identity_and_governance_render_at_delivery_and_keep_retry_drafts_private_and_stable() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let kinds = [
        "email_verification",
        "password_reset",
        "email_change",
        "appeal_access",
        "privacy_access",
        "sanction_issued",
        "sanction_ended",
        "appeal_result",
    ];
    for kind in kinds {
        let saved = f.template(kind, "VERSION_ONE").await;
        assert!(saved.subject.contains("VERSION_ONE"));
    }
    let login = f.identity.login("mail-8102", PASSWORD).await.unwrap();
    f.identity.resend_verification(8102).await.unwrap();
    f.identity
        .request_password_reset("mail-8102@example.test")
        .await
        .unwrap();
    f.identity
        .request_email_change(&login.session_token, PASSWORD, "changed@example.test")
        .await
        .unwrap();
    f.privacy_access(now).await;
    // The remaining events are still generated for unfrozen members; later everything is frozen and re-checked.
    db::query("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=8102")
        .execute(db.executor())
        .await
        .unwrap();
    let sanction = moderation::issue_sanction(
        db,
        actor(8101),
        &SanctionRequest {
            uid: 8102,
            expected_version: 1,
            kind: "warning".into(),
            hours: 0,
            reason: "Rule reason for the fixed mail events".into(),
            request_key: identity::random_token(),
        },
        now,
    )
    .await
    .unwrap();
    moderation::request_appeal_access(db, "mail-8102@example.test", now)
        .await
        .unwrap();
    let appeal = moderation::submit_appeal(
        db,
        &AppealAccess::Session(actor(8102)),
        sanction,
        "Please review this decision",
        &identity::random_token(),
        now,
    )
    .await
    .unwrap();
    moderation::resolve_appeal(
        db,
        actor(8101),
        appeal,
        "rejected",
        "Reviewed; the decision stands",
        now,
    )
    .await
    .unwrap();
    moderation::revoke_sanction(db, actor(8101), sanction, "This warning has ended", now)
        .await
        .unwrap();
    let events = f.events().await;
    assert_eq!(events.len(), 8);
    for (_, kind, payload) in &events {
        assert!(kinds.contains(&kind.as_str()));
        // The queued payload does not carry the template version: the wording is finalized only at the first
        // delivery attempt.
        assert!(
            payload.get("template_revision").is_none(),
            "{kind}: {payload}"
        );
    }
    // Every mail is really sent (written to the local mailbox), but confirmation fails: exactly the state drafts are
    // meant to protect.
    f.crash_before_ack(true).await;
    let sent = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 5)
        .await
        .unwrap();
    assert_eq!((sent.delivered, sent.failed, sent.skipped), (0, 8, 0));
    let mailbox = f.services.root.join(".local/mail");
    let original: Vec<_> = events
        .iter()
        .map(|(id, _, _)| std::fs::read(mailbox.join(format!("{id}.eml"))).unwrap())
        .collect();
    for ((id, kind, _), message) in events.iter().zip(&original) {
        let text = String::from_utf8_lossy(message);
        assert!(text.contains(&format!("VERSION_ONE {kind}")));
        assert!(!text.contains("VERSION_TWO"));
        assert!(text.contains(ORIGIN));
        let prepared = std::fs::read(mailbox.join(format!("prepared/{id}.eml"))).unwrap();
        let draft: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
        assert!(draft["subject"].as_str().unwrap().contains("VERSION_ONE"));
        assert!(draft["content_kind"].is_null());
        assert!(
            !String::from_utf8_lossy(&prepared).contains("@example.test"),
            "prepared data must not capture the recipient"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(mailbox.join(format!("prepared/{id}.eml")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    // Changing the template after the first delivery, or even restoring the built-in wording: private drafts already
    // on disk must be resent unchanged.
    // The guarantee "retries after the first delivery are byte-identical" relies on the draft files, not on a
    // database snapshot.
    for kind in kinds {
        f.template(kind, "VERSION_TWO").await;
    }
    let reset = templates::save(
        db,
        actor(8101),
        "email_verification",
        MailTemplate::default(),
        true,
    )
    .await
    .unwrap();
    assert!(reset.subject.is_empty() && reset.body.is_empty());
    // Publication followed by a crash before ACK: current links would change,
    // but an event whose private draft already exists must publish identical bytes.
    f.requeue(now + 10, &events).await;
    let retry = f
        .mailer("https://moved.example.test")
        .await
        .deliver_pending(now + 10)
        .await
        .unwrap();
    assert_eq!((retry.delivered, retry.failed, retry.skipped), (0, 8, 0));
    for ((id, _, _), expected) in events.iter().zip(&original) {
        assert_eq!(
            std::fs::read(mailbox.join(format!("{id}.eml"))).unwrap(),
            *expected
        );
    }
    f.requeue(now + 20, &events).await;
    db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=8102")
        .execute(db.executor())
        .await
        .unwrap();
    let frozen = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 20)
        .await
        .unwrap();
    assert_eq!(
        (frozen.delivered, frozen.failed, frozen.skipped),
        (0, 5, 3),
        "frozen accounts retain only privacy, appeals and governance mail"
    );
    // The three account mails whose delivery was abandoned have their token plaintext and drafts cleared on the spot;
    // the other five were only unconfirmed and stay as they are, waiting for a retry.
    for (id, kind, _) in &events {
        let abandoned = matches!(
            kind.as_str(),
            "email_verification" | "password_reset" | "email_change"
        );
        assert_eq!(f.payload(*id).await == "{}", abandoned, "{kind}");
        assert_eq!(f.draft_exists(*id), !abandoned, "{kind}");
    }
    db::query("UPDATE gonof_common_member SET email='changed@example.test' WHERE uid=8102")
        .execute(db.executor())
        .await
        .unwrap();
    f.requeue(now + 30, &events).await;
    let changed = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 30)
        .await
        .unwrap();
    assert_eq!(
        (changed.delivered, changed.skipped),
        (0, 8),
        "drafts never bypass current mailbox and token checks"
    );
    db::query("UPDATE gonof_common_member SET email='mail-8102@example.test',`freeze`=0,account_state='deleting' WHERE uid=8102").execute(db.executor()).await.unwrap();
    f.requeue(now + 40, &events).await;
    let deleting = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 40)
        .await
        .unwrap();
    assert_eq!((deleting.delivered, deleting.skipped), (0, 8));
    // All finished: not a single draft remains, and the payloads of the five account mails no longer contain tokens.
    for (id, kind, payload) in &events {
        assert!(!f.draft_exists(*id), "{kind}");
        if payload.get("token").is_some() {
            assert_eq!(f.payload(*id).await, "{}", "{kind}");
        }
    }
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn template_reset_falls_back_to_builtin_and_privacy_access_uses_its_own_message() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let value = f.template("privacy_access", "CUSTOM_DATA").await;
    // Non-administrators cannot save; there is no optimistic lock, so saving again with the old value is not a
    // conflict.
    assert!(
        templates::save(db, actor(8102), "privacy_access", value.clone(), false)
            .await
            .is_err()
    );
    templates::save(db, actor(8101), "privacy_access", value.clone(), false)
        .await
        .unwrap();
    let values = BTreeMap::from([
        ("username".into(), "user".into()),
        ("link".into(), format!("{ORIGIN}/data/access")),
        ("site_name".into(), "Sample Forum".into()),
    ]);
    // The custom template takes effect; after restoring the built-in wording, the same call falls back to it.
    let custom = templates::render(
        db,
        "privacy_access",
        &values,
        "default subject",
        "default body",
    )
    .await
    .unwrap();
    assert!(custom.0.contains("CUSTOM_DATA"));
    let reset = templates::save(db, actor(8101), "privacy_access", value, true)
        .await
        .unwrap();
    assert!(reset.subject.is_empty() && reset.body.is_empty());
    assert_eq!(
        templates::render(
            db,
            "privacy_access",
            &values,
            "default subject",
            "default body"
        )
        .await
        .unwrap(),
        ("default subject".into(), "default body".into())
    );
    // Only spaces also counts as "not customized": otherwise one formatting mistake would keep this kind of mail from
    // ever being sent.
    templates::save(
        db,
        actor(8101),
        "privacy_access",
        MailTemplate {
            subject: "   ".into(),
            body: " \n ".into(),
        },
        true,
    )
    .await
    .unwrap();
    // A row whose subject and body are only whitespace (written by hand; templates are rows of
    // gonof_common_localized_text) still counts as the built-in text.
    db::query(
        "INSERT INTO gonof_common_localized_text(kind,language,title,body,updated_at) VALUES('mail.privacy_access','en','   ',' ',1)",
    )
    .execute(db.executor())
    .await
    .unwrap();
    assert_eq!(
        templates::render(
            db,
            "privacy_access",
            &values,
            "default subject",
            "default body"
        )
        .await
        .unwrap(),
        ("default subject".into(), "default body".into())
    );
    assert!(
        templates::render(db, "unsupported", &values, "subject", "body")
            .await
            .is_err()
    );
    let now = identity::now();
    f.privacy_access(now).await;
    let events = f.events().await;
    assert_eq!(events.len(), 1);
    assert!(events[0].2.get("template_revision").is_none());
    assert_eq!(
        f.mailer(ORIGIN)
            .await
            .deliver_pending(now + 1)
            .await
            .unwrap()
            .delivered,
        1
    );
    let body = std::fs::read_to_string(
        f.services
            .root
            .join(format!(".local/mail/{}.eml", events[0].0)),
    )
    .unwrap();
    assert!(body.contains("Subject: [gonof] Confirm your email for a data request"));
    assert!(body.contains("/data/access?token="));
    assert!(body.contains("only allows handling your own data requests"));
    assert!(!body.contains("new email"));
    assert!(!body.contains("CUSTOM_DATA"));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn email_brand_binding_is_used_once_and_does_not_change_prepared_retry_mime() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let media = Media::new(
        db.clone(),
        f.services.config.media.clone(),
        f.services.root.clone(),
    );
    // Ready synthetic renditions are sufficient here: mail only emits the guarded
    // public image route, while image storage/decoding has its own integration tests.
    let mut ids = Vec::new();
    for index in 0..2 {
        let key = identity::random_token();
        let id = db::query(
            "INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(8101,?,?)",
        )
        .bind(&key)
        .bind(&key)
        .execute(db.executor())
        .await
        .unwrap()
        .last_insert_id() as i64;
        db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,width,height,description,alt_text,dateline,updated_at,checked_at,lifecycle,scan_status,scan_detail) VALUES(?,'brand.png',100,'image/png',?,?,'test-brand',32,32,'','Sample Forum',1,1,1,'ready','clean','')")
            .bind(id).bind(&key).bind(format!("synthetic/brand-{index}.png")).execute(db.executor()).await.unwrap();
        ids.push(id);
    }
    media
        .set_brand(&f.services.global, actor(8101), "email", Some(ids[0]), 0)
        .await
        .unwrap();
    templates::save(
        db,
        actor(8101),
        "privacy_access",
        MailTemplate {
            subject: "Brand mail test".into(),
            body: "<script>alert('x')</script> & {{username}}\n{{link}}".into(),
        },
        false,
    )
    .await
    .unwrap();
    let now = identity::now();
    f.privacy_access(now).await;
    f.crash_before_ack(true).await;
    assert_eq!(
        f.mailer(ORIGIN)
            .await
            .deliver_pending(now + 1)
            .await
            .unwrap()
            .failed,
        1
    );
    let events = f.events().await;
    let id = events[0].0;
    let path = f.services.root.join(format!(".local/mail/{id}.eml"));
    let original = std::fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("multipart/alternative"));
    let draft: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            f.services
                .root
                .join(format!(".local/mail/prepared/{id}.eml")),
        )
        .unwrap(),
    )
    .unwrap();
    let html = draft["html"].as_str().unwrap();
    assert!(html.contains(&format!("{ORIGIN}/brand/email/{}/source", ids[0])));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    media
        .set_brand(&f.services.global, actor(8101), "email", Some(ids[1]), 1)
        .await
        .unwrap();
    f.requeue(now + 10, &events).await;
    assert_eq!(
        f.mailer("https://moved.example.test")
            .await
            .deliver_pending(now + 10)
            .await
            .unwrap()
            .failed,
        1
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "retry keeps the original image URL, HTML and MIME bytes"
    );
    f.crash_before_ack(false).await;
    media
        .set_brand(&f.services.global, actor(8101), "email", None, 2)
        .await
        .unwrap();
    // The new application revoked the old ticket: the old event is abandoned this time and the new event is sent.
    f.privacy_access(now + 61).await;
    let report = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 62)
        .await
        .unwrap();
    assert_eq!((report.delivered, report.skipped), (1, 1));
    let events = f.events().await;
    for (id, _, _) in &events {
        assert_eq!(
            f.payload(*id).await,
            "{}",
            "no token is left in the payload once finished"
        );
        assert!(!f.draft_exists(*id), "no draft is left once finished");
    }
    let plain = std::fs::read_to_string(
        f.services
            .root
            .join(format!(".local/mail/{}.eml", events.last().unwrap().0)),
    )
    .unwrap();
    assert!(plain.contains("Content-Type: text/plain"));
    assert!(!plain.contains("multipart/alternative"));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn editing_a_template_before_the_first_attempt_changes_the_queued_mail() {
    let f = Fixture::new().await;
    let now = identity::now();
    f.template("privacy_access", "BEFORE_EDIT").await;
    f.privacy_access(now).await;
    // There is no history snapshot: the wording is finalized at the **first delivery attempt**, not inside the
    // enqueue transaction.
    // This is intended, and pinned as a contract so it is not later "fixed" as a bug.
    f.template("privacy_access", "AFTER_EDIT").await;
    let sent = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 1)
        .await
        .unwrap();
    assert_eq!((sent.delivered, sent.failed, sent.skipped), (1, 0, 0));
    let events = f.events().await;
    let body = std::fs::read_to_string(
        f.services
            .root
            .join(format!(".local/mail/{}.eml", events[0].0)),
    )
    .unwrap();
    assert!(body.contains("AFTER_EDIT"), "{body}");
    assert!(!body.contains("BEFORE_EDIT"));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn a_stored_template_that_no_longer_validates_blocks_delivery_until_reset() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    f.privacy_access(now).await;
    // Write straight into the database bypassing the form: the body lost {{link}}, so validate fails.
    db::query("INSERT INTO gonof_common_localized_text(kind,language,title,body,updated_at) VALUES('mail.privacy_access','en','Bad template',?,1)")
        .bind("Body without the action link")
        .execute(db.executor())
        .await
        .unwrap();
    // Existing dirty data is not silently let through: rendering fails and the event goes back to pending to wait for
    // a backoff retry, not to failed.
    // The cost is that all pending mails of this kind are blocked — a direct consequence of having no snapshot.
    let blocked = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 1)
        .await
        .unwrap();
    assert_eq!(
        (blocked.delivered, blocked.failed, blocked.skipped),
        (0, 1, 0)
    );
    let (status, error): (String, Option<String>) = {
        let row =
            db::query("SELECT status,last_error FROM gonof_common_outbox WHERE aggregate_id=8102")
                .fetch_one(db.executor())
                .await
                .unwrap();
        (
            db::text(&row, "status").unwrap(),
            row.try_get("last_error").unwrap(),
        )
    };
    assert_eq!(status, "pending");
    assert!(
        error.is_some_and(|text| text.contains("link")),
        "the failure reason must say the template is at fault"
    );

    // Operations lever: one click on "Restore built-in text" in the admin area resolves it.
    templates::save(
        db,
        actor(8101),
        "privacy_access",
        MailTemplate::default(),
        true,
    )
    .await
    .unwrap();
    let events = f.events().await;
    f.requeue(now + 10, &events).await;
    let sent = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 10)
        .await
        .unwrap();
    assert_eq!((sent.delivered, sent.failed, sent.skipped), (1, 0, 0));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn missing_template_rows_fall_back_to_the_builtin_text() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    f.template("privacy_access", "CUSTOM").await;
    // Incomplete migration seeding, or someone cleared the rows by hand: degrade to the built-in wording instead of
    // killing this kind of mail.
    db::query("DELETE FROM gonof_common_localized_text WHERE kind='mail.privacy_access'")
        .execute(db.executor())
        .await
        .unwrap();
    f.privacy_access(now).await;
    let sent = f
        .mailer(ORIGIN)
        .await
        .deliver_pending(now + 1)
        .await
        .unwrap();
    assert_eq!((sent.delivered, sent.failed, sent.skipped), (1, 0, 0));
    let events = f.events().await;
    let body = std::fs::read_to_string(
        f.services
            .root
            .join(format!(".local/mail/{}.eml", events[0].0)),
    )
    .unwrap();
    assert!(body.contains("Subject: [gonof] Confirm your email for a data request"));
    assert!(!body.contains("CUSTOM"));
    f.services.cleanup_ref().await.unwrap();
}
