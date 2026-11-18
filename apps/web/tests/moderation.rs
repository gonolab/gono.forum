use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self},
    identity::{self, Actor, Identity, IdentityError},
    moderation::{self, ReportTarget, SanctionRequest},
    notifications, router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
struct Fixture {
    services: TestFixture,
    app: Router,
    tokens: Vec<String>,
    csrf_tokens: Vec<String>,
    posts: Vec<Published>,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = &services.db;
        content::seed_forums(db).await.unwrap();
        let password = Argon2::default()
            .hash_password(
                b"Governance-test-password",
                &SaltString::encode_b64(b"governance-test-salt").unwrap(),
            )
            .unwrap()
            .to_string();
        for (uid, role, verified) in [
            (1, 1, 1),
            (2, 0, 1),
            (3, 0, 1),
            (4, 3, 1),
            (5, 2, 1),
            (6, 0, 0),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,?,?,?,?,1)").bind(uid).bind(format!("gov-{uid}")).bind(&password).bind(format!("gov-{uid}@example.test")).bind(verified).bind(role).execute(db.executor()).await.unwrap();
        }
        let forums: Vec<i64> = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid")
            .fetch_all(db.executor())
            .await
            .unwrap();
        db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(4,?)")
            .bind(forums[0])
            .execute(db.executor())
            .await
            .unwrap();
        let writer = ContentWriter::new(db.clone(), WriteConfig::default());
        let mut posts = vec![];
        for fid in &forums[..2] {
            posts.push(
                writer
                    .publish(
                        3,
                        NewThread {
                            visibility: Default::default(),
                            fid: *fid,
                            title: format!("Disputed thread {fid}"),
                            body: "<script>hidden-source-private</script> awaiting review".into(),
                            attachment_ids: vec![],
                            activity: Default::default(),
                            request_key: identity::random_token(),
                        },
                    )
                    .await
                    .unwrap(),
            );
        }
        let mut tokens = vec![];
        let mut csrf_tokens = vec![];
        let identity = Identity::new(
            db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        for uid in 1..=6 {
            let grant = identity
                .login(&format!("gov-{uid}"), "Governance-test-password")
                .await
                .unwrap();
            tokens.push(grant.session_token);
            csrf_tokens.push(grant.csrf_token);
        }
        let app = router::build(services.config.clone(), &services.root, db.clone());
        Self {
            services,
            app,
            tokens,
            csrf_tokens,
            posts,
        }
    }
    fn target(&self, i: usize) -> ReportTarget {
        ReportTarget {
            target_type: "thread".into(),
            target_id: self.posts[i].tid,
        }
    }
    fn sanction(&self, kind: &str) -> SanctionRequest {
        SanctionRequest {
            uid: 3,
            expected_version: 1,
            kind: kind.into(),
            hours: if kind == "freeze" { 1 } else { 0 },
            reason: "Broke the sample rules".into(),
            request_key: identity::random_token(),
        }
    }
    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.services.db.executor())
            .await
            .unwrap()
    }
    async fn call(
        &self,
        uid: Option<usize>,
        method: &str,
        path: &str,
        fields: Vec<(&str, String)>,
    ) -> (StatusCode, String, String) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(uid) = uid {
            request = request.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.tokens[uid - 1]),
            );
        }
        let body = if method == "POST" {
            request = request
                .header(header::ORIGIN, gono_forum_web::test_support::TEST_SITE_URL)
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
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        assert!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("no-store")
        );
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
        (status, location, body)
    }
    fn csrf(&self, uid: usize) -> String {
        self.csrf_tokens[uid - 1].clone()
    }
}

/// A reported image that is only someone's current avatar, with no post referencing it, still links to its content
/// on the report: the avatar route, for staff and for the reporter alike.
#[tokio::test]
async fn reported_avatars_link_to_the_avatar_route() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    db::query(
        "INSERT INTO gonof_forum_attachment(aid,uid,request_key,request_hash) VALUES(9480,3,?,?)",
    )
    .bind(identity::random_token())
    .bind(identity::random_token())
    .execute(db.executor())
    .await
    .unwrap();
    db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(9480,'avatar.png',10,'image/png',?,'avatar-object','test','','',?,?,'ready','clean','')")
        .bind("a".repeat(64))
        .bind(now)
        .bind(now)
        .execute(db.executor())
        .await
        .unwrap();
    db::query("UPDATE gonof_common_member SET avatar_aid=9480 WHERE uid=3")
        .execute(db.executor())
        .await
        .unwrap();
    let target = ReportTarget {
        target_type: "attachment".into(),
        target_id: 9480,
    };
    for (staff, reader) in [(true, 1), (false, 2)] {
        let view = moderation::report_target(db, &target, staff, reader)
            .await
            .unwrap();
        assert_eq!(view.href, "/avatar/3/9480", "staff: {staff}");
    }
    let id = moderation::submit_report(
        db,
        actor(2),
        &target,
        "offensive avatar",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    let (status, _, page) = f
        .call(Some(1), "GET", &format!("/manage/reports/{id}"), vec![])
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains(r#"href="/avatar/3/9480""#), "{page}");
    f.services.cleanup().await.unwrap();
}

/// The daily report quota counts the reports of the last 24 hours, and reads no more of the member's reports than the
/// quota itself: a member with a long history of reports does not have every one of them read on each submission.
#[tokio::test]
async fn the_report_quota_reads_only_the_newest_reports() {
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    // 40 old reports and 2 from the last day.
    for n in 0..42_i64 {
        let at = if n < 40 {
            now - 10 * 86_400 + n
        } else {
            now - 3600 + n
        };
        db::query("INSERT INTO gonof_common_report(uid,target_type,target_id,reason,status,dateline,request_key) VALUES(2,'profile',3,'old report','resolved',?,?)")
            .bind(at)
            .bind(format!("quota-history-{n}"))
            .execute(db.executor())
            .await
            .unwrap();
    }
    let profile = std::sync::Arc::new(RequestProfile::new());
    let third = trace::scope(
        profile.clone(),
        moderation::submit_report(
            db,
            actor(2),
            &f.target(0),
            "third today",
            &identity::random_token(),
            now,
            3,
        ),
    )
    .await;
    assert!(third.is_ok(), "{third:?}");
    let reads: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|record| {
            record.source == QuerySource::Application
                && record.sql.contains("FROM gonof_common_report WHERE uid=?")
                && !record.sql.contains("request_key")
                && !record.sql.contains("target_type")
        })
        .collect();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(
        !reads[0].sql.contains("COUNT(") && reads[0].sql.contains("LIMIT"),
        "the quota reads a bounded number of rows: {}",
        reads[0].sql
    );
    assert!(reads[0].rows_returned <= 3);
    // The fourth report within 24 hours is over the quota.
    assert!(matches!(
        moderation::submit_report(
            db,
            actor(2),
            &f.target(1),
            "fourth today",
            &identity::random_token(),
            now,
            3,
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    // A day later the two older ones have left the window.
    assert!(
        moderation::submit_report(
            db,
            actor(2),
            &f.target(1),
            "next day",
            &identity::random_token(),
            now + 86_400,
            3,
        )
        .await
        .is_ok()
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn reports_preserve_scope_idempotency_and_hidden_content_notifications() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let target = f.target(0);
    let key = identity::random_token();
    let (a, b) = tokio::join!(
        moderation::submit_report(db, actor(2), &target, "counted once", &key, now, 10),
        moderation::submit_report(db, actor(2), &target, "counted once", &key, now, 10)
    );
    let id = a.unwrap();
    assert_eq!(id, b.unwrap());
    assert_eq!(
        id,
        moderation::submit_report(
            db,
            actor(2),
            &target,
            "another click",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .unwrap()
    );
    assert!(matches!(
        moderation::submit_report(db, actor(2), &target, "tampered retry", &key, now, 10).await,
        Err(ContentWriteError::Conflict(_))
    ));
    assert!(
        moderation::submit_report(
            db,
            actor(3),
            &target,
            "own content",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .is_err()
    );
    assert!(
        moderation::submit_report(
            db,
            actor(6),
            &target,
            "unverified",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .is_err()
    );
    assert!(
        moderation::report_detail(db, actor(3), id, false)
            .await
            .is_err()
    );
    let outside = moderation::submit_report(
        db,
        actor(2),
        &f.target(1),
        "another forum",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    assert_eq!(
        moderation::report_list(db, actor(4), true, "pending", 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        moderation::report_detail(db, actor(4), outside, true)
            .await
            .is_err()
    );
    assert!(
        moderation::resolve_report(db, actor(4), outside, "resolved", "out of scope", now)
            .await
            .is_err()
    );
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86400);
    let request = ChangeRequest {
        pid: f.posts[0].pid,
        expected_post_version: 1,
        expected_thread_version: 1,
        request_key: identity::random_token(),
        reason: "Hidden by moderation <script>must not run</script>".into(),
        action: ContentAction::Hide,
    };
    let moderation_key = request.request_key.clone();
    editor.change(actor(4), request.clone()).await.unwrap();
    editor.change(actor(4), request).await.unwrap();
    let notices = notifications::list(db, actor(3), 1, 20).await.unwrap();
    assert_eq!(notices.items.len(), 1);
    assert_eq!(notices.items[0].kind, "moderation");
    assert!(!notices.items[0].available);
    assert!(notices.items[0].subject.is_empty());
    assert!(notices.items[0].reason.contains("Hidden by moderation"));
    assert!(
        moderation::report_target(db, &target, false, 2)
            .await
            .is_err()
    );
    assert!(
        moderation::submit_report(
            db,
            actor(5),
            &target,
            "hidden target",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .is_err()
    );
    moderation::resolve_report(db, actor(4), id, "resolved", "content hidden", now + 1)
        .await
        .unwrap();
    moderation::resolve_report(db, actor(4), id, "resolved", "content hidden", now + 1)
        .await
        .unwrap();
    assert!(matches!(
        moderation::resolve_report(db, actor(1), id, "dismissed", "overwrite", now + 1).await,
        Err(ContentWriteError::Conflict(_))
    ));
    let report_notice = notifications::list(db, actor(2), 1, 20).await.unwrap();
    assert_eq!(report_notice.items.len(), 1);
    assert_eq!(
        notifications::open_url(db, actor(2), report_notice.items[0].id, now + 2)
            .await
            .unwrap(),
        format!("/my/reports/{id}")
    );
    assert!(
        notifications::open_url(db, actor(3), report_notice.items[0].id, now + 2)
            .await
            .is_err()
    );
    // Report contents are immutable without a trigger: the only code that changes reports is the one conditional
    // update in the resolve entry point, which only writes the four resolution columns, pinned by the source scan
    // (`db::source_guards`); for resolving only once see the "overwrite" attempt above.
    // Request keys are scoped to their operator, matching the existing content request ledger.
    editor
        .change(
            actor(5),
            ChangeRequest {
                pid: f.posts[1].pid,
                expected_post_version: 1,
                expected_thread_version: 1,
                request_key: moderation_key,
                reason: "handled separately by another staff member".into(),
                action: ContentAction::Hide,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        notifications::list(db, actor(3), 1, 20)
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    db::query("DELETE FROM gonof_forum_moderator WHERE uid=4")
        .execute(db.executor())
        .await
        .unwrap();
    assert!(
        moderation::report_detail(db, actor(4), id, true)
            .await
            .is_err()
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn sanctions_invalidate_cookies_notify_owner_and_expire_once() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let request = f.sanction("freeze");
    assert!(
        moderation::issue_sanction(db, actor(4), &request, now)
            .await
            .is_err()
    );
    let (a, b) = tokio::join!(
        moderation::issue_sanction(db, actor(1), &request, now),
        moderation::issue_sanction(db, actor(1), &request, now)
    );
    let id = a.unwrap();
    assert_eq!(id, b.unwrap());
    assert_eq!(
        f.scalar("SELECT `freeze` FROM gonof_common_member WHERE uid=3")
            .await,
        1
    );
    assert_eq!(
        f.scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=3")
            .await,
        2
    );
    let identity = Identity::new(
        db.clone(),
        f.services.config.identity.clone(),
        f.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    assert!(identity.session(&f.tokens[2]).await.unwrap().is_none());
    assert!(matches!(
        identity.login("gov-3", "wrong password").await,
        Err(IdentityError::Unauthorized)
    ));
    assert!(
        matches!(identity.login("gov-3","Governance-test-password").await,Err(IdentityError::Suspended(ref reason)) if reason.contains("Broke the sample rules"))
    );
    assert!(notifications::list(db, actor(3), 1, 20).await.is_err());
    assert_eq!(
        moderation::expire_sanctions(db, now + 3599).await.unwrap(),
        0
    );
    assert_eq!(
        moderation::expire_sanctions(db, now + 3601).await.unwrap(),
        1
    );
    assert_eq!(
        moderation::expire_sanctions(db, now + 3602).await.unwrap(),
        0
    );
    assert_eq!(
        f.scalar("SELECT `freeze` FROM gonof_common_member WHERE uid=3")
            .await,
        0
    );
    assert_eq!(
        f.scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=3")
            .await,
        3
    );
    let grant = identity
        .login("gov-3", "Governance-test-password")
        .await
        .unwrap();
    let owner = grant.member.actor();
    let list = notifications::list(db, owner, 1, 20).await.unwrap();
    assert_eq!(list.items.len(), 2);
    assert!(list.items.iter().all(|n| n.kind == "sanction"));
    assert!(
        moderation::sanctions(db, actor(2), 3, false, 0)
            .await
            .is_err()
    );
    let mut warning = f.sanction("warning");
    warning.expected_version = 3;
    let warning_id = moderation::issue_sanction(db, actor(1), &warning, now)
        .await
        .unwrap();
    assert_eq!(
        f.scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=3")
            .await,
        3
    );
    moderation::revoke_sanction(db, actor(1), warning_id, "reviewed", now + 1)
        .await
        .unwrap();
    moderation::revoke_sanction(db, actor(1), warning_id, "repeat", now + 2)
        .await
        .unwrap();
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE type='sanction'")
            .await,
        4
    );
    let mut freeze = f.sanction("freeze");
    freeze.expected_version = 3;
    freeze.hours = 0;
    let freeze_id = moderation::issue_sanction(db, actor(1), &freeze, now)
        .await
        .unwrap();
    moderation::revoke_sanction(db, actor(1), freeze_id, "lifted", now + 2)
        .await
        .unwrap();
    assert_eq!(
        f.scalar("SELECT `freeze` FROM gonof_common_member WHERE uid=3")
            .await,
        0
    );
    // Sanction text is immutable without a trigger: the only code that changes sanctions is the one conditional update
    // ending a sanction, which only writes the two revocation columns, pinned by the source scan (`db::source_guards`).
    // Sanction notifications may only go to the sanctioned member: checked by primary key in
    // notifications::record::insert.
    let wrong_owner = gono_forum_web::test_support::insert_notification(
        db,
        &gono_forum_web::notifications::record::NewNotice {
            uid: 2,
            authorid: 1,
            kind: gono_forum_web::notifications::record::Kind::Sanction,
            target: gono_forum_web::notifications::record::Target::Sanction(id),
            event_key: "wrong-owner",
            reason: "test",
            dateline: now,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        wrong_owner.contains("is not the reporter or the sanctioned member"),
        "{wrong_owner}"
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn moderation_http_requires_csrf_scope_and_escapes_reasons() {
    let f = Fixture::new().await;
    let now = identity::now();
    assert_eq!(
        f.call(None, "GET", "/my/reports", vec![]).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.call(Some(2), "GET", "/manage/reports", vec![]).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call(Some(4), "GET", "/manage/sanctions?uid=3", vec![])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let form = vec![
        ("_csrf", f.csrf(2)),
        ("target_type", "thread".into()),
        ("target_id", f.posts[0].tid.to_string()),
        (
            "reason",
            "<script>alert('report')</script>a specific problem".into(),
        ),
        ("request_key", identity::random_token()),
    ];
    let mut bad = form.clone();
    bad[0].1 = "invalid".into();
    assert_eq!(
        f.call(Some(2), "POST", "/my/reports", bad).await.0,
        StatusCode::FORBIDDEN
    );
    let result = f.call(Some(2), "POST", "/my/reports", form).await;
    assert_eq!(result.0, StatusCode::SEE_OTHER);
    let path = result.1;
    let body = f.call(Some(2), "GET", &path, vec![]).await;
    assert_eq!(body.0, StatusCode::OK);
    assert!(body.2.contains("&#60;script&#62;"));
    assert!(!body.2.contains("<script>alert('report')"));
    assert_eq!(
        f.call(Some(3), "GET", &path, vec![]).await.0,
        StatusCode::NOT_FOUND
    );
    // The member view is at /my/reports/{id} and the admin area at /manage/reports/{id}; they can no longer be built
    // by prefix concatenation.
    let detail = path.replace("/my/reports/", "/manage/reports/");
    let staff_view = f.call(Some(4), "GET", &detail, vec![]).await;
    assert_eq!(staff_view.0, StatusCode::OK);
    // Each list links to its own detail: links in the member list must open (they used to be /reports/{id}, a dead
    // link).
    let mine = f.call(Some(2), "GET", "/my/reports?status=", vec![]).await;
    assert!(
        mine.2.contains(&format!("href=\"{path}\"")),
        "the member report list must link to {path}"
    );
    let queue = f.call(Some(4), "GET", "/manage/reports", vec![]).await;
    assert!(
        queue.2.contains(&format!("href=\"{detail}\"")),
        "the admin report list must link to {detail}"
    );
    assert!(
        staff_view
            .2
            .contains(r#"href="/manage/reports">Back to reports"#)
    );
    // Both entry points share one template, so each canonical must belong to its own page: hard-coding either one
    // would make the other declare the wrong URL publicly, and the admin directory could not tell which page it is.
    let canonical = |html: &str| {
        html.split_once("rel=\"canonical\" href=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value.to_owned())
            .unwrap_or_default()
    };
    assert!(
        canonical(&staff_view.2).ends_with(&detail),
        "the admin report detail's canonical must point to {detail}, got {}",
        canonical(&staff_view.2)
    );
    assert!(
        canonical(&body.2).ends_with(&path),
        "the member report detail's canonical must point to {path}, got {}",
        canonical(&body.2)
    );
    let resolved = f
        .call(
            Some(4),
            "POST",
            &detail,
            vec![
                ("_csrf", f.csrf(4)),
                ("status", "dismissed".into()),
                (
                    "resolution",
                    "Not enough evidence <script>test</script>".into(),
                ),
            ],
        )
        .await;
    assert_eq!(resolved.0, StatusCode::SEE_OTHER);
    let body = f.call(Some(2), "GET", "/my/notifications", vec![]).await.2;
    assert!(body.contains("Report outcome"));
    assert!(body.contains("Not enough evidence &#60;script&#62;test&#60;/script&#62;"));
    assert!(body.contains("View record"));
    let notice = notifications::list(&f.services.db, actor(2), 1, 20)
        .await
        .unwrap()
        .items[0]
        .id;
    assert_eq!(
        f.call(
            Some(2),
            "POST",
            &format!("/my/notifications/{notice}/open"),
            vec![
                ("_csrf", f.csrf(2)),
                ("request_key", identity::random_token())
            ]
        )
        .await
        .1,
        path
    );
    let warning = f.sanction("warning");
    let warning_id = moderation::issue_sanction(&f.services.db, actor(1), &warning, now)
        .await
        .unwrap();
    let body = f
        .call(Some(3), "GET", "/my/sanctions?uid=2", vec![])
        .await
        .2;
    assert!(body.contains("Broke the sample rules"));
    assert!(
        !f.call(Some(2), "GET", "/my/sanctions?uid=3", vec![])
            .await
            .2
            .contains("Broke the sample rules")
    );
    assert!(
        f.call(Some(1), "GET", "/manage/sanctions?uid=3", vec![])
            .await
            .2
            .contains("Confirm and notify the member")
    );
    assert!(
        f.call(
            Some(2),
            "GET",
            &format!("/thread-{}", f.posts[0].tid),
            vec![]
        )
        .await
        .2
        .contains("/my/reports/new?target_type=thread")
    );
    let revoked = f
        .call(
            Some(1),
            "POST",
            &format!("/manage/sanctions/{warning_id}/revoke"),
            vec![
                ("_csrf", f.csrf(1)),
                ("reason", "warning revoked after review".into()),
            ],
        )
        .await;
    assert_eq!(revoked.0, StatusCode::SEE_OTHER);
    assert_eq!(revoked.1, "/manage/sanctions?uid=3");
    assert!(
        f.call(Some(3), "GET", "/my/notifications", vec![])
            .await
            .2
            .contains("Your warning was revoked")
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn profile_and_attachment_reports_require_public_targets_and_admin_review() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let now = identity::now();
    let profile = ReportTarget {
        target_type: "profile".into(),
        target_id: 3,
    };
    let view = moderation::report_target(db, &profile, false, 2)
        .await
        .unwrap();
    assert_eq!(view.href, "/space/3");
    let profile_id = moderation::submit_report(
        db,
        actor(2),
        &profile,
        "profile problem",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    assert!(
        moderation::report_detail(db, actor(5), profile_id, true)
            .await
            .is_err()
    );
    assert!(
        moderation::report_detail(db, actor(1), profile_id, true)
            .await
            .is_ok()
    );
    let aid =
        db::query("INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(3,?,?)")
            .bind(identity::random_token())
            .bind(identity::random_token())
            .execute(db.executor())
            .await
            .unwrap()
            .last_insert_id() as i64;
    db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,'fixture.txt',10,'text/plain',?,'private-object-key','test','','',?,?,'ready','clean','')")
  .bind(aid).bind(identity::random_token()).bind(now).bind(now).execute(db.executor()).await.unwrap();
    let target = ReportTarget {
        target_type: "attachment".into(),
        target_id: aid,
    };
    assert!(
        moderation::report_target(db, &target, false, 2)
            .await
            .is_err()
    );
    assert!(
        moderation::submit_report(
            db,
            actor(2),
            &target,
            "private attachment",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .is_err()
    );
    db::query("INSERT INTO gonof_forum_attachment_reference(aid,pid,dateline) VALUES(?,?,?)")
        .bind(aid)
        .bind(f.posts[0].pid)
        .bind(now)
        .execute(db.executor())
        .await
        .unwrap();
    let view = moderation::report_target(db, &target, false, 2)
        .await
        .unwrap();
    assert!(view.href.contains("/media/"));
    assert!(!view.href.contains("private-object-key"));
    let id = moderation::submit_report(
        db,
        actor(2),
        &target,
        "public attachment problem",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    assert!(
        moderation::report_detail(db, actor(4), id, true)
            .await
            .is_err()
    );
    moderation::resolve_report(db, actor(1), id, "dismissed", "file is fine", now + 1)
        .await
        .unwrap();
    db::query(
        "UPDATE gonof_forum_attachment_0 SET lifecycle='rejected',scan_status='rejected' WHERE aid=?",
    )
    .bind(aid)
    .execute(db.executor())
    .await
    .unwrap();
    assert!(
        moderation::report_target(db, &target, false, 2)
            .await
            .is_err()
    );
    assert!(
        moderation::submit_report(
            db,
            actor(2),
            &target,
            "unavailable attachment",
            &identity::random_token(),
            now + 2,
            10,
        )
        .await
        .is_err()
    );
    let report_id = moderation::submit_report(
        db,
        actor(2),
        &f.target(0),
        "rate limit sample",
        &identity::random_token(),
        now + 2,
        10,
    )
    .await
    .unwrap();
    moderation::resolve_report(db, actor(1), report_id, "dismissed", "all fine", now + 3)
        .await
        .unwrap();
    // Existing reports use the same persistent quota across service instances and restarts.
    for _ in 0..7 {
        let r = moderation::submit_report(
            db,
            actor(2),
            &f.target(0),
            "submitted again",
            &identity::random_token(),
            now + 4,
            10,
        )
        .await;
        match r {
            Ok(id) => moderation::resolve_report(db, actor(1), id, "dismissed", "done", now + 5)
                .await
                .unwrap(),
            Err(ContentWriteError::Forbidden(_)) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert!(matches!(
        moderation::submit_report(
            db,
            actor(2),
            &f.target(1),
            "over the limit",
            &identity::random_token(),
            now + 6,
            10,
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn reply_reports_keep_withdrawn_topic_titles_private() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let reply = ContentWriter::new(db.clone(), WriteConfig::default())
        .reply(
            2,
            NewReply {
                tid: f.posts[0].tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A reply that stays public".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86400);
    let current = editor.editable(actor(3), f.posts[0].pid).await.unwrap();
    editor
        .change(
            actor(3),
            ChangeRequest {
                pid: current.pid,
                expected_post_version: current.version,
                expected_thread_version: current.thread_version,
                action: ContentAction::Withdraw,
                reason: String::new(),
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let target = ReportTarget {
        target_type: "reply".into(),
        target_id: reply.pid,
    };
    let view = moderation::report_target(db, &target, false, 2)
        .await
        .unwrap();
    assert_eq!(view.title, "Thread withdrawn");
    assert_eq!(view.body, "A reply that stays public");
    assert_eq!(
        moderation::report_target(db, &target, true, 2)
            .await
            .unwrap()
            .title,
        current.title
    );
    let id = moderation::submit_report(
        db,
        actor(4),
        &target,
        "the reply is reportable",
        &identity::random_token(),
        identity::now(),
        10,
    )
    .await
    .unwrap();
    let body = f
        .call(Some(4), "GET", &format!("/my/reports/{id}"), vec![])
        .await;
    assert_eq!(body.0, StatusCode::OK);
    assert!(body.2.contains("Thread withdrawn"));
    assert!(body.2.contains(r#"href="/my/reports">Back to reports"#));
    assert!(!body.2.contains(&current.title));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn sanction_read_authorization_keeps_forbidden_for_invalid_administrators() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    assert!(moderation::sanction_member(db, actor(1), 3).await.is_ok());
    // Global moderators can impose mutes, so they can also read sanction records (docs/roles.md, "Mutes" and
    // "Staff roles").
    assert!(moderation::sanction_member(db, actor(5), 3).await.is_ok());
    assert!(
        moderation::sanctions(db, actor(5), 3, true, 0)
            .await
            .is_ok()
    );
    for denied in [
        actor(4),
        actor(999),
        Actor {
            uid: 1,
            credentials_version: 2,
            api_key: None,
        },
    ] {
        assert!(matches!(
            moderation::sanction_member(db, denied, 3).await,
            Err(ContentWriteError::Forbidden(
                "Only administrators and global moderators can manage account sanctions"
            ))
        ));
        assert!(matches!(
            moderation::sanctions(db, denied, 3, true, 0).await,
            Err(ContentWriteError::Forbidden(
                "Only administrators and global moderators can manage account sanctions"
            ))
        ));
    }
    db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=1")
        .execute(db.executor())
        .await
        .unwrap();
    assert!(matches!(
        moderation::sanction_member(db, actor(1), 3).await,
        Err(ContentWriteError::Forbidden(
            "Only administrators and global moderators can manage account sanctions"
        ))
    ));
    f.services.cleanup().await.unwrap();
}

// Moderators and global moderators recuse themselves from reports against their own content; reports they submitted
// are handled as usual, and administrators are not restricted.
#[tokio::test]
async fn staff_recuse_from_reports_on_their_own_content() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let now = identity::now();
    let mut reports = vec![];
    for uid in [4, 5, 1] {
        let reply = writer
            .reply(
                uid,
                NewReply {
                    tid: f.posts[0].tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: format!("Reply by staff member {uid}"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
        let target = ReportTarget {
            target_type: "reply".into(),
            target_id: reply.pid,
        };
        let id = moderation::submit_report(
            db,
            actor(2),
            &target,
            "report on a staff reply",
            &identity::random_token(),
            now,
            10,
        )
        .await
        .unwrap();
        reports.push((uid, id));
    }
    for &(uid, id) in &reports[..2] {
        assert!(matches!(
            moderation::resolve_report(db, actor(uid), id, "dismissed", "own review", now + 1)
                .await,
            Err(ContentWriteError::Forbidden(_))
        ));
        let pending =
            format!("SELECT COUNT(*) FROM gonof_common_report WHERE id={id} AND status='pending'");
        assert_eq!(f.scalar(&pending).await, 1);
        let (status, _, html) = f
            .call(
                Some(uid as usize),
                "GET",
                &format!("/manage/reports/{id}"),
                vec![],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Please leave it to another moderator or an administrator"));
        assert!(!html.contains(&format!("action=\"/manage/reports/{id}\"")));
    }
    // A moderator's own reply is reviewed by a global moderator, and a global moderator's own reply by an
    // administrator.
    moderation::resolve_report(db, actor(5), reports[0].1, "dismissed", "all fine", now + 2)
        .await
        .unwrap();
    moderation::resolve_report(db, actor(1), reports[1].1, "dismissed", "all fine", now + 2)
        .await
        .unwrap();
    let (_, _, html) = f
        .call(
            Some(1),
            "GET",
            &format!("/manage/reports/{}", reports[2].1),
            vec![],
        )
        .await;
    assert!(html.contains(&format!("action=\"/manage/reports/{}\"", reports[2].1)));
    moderation::resolve_report(db, actor(1), reports[2].1, "dismissed", "all fine", now + 2)
        .await
        .unwrap();
    // A moderator handling a report they submitted themselves is not affected.
    let own = moderation::submit_report(
        db,
        actor(4),
        &f.target(0),
        "moderator's own report",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    moderation::resolve_report(db, actor(4), own, "resolved", "handled", now + 3)
        .await
        .unwrap();
    f.services.cleanup().await.unwrap();
}

/// Submitting a report must not hold other members' pending report rows while waiting for the content
/// lock.
///
/// Resolving a report locks the content first (forum, thread, post), then the report row. Here a separate
/// transaction plays the resolving side: it holds the forum row first, and after the submitting side's forum locking
/// statement has been sent (and is waiting), it goes on to lock the first report. The old code's duplicate check went
/// through the (target_type,target_id) index and locked this report with FOR UPDATE, so the two sides waited on each
/// other in a deadlock.
#[tokio::test]
async fn submitting_a_report_waits_for_content_without_holding_other_reports() {
    // Only verifies MySQL row locks and lock wait timeouts.
    gono_forum_web::mysql_only!();
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    use std::{sync::Arc, time::Duration};
    let f = Fixture::new().await;
    let db = f.services.db.clone();
    let now = identity::now();
    let first = moderation::submit_report(
        &db,
        actor(2),
        &f.target(0),
        "first report",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    // Give another reporter more reports than this target has, so the duplicate check picks the target index (in real
    // data the same thread is rarely reported repeatedly, so this index is the most selective). dateline is very
    // early and does not use the 24-hour quota.
    for index in 0..60 {
        db::query("INSERT INTO gonof_common_report(uid,target_type,target_id,reason,dateline,request_key) VALUES(5,'thread',?,'old report',1,?)")
            .bind(1_000_000 + index)
            .bind(identity::random_token())
            .execute(db.executor())
            .await
            .unwrap();
    }
    gono_forum_web::db::raw_sql("ANALYZE TABLE gonof_common_report")
        .execute(&db.pool)
        .await
        .unwrap();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=?")
        .bind(f.posts[0].tid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    let mut handler = db.begin_write().await.unwrap();
    db::query("SET SESSION innodb_lock_wait_timeout=1")
        .execute(&mut *handler)
        .await
        .unwrap();
    db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
        .bind(fid)
        .fetch_one(&mut *handler)
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let submitting = {
        let db = db.clone();
        let profile = profile.clone();
        let target = f.target(0);
        tokio::spawn(async move {
            trace::scope(
                profile,
                Box::pin(moderation::submit_report(
                    &db,
                    actor(5),
                    &target,
                    "second report",
                    &identity::random_token(),
                    now,
                    10,
                )),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !profile.snapshot().iter().any(|record| {
            record.source == QuerySource::Application
                // The report target's forum row takes a shared lock and still has to wait for the
                // resolving side's exclusive lock.
                && record.sql == "SELECT fid FROM gonof_forum_forum WHERE fid=? LOCK IN SHARE MODE"
                && record.rows_returned == 0
                && record.elapsed.is_zero()
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("submitting a report must first wait on the forum lock");
    let locked = db::query("SELECT id FROM gonof_common_report WHERE id=? FOR UPDATE")
        .bind(first)
        .fetch_optional(&mut *handler)
        .await;
    assert!(
        matches!(locked, Ok(Some(_))),
        "the resolving side must get the report row at once: {locked:?}"
    );
    handler.commit().await.unwrap();
    submitting
        .await
        .unwrap()
        .expect("the submission must complete once the forum is released");
    let account_locks = profile
        .snapshot()
        .iter()
        .filter(|record| {
            record.sql.starts_with("SELECT adminid,emailstatus,mobile,`freeze`,credentials_version,account_state FROM gonof_common_member")
        })
        .count();
    assert_eq!(
        account_locks, 1,
        "the submission must not be rolled back and retried after a deadlock"
    );
    f.services.cleanup_ref().await.unwrap();
}
