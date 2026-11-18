use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    identity::{self, Actor, Identity, IdentityError},
    moderation::{self as gov, AppealAccess, SanctionRequest},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Appeal-test-password";
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
/// The seeded appeal window `settings.governance.appeal_window_days` (30 days).
const APPEAL_WINDOW: i64 = 30 * 86_400;

struct Fixture {
    services: TestFixture,
    app: Router,
    sessions: Vec<String>,
    csrf_tokens: Vec<String>,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        content::seed_forums(&services.db).await.unwrap();
        let hash = Argon2::default()
            .hash_password(
                PASSWORD.as_bytes(),
                &SaltString::encode_b64(b"appeal-test-salt").unwrap(),
            )
            .unwrap()
            .to_string();
        let identity = Identity::new(
            services.db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = vec![];
        let mut csrf_tokens = vec![];
        for (uid, role) in [(1, 1), (2, 0), (3, 0), (4, 3), (5, 1)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,?,?,1,?,1)").bind(uid).bind(format!("appeal-{uid}")).bind(&hash).bind(format!("appeal-{uid}@example.test")).bind(role).execute(services.db.executor()).await.unwrap();
            let grant = identity
                .login(&format!("appeal-{uid}"), PASSWORD)
                .await
                .unwrap();
            sessions.push(grant.session_token);
            csrf_tokens.push(grant.csrf_token);
        }
        let app = router::build(services.config.clone(), &services.root, services.db.clone());
        Self {
            services,
            app,
            sessions,
            csrf_tokens,
        }
    }
    fn identity(&self) -> Identity {
        Identity::new(
            self.services.db.clone(),
            self.services.config.identity.clone(),
            self.services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        )
    }
    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.services.db.executor())
            .await
            .unwrap()
    }
    async fn sanction(&self, uid: i64, kind: &str, now: i64) -> i64 {
        let version: i64 =
            db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_one(self.services.db.executor())
                .await
                .unwrap();
        gov::issue_sanction(
            &self.services.db,
            actor(1),
            &SanctionRequest {
                uid,
                kind: kind.into(),
                hours: if kind == "freeze" { 1 } else { 0 },
                reason: "Broke the test community rules".into(),
                expected_version: version,
                request_key: identity::random_token(),
            },
            now,
        )
        .await
        .unwrap()
    }
    async fn token(&self, uid: i64, now: i64) -> String {
        gov::request_appeal_access(
            &self.services.db,
            &format!("appeal-{uid}@example.test"),
            now,
        )
        .await
        .unwrap();
        let payload:String=db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE aggregate_id=? AND event_type='appeal_access' ORDER BY eventid DESC LIMIT 1").bind(uid).fetch_one(self.services.db.executor()).await.unwrap();
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["token"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn call(
        &self,
        cookie: &str,
        method: &str,
        path: &str,
        fields: Vec<(&str, String)>,
        origin: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, cookie);
        let body = if method == "POST" {
            req = req
                .header(header::ORIGIN, origin)
                .header(
                    header::HOST,
                    gono_forum_web::test_support::host_of(
                        gono_forum_web::test_support::TEST_SITE_URL,
                    ),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields)
                .finish()
        } else {
            String::new()
        };
        let response = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, headers, body)
    }
}

/// The application statements one appeal read runs.
async fn appeal_statements<F: std::future::Future>(future: F) -> (F::Output, usize) {
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    let profile = std::sync::Arc::new(RequestProfile::new());
    let output = trace::scope(profile.clone(), future).await;
    let count = profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .count();
    (output, count)
}

/// Reading the appeal list or one case writes nothing, so it runs in a read-only transaction without locks: it does
/// not wait for another transaction holding the member's row (on SQLite, the only writer), and a list page reads its
/// sanctions and their appeals in the same number of statements however many records it shows.
#[tokio::test]
async fn appeal_pages_read_without_locks_in_a_fixed_number_of_statements() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let access = AppealAccess::Session(actor(3));
    let first = f.sanction(3, "warning", now).await;
    for n in 1..6 {
        f.sanction(3, "warning", now + n).await;
    }
    gov::submit_appeal(
        db,
        &access,
        first,
        "The warning was about another member",
        &identity::random_token(),
        now + 10,
    )
    .await
    .unwrap();
    // Another transaction holds the member's row (and, on SQLite, the write lock): reading still goes through.
    let mut holder = db.begin_write().await.unwrap();
    db::query("SELECT uid FROM gonof_common_member WHERE uid=3 FOR UPDATE")
        .fetch_one(db.observe(&mut *holder))
        .await
        .unwrap();
    let wait = std::time::Duration::from_secs(3);
    let listed = tokio::time::timeout(wait, gov::appeal_cases(db, &access, 0, now + 11)).await;
    let case = tokio::time::timeout(wait, gov::appeal_case(db, &access, first, now + 11)).await;
    holder.rollback().await.unwrap();
    let cases = listed.expect("the list does not wait").unwrap();
    assert_eq!(cases.len(), 6);
    for case in &cases {
        assert_eq!(case.appeal.is_some(), case.sanction.id == first);
        assert_eq!(case.can_submit, case.sanction.id != first);
    }
    let case = case.expect("a case does not wait").unwrap();
    assert!(case.appeal.is_some() && !case.can_submit);
    // One record or six: the same statements.
    let other = AppealAccess::Session(actor(2));
    let foreign = f.sanction(2, "warning", now + 12).await;
    let (one, single) = appeal_statements(gov::appeal_cases(db, &other, 0, now + 13)).await;
    assert_eq!(one.unwrap().len(), 1);
    let (six, many) = appeal_statements(gov::appeal_cases(db, &access, 0, now + 13)).await;
    assert_eq!(six.unwrap().len(), 6);
    assert_eq!(
        single, many,
        "statements do not grow with the records shown"
    );
    // A case of someone else is still not found.
    assert!(
        gov::appeal_case(db, &access, foreign, now + 14)
            .await
            .is_err()
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn frozen_access_rotates_and_is_limited_to_owned_appeals() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let sanction = f.sanction(2, "freeze", now).await;
    assert!(matches!(
        f.identity().login("appeal-2", PASSWORD).await,
        Err(IdentityError::Suspended(_))
    ));
    assert!(
        f.identity()
            .session(&f.sessions[1])
            .await
            .unwrap()
            .is_none()
    );
    gov::request_appeal_access(db, "missing@example.test", now)
        .await
        .unwrap();
    let raw = f.token(2, now + 1).await;
    gov::check_appeal_access(db, &raw, now + 1).await.unwrap();
    gov::check_appeal_access(db, &raw, now + 1).await.unwrap();
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE purpose='appeal_access' AND consumed_at IS NOT NULL").await,0);
    assert!(!f.identity().verification_status(&raw).await.unwrap());
    assert!(!f.identity().password_reset_status(&raw).await.unwrap());
    assert!(
        gov::appeal_cases(db, &AppealAccess::Token(raw.clone()), 0, now + 1)
            .await
            .is_err()
    );
    let (cookie, ttl) = gov::exchange_appeal_access(db, &raw, now + 2)
        .await
        .unwrap();
    assert!(ttl <= 3600);
    assert_ne!(raw, cookie);
    assert!(gov::check_appeal_access(db, &raw, now + 2).await.is_err());
    assert!(
        gov::exchange_appeal_access(db, &cookie, now + 2)
            .await
            .is_err()
    );
    let access = AppealAccess::Token(cookie);
    assert!(
        gov::appeal_case(db, &AppealAccess::Session(actor(3)), sanction, now + 3)
            .await
            .is_err()
    );
    let foreign = f.sanction(3, "warning", now).await;
    assert!(
        gov::submit_appeal(
            db,
            &access,
            foreign,
            "posing as another member",
            &identity::random_token(),
            now + 3
        )
        .await
        .is_err()
    );
    let key = identity::random_token();
    let id = gov::submit_appeal(
        db,
        &access,
        sanction,
        "please review the facts",
        &key,
        now + 3,
    )
    .await
    .unwrap();
    assert_eq!(
        gov::submit_appeal(
            db,
            &access,
            sanction,
            "please review the facts",
            &key,
            now + 4
        )
        .await
        .unwrap(),
        id
    );
    assert!(
        gov::submit_appeal(
            db,
            &access,
            sanction,
            "an appeal cannot be rewritten",
            &key,
            now + 4
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_appeal")
            .await,
        1
    );
    let next = f.token(2, now + 62).await;
    assert!(
        gov::appeal_case(db, &access, sanction, now + 63)
            .await
            .is_err()
    );
    assert!(
        gov::check_appeal_access(db, &next, now + 3662)
            .await
            .is_err()
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn appeal_resolution_races_expiry_without_reviving_old_sessions() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let sid = f.sanction(2, "freeze", now).await;
    let raw = f.token(2, now + 3500).await;
    let (grant, _) = gov::exchange_appeal_access(db, &raw, now + 3500)
        .await
        .unwrap();
    let access = AppealAccess::Token(grant);
    let id = gov::submit_appeal(
        db,
        &access,
        sid,
        "the freeze reason is wrong",
        &identity::random_token(),
        now + 3501,
    )
    .await
    .unwrap();
    assert!(
        gov::resolve_appeal(
            db,
            actor(4),
            id,
            "accepted",
            "moderators cannot handle it",
            now + 3
        )
        .await
        .is_err()
    );
    let (a, b) = tokio::join!(
        gov::resolve_appeal(
            db,
            actor(1),
            id,
            "accepted",
            "revoked after review",
            now + 3601
        ),
        gov::expire_sanctions(db, now + 3601)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        f.scalar("SELECT `freeze` FROM gonof_common_member WHERE uid=2")
            .await,
        0
    );
    assert_eq!(
        f.scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=2")
            .await,
        3
    );
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE event_key LIKE 'sanction.%.ended'"
        )
        .await,
        1
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='appeal_result'")
            .await,
        1
    );
    assert!(
        f.identity()
            .session(&f.sessions[1])
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        gov::check_appeal_access(db, &raw, now + 3601)
            .await
            .is_err()
    );
    assert!(gov::appeal_cases(db, &access, 0, now + 3601).await.is_err());
    gov::resolve_appeal(
        db,
        actor(1),
        id,
        "accepted",
        "revoked after review",
        now + 3602,
    )
    .await
    .unwrap();
    assert!(
        gov::resolve_appeal(
            db,
            actor(5),
            id,
            "rejected",
            "override another admin",
            now + 3602
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='appeal_result'")
            .await,
        1
    );
    assert!(f.identity().login("appeal-2", PASSWORD).await.is_ok());
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn accepting_an_older_appeal_preserves_new_freeze_and_database_history() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let first = f.sanction(2, "freeze", now).await;
    let raw = f.token(2, now + 1).await;
    let id = gov::submit_appeal(
        db,
        &AppealAccess::Token(
            gov::exchange_appeal_access(db, &raw, now + 1)
                .await
                .unwrap()
                .0,
        ),
        first,
        "review the old sanction",
        &identity::random_token(),
        now + 2,
    )
    .await
    .unwrap();
    gov::revoke_sanction(db, actor(1), first, "revoke the old one first", now + 3)
        .await
        .unwrap();
    let second = f.sanction(2, "freeze", now + 4).await;
    gov::resolve_appeal(
        db,
        actor(1),
        id,
        "accepted",
        "the sanction does not stand",
        now + 5,
    )
    .await
    .unwrap();
    assert_eq!(
        f.scalar("SELECT `freeze` FROM gonof_common_member WHERE uid=2")
            .await,
        1
    );
    assert_eq!(
        db::query_scalar::<i64>("SELECT revoked_at FROM gonof_common_member_sanction WHERE id=?")
            .bind(second)
            .fetch_one(db.executor())
            .await
            .unwrap(),
        0
    );
    // Rewriting a decided appeal: that the resolution cannot be changed is guaranteed by the check under the lock and
    // the conditional update in the resolution entry point; that the submitted content appears in no UPDATE and that
    // appeals cannot be deleted are pinned by the source scan (`db::source_guards`).
    assert!(matches!(
        gov::resolve_appeal(db, actor(1), id, "rejected", "rewritten", now + 6).await,
        Err(content::writes::ContentWriteError::Conflict(_))
    ));
    let resolution: String =
        db::query_scalar("SELECT resolution FROM gonof_common_member_appeal WHERE id=?")
            .bind(id)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(resolution, "the sanction does not stand");
    // With the appellant changed, the sanction no longer belongs to them: the check script finds it (ownership is
    // kept only by code).
    gono_forum_web::test_support::assert_identity_rejected_or_flagged(
        db,
        "UPDATE gonof_common_member_appeal SET uid=3",
        "appeals against a sanction of another member or after the 30-day appeal period",
    )
    .await;
    let old = f.sanction(3, "warning", now - APPEAL_WINDOW - 1).await;
    assert!(
        gov::submit_appeal(
            db,
            &AppealAccess::Session(actor(3)),
            old,
            "late appeal",
            &identity::random_token(),
            now
        )
        .await
        .is_err()
    );
    let boundary = f.sanction(3, "warning", now - APPEAL_WINDOW).await;
    gov::submit_appeal(
        db,
        &AppealAccess::Session(actor(3)),
        boundary,
        "exactly thirty days",
        &identity::random_token(),
        now,
    )
    .await
    .unwrap();
    // Late appeals are not rejected by the database: the submission entry point rejects them (above), and ones written
    // in by bypassing the entry point are found by the check script.
    gono_forum_web::test_support::assert_identity_rejected_or_flagged(
        db,
        &format!(
            "INSERT INTO gonof_common_member_appeal(uid,sanction_id,reason,dateline,request_key) VALUES(3,{old},'late appeal that bypasses the entry point',{now},'{}')",
            identity::random_token()
        ),
        "appeals against a sanction of another member or after the 30-day appeal period",
    )
    .await;
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_rule WHERE enabled<>0")
            .await,
        0
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn frozen_governance_mail_and_password_change_revoke_appeal_access() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let sid = f.sanction(2, "freeze", now).await;
    let raw = f.token(2, now).await;
    let id = gov::submit_appeal(
        db,
        &AppealAccess::Token(gov::exchange_appeal_access(db, &raw, now).await.unwrap().0),
        sid,
        "please check again",
        &identity::random_token(),
        now,
    )
    .await
    .unwrap();
    gov::resolve_appeal(
        db,
        actor(1),
        id,
        "rejected",
        "Freeze upheld after checking",
        now,
    )
    .await
    .unwrap();
    let dir = f.services.root.join("mail-appeals");
    std::fs::write(&dir, b"block delivery").unwrap();
    let failed = gono_forum_web::test_support::deliver_mail(
        db,
        &dir,
        gono_forum_web::test_support::TEST_SITE_URL,
        now,
    )
    .await
    .unwrap();
    assert_eq!(failed.failed, 2);
    assert_eq!(failed.skipped, 1);
    std::fs::remove_file(&dir).unwrap();
    let result = gono_forum_web::test_support::deliver_mail(
        db,
        &dir,
        gono_forum_web::test_support::TEST_SITE_URL,
        now + 20,
    )
    .await
    .unwrap();
    assert_eq!(result.delivered, 2);
    assert_eq!(result.skipped, 0);
    let text = std::fs::read_dir(&dir)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry.file_type().unwrap().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "eml")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Reason: Broke the test community rules"));
    assert!(text.contains("Ends: "));
    assert!(text.contains("/appeals"));
    assert!(text.contains("Freeze upheld after checking"));
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(
            db,
            &dir,
            gono_forum_web::test_support::TEST_SITE_URL,
            now + 1
        )
        .await
        .unwrap()
        .claimed,
        0
    );
    f.sanction(3, "warning", now).await;
    let raw = f.token(3, now).await;
    let (cookie, _) = gov::exchange_appeal_access(db, &raw, now).await.unwrap();
    f.sanction(4, "warning", now).await;
    let unused = f.token(4, now).await;
    gov::check_appeal_access(db, &unused, now).await.unwrap();
    f.identity()
        .change_password(&f.sessions[3], PASSWORD, "Changed-unused-password")
        .await
        .unwrap();
    assert!(
        gov::check_appeal_access(db, &unused, now + 1)
            .await
            .is_err()
    );
    f.identity()
        .change_password(&f.sessions[2], PASSWORD, "Changed-appeal-password")
        .await
        .unwrap();
    assert!(
        gov::appeal_cases(db, &AppealAccess::Token(cookie), 0, now + 1)
            .await
            .is_err()
    );
    assert!(gov::check_appeal_access(db, &raw, now + 1).await.is_err());
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn appeal_http_uses_csrf_single_use_link_cookie_and_staff_authorization() {
    let f = Fixture::new().await;
    let now = identity::now();
    let sid = f.sanction(2, "freeze", now).await;
    let raw = f.token(2, now).await;
    let csrf = identity::random_token();
    let anon = format!("gonof_csrf__port58833={csrf}");
    let origin = gono_forum_web::test_support::TEST_SITE_URL;
    let url = format!("/appeals/access?token={raw}");
    let (status, headers, body) = f.call(&anon, "GET", &url, vec![], origin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::REFERRER_POLICY], "strict-origin");
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert!(body.contains("Continue to appeals"));
    assert!(!body.contains("data-debug-panel"));
    assert_eq!(
        f.call(
            &anon,
            "POST",
            "/appeals/access",
            vec![("token", raw.clone())],
            origin
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let fields = vec![("token", raw.clone()), ("_csrf", csrf.clone())];
    assert_eq!(
        f.call(
            &anon,
            "POST",
            "/appeals/access",
            fields.clone(),
            "https://evil.example"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, headers, _) = f
        .call(&anon, "POST", "/appeals/access", fields, origin)
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/appeals");
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|h| h.to_str().unwrap())
        .find(|h| h.starts_with("gonof_appeal__port58833="))
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Path=/appeals"));
    assert!(!cookie.contains(&raw));
    let client = format!("{anon}; {}", cookie.split(';').next().unwrap());
    assert_eq!(
        f.call(&anon, "GET", &url, vec![], origin).await.0,
        StatusCode::BAD_REQUEST
    );
    let detail = format!("/appeals/sanctions/{sid}");
    let (status, _, body) = f.call(&client, "GET", &detail, vec![], origin).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Submit appeal"));
    assert!(!body.contains("data-current-user="));
    assert_eq!(
        f.call(&client, "GET", "/my/profile", vec![], origin)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    let mixed = format!("{client}; gonof_session__port58833={}", f.sessions[0]);
    let (status, _, body) = f.call(&mixed, "GET", &detail, vec![], origin).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Account UID 2"));
    // The ticket decides the account in the appeals area, so the form must also stay under /appeals, where the ticket
    // cookie can reach.
    assert!(body.contains(&format!("action=\"{detail}\"")));
    assert!(!body.contains(&format!("action=\"/my/sanctions/{sid}/appeal\"")));
    let fields = vec![
        ("_csrf", csrf.clone()),
        ("request_key", identity::random_token()),
        ("reason", "Appeal from a frozen member".into()),
    ];
    let (status, headers, _) = f.call(&client, "POST", &detail, fields, origin).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), detail.as_str());
    let (_, _, body) = f.call(&client, "GET", &detail, vec![], origin).await;
    assert!(body.contains("Appeal: Pending"));
    let (status, headers, _) = f
        .call(
            &client,
            "POST",
            "/appeals/exit",
            vec![("_csrf", csrf.clone())],
            origin,
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.to_str().unwrap().contains("gonof_appeal__port58833=;"))
    );
    assert_eq!(
        f.call(&client, "GET", &detail, vec![], origin).await.0,
        StatusCode::SEE_OTHER
    );
    let moderator = format!("gonof_session__port58833={}", f.sessions[3]);
    assert_eq!(
        f.call(&moderator, "GET", "/manage/appeals", vec![], origin)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let admin = format!("gonof_session__port58833={}", f.sessions[0]);
    let (_, _, body) = f
        .call(&admin, "GET", "/manage/appeals", vec![], origin)
        .await;
    assert!(body.contains("Appeal from a frozen member"));
    let id = f.scalar("SELECT id FROM gonof_common_member_appeal").await;
    let fields = vec![
        ("_csrf", f.csrf_tokens[0].clone()),
        ("status", "accepted".into()),
        ("resolution", "Checked and restored".into()),
    ];
    assert_eq!(
        f.call(
            &admin,
            "POST",
            &format!("/manage/appeals/{id}"),
            fields,
            origin
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    // The log page takes the result and reason from `result`/`reason`, the same as report handling.
    let (_, _, logs) = f
        .call(&admin, "GET", "/manage/logs?kind=appeal", vec![], origin)
        .await;
    let resolved = logs
        .split("<article")
        .find(|row| row.contains("Appeal handled"))
        .expect("the log row for handling the appeal");
    assert!(resolved.contains("Accepted"), "{resolved}");
    assert!(resolved.contains("Checked and restored"), "{resolved}");
    assert_eq!(
        f.call(&client, "GET", &detail, vec![], origin).await.0,
        StatusCode::SEE_OTHER
    );
    f.services.cleanup().await.unwrap();
}
