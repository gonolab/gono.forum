//! Invite code prefixes and the mail sending switch (the registration modes stay as they are, with no
//! separate invite code switch; the prefixes live in one settings key `settings.invite.prefixes`, and codes
//! starting with any prefix are not consumed and can be reused; turning off the mail sending switch
//! `settings.email.enabled` only stops sending, and the e-mail verification rules stay the same).
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    identity::{self, Actor, IdentityError},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9961;
const PASSWORD: &str = "Prefix-password-2026!";

struct Fixture {
    f: TestFixture,
    app: Router,
    identity: identity::Identity,
    admin: identity::SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,'prefix-admin','synthetic-hash','prefix-admin@example.test',1,1,1)")
            .bind(ADMIN)
            .execute(f.db.executor())
            .await
            .unwrap();
        let identity = identity::Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let admin = http_auth::login(&identity, &f.db, ADMIN).await;
        // Shares the same process cache handle with the save functions: the app's next request after a save sees
        // the new value.
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self {
            f,
            app,
            identity,
            admin,
        }
    }

    async fn update(&self, change: impl FnOnce(&mut runtime::SiteSettings)) {
        runtime::update(
            &self.f.db,
            &self.f.global,
            Actor {
                uid: ADMIN,
                credentials_version: self.admin.member.credentials_version,
                api_key: None,
            },
            "site.settings.save",
            change,
        )
        .await
        .unwrap();
    }

    async fn get(&self, path: &str, session: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(token) = session {
            request = request.header(header::COOKIE, format!("gonof_session={token}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn admin_post(
        &self,
        path: &str,
        fields: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session={}", self.admin.session_token),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn register(
        &self,
        name: &str,
        code: &str,
    ) -> Result<identity::SessionGrant, IdentityError> {
        self.identity
            .register(name, &format!("{name}@example.test"), PASSWORD, code)
            .await
    }

    async fn stored(&self, key: &str) -> String {
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(key)
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }

    /// The data of this member's registration log.
    async fn register_log(&self, uid: i64) -> serde_json::Value {
        let data: String = db::query_scalar(
            "SELECT data FROM gonof_common_log WHERE uid=? AND type='identity.register'",
        )
        .bind(uid)
        .fetch_one(self.f.db.executor())
        .await
        .unwrap();
        serde_json::from_str(&data).unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }

    async fn outbox_status(&self, uid: i64) -> Vec<String> {
        db::query_scalar("SELECT status FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='email_verification' ORDER BY eventid")
            .bind(uid)
            .fetch_all(self.f.db.executor())
            .await
            .unwrap()
    }
}

/// The whole "Registration & permissions" page form (an unsubmitted checkbox means unticked, so the whole page is
/// submitted together).
fn registration_form(prefixes: &str) -> [(&str, &str); 9] {
    [
        ("registration", "invite"),
        ("signup_ip_hourly_limit", "20"),
        ("invitations_enabled", "true"),
        ("invite_daily_limit", "5"),
        ("invite_active_limit", "5"),
        ("invite_ttl_days", "7"),
        ("invite_prefixes", prefixes),
        ("username_cooldown_days", "30"),
        ("username_history_days", "90"),
    ]
}

fn invalid(result: Result<identity::SessionGrant, IdentityError>) -> &'static str {
    match result {
        Err(IdentityError::Validation(message)) => message,
        Err(other) => panic!("expected a validation error: {other:?}"),
        Ok(grant) => panic!("registration should fail: {}", grant.member.username),
    }
}

/// Invite code prefixes in the admin "Registration & permissions" page: saving trims whitespace and drops blank
/// lines and duplicates, and a wrong format gives 422 and stays on the form; a code starting with a prefix can
/// register in both the invite-only and the open registration mode, is not consumed, can be reused and is
/// case-sensitive; when the table happens to contain this code it is still consumed as an invitation, and the
/// prefix is looked at after the invitation has been used; the registration log records which prefix let it
/// through.
#[tokio::test]
async fn invite_prefixes_admit_matching_codes_without_consuming_them() {
    let f = Fixture::new().await;
    let page = "/manage/settings/registration";
    // "\u{91d1}\u{5e01}\u{524d}\u{7f00}" is the Chinese for "coin prefix": four letters, but not ASCII ones.
    for bad in ["h", "1234567", "ho ph", "\u{91d1}\u{5e01}\u{524d}\u{7f00}"] {
        let (status, _, body) = f.admin_post(page, &registration_form(bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert!(body.contains("no duplicates, each 2 to 6 letters"), "{bad}");
    }
    assert_eq!(f.stored("settings.invite.prefixes").await, "");
    let (status, headers, _) = f
        .admin_post(page, &registration_form("  hophot \r\n\r\nhophot\nA-\n"))
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with(page)
    );
    assert_eq!(f.stored("settings.invite.prefixes").await, "hophot\nA-");
    let (status, body) = f.get(page, Some(&f.admin.session_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("name=\"invite_prefixes\""));
    assert!(body.contains("no duplicates, each 2–6 letters"));
    assert!(body.contains("hophot\nA-"));
    let (_, body) = f.get("/manage/invites", Some(&f.admin.session_token)).await;
    assert!(body.contains("data-invite-prefix-hint"));
    assert!(body.contains("2 prefixes are set."));

    // Invite-only mode: a code starting with a prefix can be used again and again without writing the invite
    // table.
    let first = f.register("prefix-one", "hophoto43saf242").await.unwrap();
    let second = f.register("prefix-two", "hophoto43saf242").await.unwrap();
    let third = f.register("prefix-three", "A-9x").await.unwrap();
    assert_eq!(f.count("SELECT COUNT(*) FROM gonof_common_invite").await, 0);
    for (grant, prefix) in [(&first, "hophot"), (&second, "hophot"), (&third, "A-")] {
        let log = f.register_log(grant.member.uid).await;
        assert_eq!(log["invite_prefix"], prefix);
        assert!(log["invite_id"].is_null() && log["inviter_uid"].is_null());
        assert!(
            !grant.member.email_verified,
            "a member registered by prefix still has to verify the email"
        );
    }
    for code in ["HOPHoto43saf242", "xhophoto", "a-9x", "hopho"] {
        assert_eq!(
            invalid(f.register("prefix-bad", code).await),
            "The invite code is invalid or has expired",
            "{code}"
        );
    }
    assert_eq!(
        invalid(f.register("prefix-empty", "").await),
        "Please enter a valid invite code"
    );

    // The table happens to contain this code: it is consumed as an invitation first; used again afterwards, it
    // passes by the prefix.
    let invite = f
        .identity
        .generate_invite(ADMIN, &identity::random_token())
        .await
        .unwrap();
    let head = invite.code[..4].to_owned();
    assert_eq!(
        invalid(f.register("prefix-unrelated", &format!("{head}zz")).await),
        "The invite code is invalid or has expired",
        "without this prefix, a code missing from the table does not pass"
    );
    f.update(|s| s.invite.prefixes.push(head.clone())).await;
    let consumer = f.register("prefix-consumer", &invite.code).await.unwrap();
    let log = f.register_log(consumer.member.uid).await;
    assert_eq!(log["inviter_uid"], ADMIN);
    assert_eq!(log["invite_id"], invite.id);
    assert!(log["invite_prefix"].is_null());
    let used: Option<i64> =
        db::query_scalar("SELECT fuid FROM gonof_common_invite WHERE id=? AND status='used'")
            .bind(invite.id)
            .fetch_one(f.f.db.executor())
            .await
            .unwrap();
    assert_eq!(used, Some(consumer.member.uid));
    let again = f.register("prefix-again", &invite.code).await.unwrap();
    let log = f.register_log(again.member.uid).await;
    assert_eq!(log["invite_prefix"], head.as_str());
    assert!(log["invite_id"].is_null() && log["inviter_uid"].is_null());

    // Open registration: the code may be left empty; if filled in it is still checked.
    f.update(|s| s.signup.mode = "open".into()).await;
    f.register("prefix-open", "").await.unwrap();
    f.register("prefix-open-code", "hophothello").await.unwrap();
    assert_eq!(
        invalid(f.register("prefix-open-bad", "nothing-here").await),
        "The invite code is invalid or has expired"
    );
    // Registration closed: prefixes do not let anyone through either.
    f.update(|s| s.signup.mode = "closed".into()).await;
    assert!(matches!(
        f.register("prefix-closed", "hophothello").await,
        Err(IdentityError::Conflict(_))
    ));
    f.f.cleanup().await.unwrap();
}

/// The mail sending switch in the admin "Mail settings" page: when off, delivery skips whole batches and events stay
/// in the queue, the member stays unverified and still has to verify before posting, and pages that send mail
/// say sending is paused; after turning it back on the queued mail is sent as usual.
#[tokio::test]
async fn mail_switch_pauses_delivery_without_lifting_verification() {
    let f = Fixture::new().await;
    f.update(|s| s.signup.mode = "open".into()).await;
    let (_, body) = f.get("/manage/mail", Some(&f.admin.session_token)).await;
    assert!(body.contains("data-mail-switch") && body.contains("Turn off sending"));

    let (status, headers, _) = f
        .admin_post("/manage/mail/switch", &[("enabled", "off")])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/manage/mail?saved=off");
    assert_eq!(f.stored("settings.email.enabled").await, "false");
    let (_, body) = f
        .get("/manage/mail?saved=off", Some(&f.admin.session_token))
        .await;
    assert!(body.contains("Sending is off;") && body.contains("Turn on sending"));
    assert_eq!(
        f.count("SELECT COUNT(*) FROM gonof_common_log WHERE type='mail.switch'")
            .await,
        1
    );

    let member = f.register("paused-member", "").await.unwrap();
    assert_eq!(f.outbox_status(member.member.uid).await, ["pending"]);
    let mailbox = f.f.root.join("switch-mail");
    let report = gono_forum_web::test_support::deliver_mail(
        &f.f.db,
        &mailbox,
        TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(report.claimed, 0, "nothing is claimed while sending is off");
    assert_eq!(f.outbox_status(member.member.uid).await, ["pending"]);
    // The verification rules are unchanged: the member is still unverified and cannot post.
    let session = f
        .identity
        .session(&member.session_token)
        .await
        .unwrap()
        .unwrap();
    assert!(!session.member.email_verified && !session.member.can_contribute());
    let (_, body) = f.get("/verify-email", Some(&member.session_token)).await;
    assert!(
        body.contains("data-mail-paused"),
        "the verification page says sending is paused"
    );
    let (_, body) = f.get("/forgot-password", None).await;
    assert!(
        body.contains("data-mail-paused"),
        "the password reset page says sending is paused"
    );
    let (_, body) = f.get("/my/profile", Some(&member.session_token)).await;
    assert!(
        body.contains("data-mail-paused"),
        "the email change form says sending is paused"
    );
    for path in ["/appeals", "/data/access"] {
        let (status, body) = f.get(path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            body.contains("data-mail-paused"),
            "{path} says sending is paused"
        );
    }

    let (status, headers, _) = f
        .admin_post("/manage/mail/switch", &[("enabled", "on")])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/manage/mail?saved=on");
    assert_eq!(f.stored("settings.email.enabled").await, "true");
    let (_, body) = f.get("/forgot-password", None).await;
    assert!(!body.contains("data-mail-paused"));
    let report = gono_forum_web::test_support::deliver_mail(
        &f.f.db,
        &mailbox,
        TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert!(
        report.delivered >= 1,
        "the queued verification mail goes out once sending is back on"
    );
    assert_eq!(f.outbox_status(member.member.uid).await, ["sent"]);
    f.f.cleanup().await.unwrap();
}
