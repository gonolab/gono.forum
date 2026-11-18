//! Community directory group: the verified member directory and badges, and the friend link directory, home page
//! display and member application entry.
//! Only administrators can verify members and maintain links; the directory follows profile
//! privacy, blocks, freezes and expiry; the rel of linked websites and friend links; idempotency, quotas, cooldown,
//! rate limits, read-only mode and the switch for applications.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities::ActivitySpec,
    content::{
        self,
        writes::{ContentWriter, NewThread, ThreadVisibility, WriteConfig},
    },
    db::{self, Database},
    directory,
    identity::{self, Actor, Identity, IdentityError, SessionGrant},
    links, router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: i64 = 7401;
const MOD: i64 = 7402;
const A: i64 = 7403;
const B: i64 = 7404;
const C: i64 = 7405;
const D: i64 = 7406;
const E: i64 = 7407;
const VIEWER: i64 = 7408;
const YOUNG: i64 = 7409;
const UNVERIFIED: i64 = 7410;
const F: i64 = 7411;

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
    fid: i64,
    now: i64,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        let now = identity::now();
        for (uid, adminid, emailstatus, age) in [
            (ADMIN, 1, 1, 100),
            (MOD, 2, 1, 100),
            (A, 0, 1, 100),
            (B, 0, 1, 100),
            (C, 0, 1, 100),
            (D, 0, 1, 100),
            (E, 0, 1, 100),
            (VIEWER, 0, 1, 100),
            (YOUNG, 0, 1, 3),
            (UNVERIFIED, 0, 0, 100),
            (F, 0, 1, 100),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,?,?,?)")
                .bind(uid)
                .bind(format!("dir-{uid}"))
                .bind(format!("dir-{uid}@example.test"))
                .bind(emailstatus)
                .bind(adminid)
                .bind(now - age * 86_400)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let fid = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let config = services.config.clone();
        let app = router::build_with_global(
            config,
            &services.root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            fid,
            now,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Edits the database directly. Settings and the friend links page read from the in-process global cache, so the
    /// cache of this process is cleared afterwards, equivalent to "takes effect in this process immediately after
    /// saving".
    async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db().executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        self.services.global.clear_local();
    }

    async fn thread(&self, uid: i64, title: &str) -> i64 {
        ContentWriter::new(self.db().clone(), WriteConfig::default())
            .publish_at(
                uid,
                NewThread {
                    visibility: ThreadVisibility::Public,
                    activity: ActivitySpec::default(),
                    fid: self.fid,
                    title: title.into(),
                    body: format!("{title} body text"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                self.now,
            )
            .await
            .unwrap()
            .tid
    }

    async fn grant(&self, uid: i64, category: &str, title: &str, expires_at: Option<i64>) {
        directory::save(
            self.db(),
            actor(ADMIN),
            uid,
            directory::Grant {
                category: category.into(),
                title: title.into(),
                site_url: format!("https://site-{uid}.example.test/"),
                expires_at,
                reviewed_at: None,
            },
            self.now,
        )
        .await
        .unwrap();
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        self.send_from(request, "192.0.2.58").await
    }

    async fn send_from(&self, mut request: Request<Body>, ip: &str) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            format!("{ip}:48323").parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn get(&self, path: &str, session: Option<&SessionGrant>) -> Reply {
        let mut builder = Request::builder().uri(path);
        if let Some(session) = session {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", session.session_token),
            );
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, session: &SessionGrant, fields: &[(&str, &str)]) -> Reply {
        self.post_from(path, session, fields, "192.0.2.58").await
    }

    async fn post_from(
        &self,
        path: &str,
        session: &SessionGrant,
        fields: &[(&str, &str)],
        ip: &str,
    ) -> Reply {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &session.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        self.send_from(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", session.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
            ip,
        )
        .await
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        let identity = Identity::new(
            self.db().clone(),
            self.services.config.identity.clone(),
            self.services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        http_auth::login(&identity, self.db(), uid).await
    }

    async fn apply(&self, session: &SessionGrant, domain: &str, key: &str) -> Reply {
        let url = format!("https://{domain}/");
        self.post(
            "/links/apply",
            session,
            &[
                ("name", &format!("Site {domain}")),
                ("category", "blog"),
                ("url", &url),
                ("backlink_url", ""),
                ("note", "Link exchange"),
                ("request_key", key),
            ],
        )
        .await
    }

    async fn link_id(&self, domain: &str) -> i64 {
        self.scalar(&format!(
            "SELECT MAX(id) FROM gonof_common_friend_link WHERE domain='{domain}'"
        ))
        .await
    }

    async fn logs(&self, kind: &str) -> i64 {
        self.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='{kind}'"
        ))
        .await
    }
}

/// Only administrators can grant, modify, renew and revoke; replaying the same form writes no extra log.
#[tokio::test]
async fn only_admins_grant_renew_and_revoke_verifications() {
    let f = Fixture::new().await;
    let viewer = f.login(VIEWER).await;
    let moderator = f.login(MOD).await;
    let admin = f.login(ADMIN).await;
    let form = [
        ("category", "merchant"),
        ("title", "Official Account of Example Cloud"),
        ("site_url", "https://cloud.example.test/"),
        ("expires", ""),
    ];
    for session in [&viewer, &moderator] {
        assert_eq!(
            f.get("/manage/verifications", Some(session)).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            f.post(&format!("/manage/verifications/{A}"), session, &form)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
    assert!(matches!(
        directory::save(
            f.db(),
            actor(MOD),
            A,
            directory::Grant {
                category: "merchant".into(),
                title: "Not allowed".into(),
                site_url: String::new(),
                expires_at: None,
                reviewed_at: None,
            },
            f.now,
        )
        .await,
        Err(IdentityError::Forbidden)
    ));
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_verification")
            .await,
        0
    );

    let saved = f
        .post(&format!("/manage/verifications/{A}"), &admin, &form)
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        saved.location(),
        format!("/manage/verifications/{A}?done=saved")
    );
    assert_eq!(f.logs("identity.verification.grant").await, 1);
    // Submitting the same form again: nothing changed, nothing is written.
    let again = f
        .post(&format!("/manage/verifications/{A}"), &admin, &form)
        .await;
    assert_eq!(
        again.location(),
        format!("/manage/verifications/{A}?done=unchanged")
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type LIKE 'identity.verification.%'")
            .await,
        1
    );
    // Changing only the expiry date is a renewal.
    let renewed = f
        .post(
            &format!("/manage/verifications/{A}"),
            &admin,
            &[
                ("category", "merchant"),
                ("title", "Official Account of Example Cloud"),
                ("site_url", "https://cloud.example.test/"),
                ("expires", "2099-12-31"),
            ],
        )
        .await;
    assert_eq!(renewed.status, StatusCode::SEE_OTHER, "{}", renewed.body);
    assert_eq!(f.logs("identity.verification.renew").await, 1);
    assert!(
        f.scalar(&format!(
            "SELECT expires_at FROM gonof_common_member_verification WHERE uid={A}"
        ))
        .await
            > f.now
    );
    // An unsafe URL and a past expiry date both stay in the form with errors; the original record is unchanged.
    for (site_url, expires) in [
        ("javascript:alert(1)", ""),
        ("https://user:pw@cloud.example.test/", ""),
        ("https://cloud.example.test/", "2000-01-01"),
    ] {
        let rejected = f
            .post(
                &format!("/manage/verifications/{A}"),
                &admin,
                &[
                    ("category", "merchant"),
                    ("title", "Official Account of Example Cloud"),
                    ("site_url", site_url),
                    ("expires", expires),
                ],
            )
            .await;
        assert_eq!(
            rejected.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{site_url} {expires}"
        );
    }
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_verification WHERE uid={A} AND site_url='https://cloud.example.test/'"
        ))
        .await,
        1
    );
    // Admin list and edit page.
    let list = f.get("/manage/verifications", Some(&admin)).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.body.contains("Official Account of Example Cloud")
            && list.body.contains("Verification guidelines")
    );
    assert_eq!(
        f.get(&format!("/manage/verifications?uid={B}"), Some(&admin))
            .await
            .location(),
        format!("/manage/verifications/{B}")
    );
    assert!(
        f.get(&format!("/manage/verifications/{B}"), Some(&admin))
            .await
            .body
            .contains("Grant verification")
    );
    // Revoking requires a reason; moderators cannot revoke.
    assert_eq!(
        f.post(
            &format!("/manage/verifications/{A}/revoke"),
            &moderator,
            &[("reason", "Not allowed")]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.post(
            &format!("/manage/verifications/{A}/revoke"),
            &admin,
            &[("reason", "")]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let revoked = f
        .post(
            &format!("/manage/verifications/{A}/revoke"),
            &admin,
            &[("reason", "Credentials expired")],
        )
        .await;
    assert_eq!(revoked.location(), "/manage/verifications?done=revoked");
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_verification")
            .await,
        0
    );
    assert_eq!(f.logs("identity.verification.revoke").await, 1);
    // Revoking again: nothing to revoke, no log written.
    f.post(
        &format!("/manage/verifications/{A}/revoke"),
        &admin,
        &[("reason", "Again")],
    )
    .await;
    assert_eq!(f.logs("identity.verification.revoke").await, 1);
    f.services.cleanup().await.unwrap();
}

/// The edit page carries the `reviewed_at` of the verification it shows. After the verification is revoked in
/// another tab, saving the old edit form is a conflict and does not grant it again; an edit from a form read before
/// another administrator's change is refused the same way.
#[tokio::test]
async fn stale_verification_edit_forms_are_conflicts() {
    let f = Fixture::new().await;
    let admin = f.login(ADMIN).await;
    f.grant(A, "merchant", "Official Account of Example Cloud", None)
        .await;
    let reviewed_at = f
        .scalar(&format!(
            "SELECT reviewed_at FROM gonof_common_member_verification WHERE uid={A}"
        ))
        .await;
    let page = f
        .get(&format!("/manage/verifications/{A}"), Some(&admin))
        .await;
    let reviewed = reviewed_at.to_string();
    let edit = [
        ("category", "merchant"),
        ("title", "Example Cloud, renamed"),
        ("site_url", "https://site-7403.example.test/"),
        ("expires", ""),
        ("reviewed_at", reviewed.as_str()),
    ];
    // Another administrator changes the verification: the old form is refused.
    db::query("UPDATE gonof_common_member_verification SET title='Changed elsewhere',reviewed_at=reviewed_at+5 WHERE uid=?")
        .bind(A)
        .execute(f.db().executor())
        .await
        .unwrap();
    let stale = f
        .post(&format!("/manage/verifications/{A}"), &admin, &edit)
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert!(
        stale
            .body
            .contains("Please refresh and fill in the form again")
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_verification WHERE uid={A} AND title='Changed elsewhere'"
        ))
        .await,
        1
    );
    // Revoked in another tab: the old form does not grant the verification again.
    let revoked = f
        .post(
            &format!("/manage/verifications/{A}/revoke"),
            &admin,
            &[("reason", "Credentials expired")],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::SEE_OTHER);
    let stale = f
        .post(&format!("/manage/verifications/{A}"), &admin, &edit)
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_verification")
            .await,
        0
    );
    assert_eq!(f.logs("identity.verification.grant").await, 1);
    // The edit page these submissions stand for carried the review time.
    assert!(
        page.body
            .contains(&format!(r#"name="reviewed_at" value="{reviewed_at}""#)),
        "the edit form carries the review time"
    );
    // The page now has no verification, so its form carries no review time and grants a new one.
    let page = f
        .get(&format!("/manage/verifications/{A}"), Some(&admin))
        .await;
    assert!(!page.body.contains(r#"name="reviewed_at""#));
    let granted = f
        .post(&format!("/manage/verifications/{A}"), &admin, &edit[..4])
        .await;
    assert_eq!(granted.status, StatusCode::SEE_OTHER, "{}", granted.body);
    assert_eq!(f.logs("identity.verification.grant").await, 2);
    f.services.cleanup().await.unwrap();
}

/// The directory has the same scope as profile privacy and blocks; frozen and expired members are not
/// shown anywhere; floor badges are shown as usual; verification on the profile page comes
/// after the profile gate; linked websites carry nofollow ugc.
#[tokio::test]
async fn directory_badges_follow_privacy_freeze_and_expiry() {
    let f = Fixture::new().await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='members' WHERE uid={B}"
    ))
    .await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='private' WHERE uid={C}"
    ))
    .await;
    let tid_a = f.thread(A, "Thread by the verified merchant").await;
    let tid_c = f.thread(C, "Thread by the private profile member").await;
    let tid_d = f.thread(D, "Thread by the frozen member").await;
    let tid_e = f.thread(E, "Thread by the expired member").await;
    f.grant(A, "merchant", "TITLE_A Merchant", None).await;
    f.grant(B, "merchant", "TITLE_B Merchant", None).await;
    f.grant(C, "blogger", "TITLE_C Blogger", None).await;
    f.grant(D, "merchant", "TITLE_D Merchant", None).await;
    f.grant(E, "merchant", "TITLE_E Merchant", Some(f.now + 60))
        .await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid={D}"
    ))
    .await;
    f.execute(&format!(
        "UPDATE gonof_common_member_verification SET expires_at={} WHERE uid={E}",
        f.now - 1
    ))
    .await;

    let anonymous = f.get("/verified", None).await;
    assert_eq!(anonymous.status, StatusCode::OK);
    assert!(anonymous.body.contains("TITLE_A"), "{}", anonymous.body);
    for hidden in ["TITLE_B", "TITLE_C", "TITLE_D", "TITLE_E"] {
        assert!(
            !anonymous.body.contains(hidden),
            "anonymous visitors must not see {hidden}"
        );
    }
    assert!(anonymous.body.contains(&format!(
        "href=\"https://site-{A}.example.test/\" rel=\"nofollow ugc noopener noreferrer\" target=\"_blank\""
    )));
    let viewer = f.login(VIEWER).await;
    let logged_in = f.get("/verified", Some(&viewer)).await;
    assert!(logged_in.body.contains("TITLE_A") && logged_in.body.contains("TITLE_B"));
    assert!(!logged_in.body.contains("TITLE_C") && !logged_in.body.contains("TITLE_D"));
    f.execute(&format!(
        "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES({VIEWER},{A},{})",
        f.now
    ))
    .await;
    let blocking = f.get("/verified?category=merchant", Some(&viewer)).await;
    assert!(!blocking.body.contains("TITLE_A") && blocking.body.contains("TITLE_B"));
    assert_eq!(
        f.get("/verified?category=intern", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get("/verified?category=merchant&page=2", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Profile page: a public profile shows the verification and website; for an "only me" profile the gate stops
    // the request first, and the body has no verification note.
    let profile = f.get(&format!("/space/{A}"), None).await;
    assert!(profile.body.contains("data-verified-badge=\"merchant\""));
    assert!(profile.body.contains("TITLE_A Merchant"));
    assert!(
        profile
            .body
            .contains("rel=\"nofollow ugc noopener noreferrer\"")
    );
    let private = f.get(&format!("/space/{C}"), Some(&viewer)).await;
    assert_eq!(private.status, StatusCode::FORBIDDEN);
    assert!(!private.body.contains("TITLE_C"));
    assert!(
        !f.get(&format!("/space/{D}"), None)
            .await
            .body
            .contains("data-profile-verification")
    );

    // Floors: the verification badge is part of the byline, so authors with "only me" profiles show it as usual; frozen
    // and expired ones do not.
    let topic = f.get(&format!("/thread-{tid_a}"), None).await;
    assert!(
        topic
            .body
            .contains("<span class=\"tag tag--verified\" title=\"TITLE_A Merchant\" data-verified-badge=\"merchant\">Merchant</span>"),
        "{}",
        topic.body
    );
    assert!(
        f.get(&format!("/thread-{tid_c}"), None)
            .await
            .body
            .contains("data-verified-badge=\"blogger\"")
    );
    for tid in [tid_d, tid_e] {
        assert!(
            !f.get(&format!("/thread-{tid}"), None)
                .await
                .body
                .contains("data-verified-badge")
        );
    }
    // Restored automatically after unfreezing, without granting again.
    f.execute(&format!(
        "UPDATE gonof_common_member SET `freeze`=0 WHERE uid={D}"
    ))
    .await;
    assert!(
        f.get(&format!("/thread-{tid_d}"), None)
            .await
            .body
            .contains("data-verified-badge=\"merchant\"")
    );
    f.services.cleanup().await.unwrap();
}

/// Applications: replaying the same request key yields one row; at most 2 in flight per account;
/// each website unique (only one even under concurrency); a 7-day cooldown after rejection; account age, e-mail
/// verification, read-only mode and the switch are all rechecked in the transaction; rate limits return 429.
#[tokio::test]
async fn link_applications_are_idempotent_limited_and_gated() {
    let f = Fixture::new().await;
    let a = f.login(A).await;
    let b = f.login(B).await;
    let admin = f.login(ADMIN).await;

    assert_eq!(
        f.get("/links/apply", None).await.status,
        StatusCode::OK,
        "anonymous visitors can read the notes"
    );
    assert!(
        f.get("/links/apply", None)
            .await
            .body
            .contains("Log in to apply")
    );
    let page = f.get("/links/apply", Some(&a)).await;
    assert!(page.body.contains("data-link-apply-form"));

    let key = identity::random_token();
    let first = f.apply(&a, "alpha.example.test", &key).await;
    assert_eq!(first.status, StatusCode::SEE_OTHER, "{}", first.body);
    assert_eq!(first.location(), "/links/apply?done=submitted");
    let replay = f.apply(&a, "alpha.example.test", &key).await;
    assert_eq!(replay.status, StatusCode::SEE_OTHER, "{}", replay.body);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid={A}"
        ))
        .await,
        1
    );
    assert_eq!(f.logs("site.links.apply").await, 1);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_friend_link WHERE domain='alpha.example.test' AND status='pending' AND live_domain='alpha.example.test'")
            .await,
        1
    );
    // Someone else applies for the same website (different case or www. still counts as the same): the unique key
    // refuses it.
    let taken = f
        .apply(&b, "WWW.Alpha.Example.Test", &identity::random_token())
        .await;
    assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        taken
            .body
            .contains("This site is already listed or under review"),
        "{}",
        taken.body
    );
    // A second one is allowed; a third exceeds the 2 per account.
    assert_eq!(
        f.apply(&a, "beta.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    let third = f
        .apply(&a, "gamma.example.test", &identity::random_token())
        .await;
    assert_eq!(third.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        third
            .body
            .contains("reached the limit of pending and listed links")
    );
    // Rejection frees the website, but nobody can apply for it again within 7 days.
    let alpha = f.link_id("alpha.example.test").await;
    let rejected = f
        .post(
            &format!("/manage/links/{alpha}"),
            &admin,
            &[("action", "reject"), ("reason", "Our link was not found")],
        )
        .await;
    assert_eq!(rejected.status, StatusCode::SEE_OTHER, "{}", rejected.body);
    let cooling = f
        .apply(&b, "alpha.example.test", &identity::random_token())
        .await;
    assert_eq!(cooling.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        cooling
            .body
            .contains("cannot apply again until the waiting period ends"),
        "{}",
        cooling.body
    );
    f.execute(&format!(
        "UPDATE gonof_common_friend_link SET handled_at={} WHERE id={alpha}",
        f.now - 8 * 86_400
    ))
    .await;
    assert_eq!(
        f.apply(&b, "alpha.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    // The applicant can see the rejection reason.
    assert!(
        f.get("/links/apply", Some(&a))
            .await
            .body
            .contains("Our link was not found")
    );
    // Two people race for the same new website concurrently: only one gets written.
    let c = f.login(C).await;
    let (key_b, key_c) = (identity::random_token(), identity::random_token());
    let (one, two) = tokio::join!(
        f.apply(&b, "race.example.test", &key_b),
        f.apply(&c, "race.example.test", &key_c),
    );
    let mut statuses = [one.status, two.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::SEE_OTHER, StatusCode::UNPROCESSABLE_ENTITY]
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_friend_link WHERE domain='race.example.test'")
            .await,
        1
    );

    // Eligibility: registered under 30 days, e-mail not verified, applying for this site itself, and the backlink must
    // be on the website applied for.
    let young = f.login(YOUNG).await;
    let too_new = f
        .apply(&young, "young.example.test", &identity::random_token())
        .await;
    assert!(
        too_new.body.contains("not old enough to apply yet"),
        "{}",
        too_new.body
    );
    let unverified = f.login(UNVERIFIED).await;
    assert!(
        f.apply(&unverified, "mail.example.test", &identity::random_token())
            .await
            .body
            .contains("Verify your email before applying")
    );
    let d = f.login(D).await;
    // This site's URL comes from the admin "forum URL" setting; the test configuration's base_url is an IP, so fill in
    // a domain first.
    f.execute(
        "UPDATE gonof_common_settings SET svalue='https://example.com' WHERE skey='settings.site.url'",
    )
    .await;
    let own = f
        .post(
            "/links/apply",
            &d,
            &[
                ("name", "This Site"),
                ("category", "forum"),
                ("url", "https://www.example.com/"),
                ("request_key", &identity::random_token()),
            ],
        )
        .await;
    assert_eq!(own.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        own.body.contains("You cannot apply with this site"),
        "{}",
        own.body
    );
    let backlink = f
        .post(
            "/links/apply",
            &d,
            &[
                ("name", "Wrong Backlink"),
                ("category", "forum"),
                ("url", "https://delta.example.test/"),
                ("backlink_url", "https://elsewhere.example.test/links"),
                ("request_key", &identity::random_token()),
            ],
        )
        .await;
    assert!(
        backlink
            .body
            .contains("The page with our link must be on the site you are applying with"),
        "{}",
        backlink.body
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid IN ({YOUNG},{UNVERIFIED},{D})"
        ))
        .await,
        0
    );

    // Not accepted during read-only mode; not accepted when applications are closed, and the directory and home page
    // show no application entry.
    f.execute(
        "UPDATE gonof_common_settings SET svalue='true' WHERE skey='settings.site.read_only'",
    )
    .await;
    assert_eq!(
        f.apply(&d, "readonly.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    f.execute(
        "UPDATE gonof_common_settings SET svalue='false' WHERE skey='settings.site.read_only'",
    )
    .await;
    f.execute(
        "UPDATE gonof_common_settings SET svalue='false' WHERE skey='settings.links.applications'",
    )
    .await;
    let closed = f
        .apply(&d, "closed.example.test", &identity::random_token())
        .await;
    assert!(
        closed.body.contains("Link applications are closed for now"),
        "{}",
        closed.body
    );
    assert!(!f.get("/links", None).await.body.contains("/links/apply"));
    assert!(!f.get("/", None).await.body.contains("/links/apply"));
    assert!(
        f.get("/links/apply", Some(&d))
            .await
            .body
            .contains("data-link-apply-closed")
    );
    f.execute(
        "UPDATE gonof_common_settings SET svalue='true' WHERE skey='settings.links.applications'",
    )
    .await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid={D}"
        ))
        .await,
        0
    );

    // Rate limit: 5 per hour per account, the 6th gets 429 (the token is checked before counting, so failed submissions
    // count too). Submitting from another IP only hits the account limit.
    let limited = f.login(F).await;
    let token = identity::random_token();
    let invalid = [("name", ""), ("request_key", token.as_str())];
    for _ in 0..5 {
        let reply = f
            .post_from("/links/apply", &limited, &invalid, "192.0.2.99")
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let key = identity::random_token();
    let valid = [
        ("name", "Rate Limited"),
        ("category", "blog"),
        ("url", "https://limited.example.test/"),
        ("request_key", key.as_str()),
    ];
    assert_eq!(
        f.post_from("/links/apply", &limited, &valid, "192.0.2.99")
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 20 per hour per IP: on a new IP, four accounts each submit 5 invalid ones (each account's quota exactly used up),
    // and B's submission on this IP (with account quota left) is refused because the IP limit is exceeded.
    let ip = "192.0.2.77";
    for uid in [VIEWER, MOD, ADMIN, E] {
        let session = f.login(uid).await;
        for _ in 0..5 {
            let reply = f.post_from("/links/apply", &session, &invalid, ip).await;
            assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
    }
    let key = identity::random_token();
    let fresh = [
        ("name", "IP Rate Limited"),
        ("category", "blog"),
        ("url", "https://ip-limited.example.test/"),
        ("request_key", key.as_str()),
    ];
    assert_eq!(
        f.post_from("/links/apply", &b, &fresh, ip).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // A signed-out submission redirects to login.
    let anonymous = f
        .send(
            Request::builder()
                .method("POST")
                .uri("/links/apply")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("name=x"))
                .unwrap(),
        )
        .await;
    assert!(
        anonymous.status == StatusCode::SEE_OTHER || anonymous.status == StatusCode::FORBIDDEN,
        "{}",
        anonymous.status
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_friend_link WHERE domain IN ('limited.example.test','ip-limited.example.test')")
            .await,
        0
    );
    f.services.cleanup().await.unwrap();
}

/// Admin review: administrators only; approved links enter the directory and the home page sidebar (at
/// most 12), with rel nofollow noopener + referrerpolicy=origin; repeated handling does not write twice;
/// taking down or withdrawing frees the website.
#[tokio::test]
async fn admins_review_links_and_the_pages_render_them_safely() {
    let f = Fixture::new().await;
    let a = f.login(A).await;
    let viewer = f.login(VIEWER).await;
    let moderator = f.login(MOD).await;
    let admin = f.login(ADMIN).await;
    assert_eq!(
        f.apply(&a, "approved.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    let id = f.link_id("approved.example.test").await;
    let approve = [
        ("action", "approve"),
        ("category", "tools"),
        ("name", "Approved Site"),
        ("url", "https://approved.example.test/"),
        ("description", "A one-line description"),
        ("displayorder", "5"),
        ("home", "true"),
    ];
    for session in [&viewer, &moderator] {
        assert_eq!(
            f.get("/manage/links", Some(session)).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            f.post(&format!("/manage/links/{id}"), session, &approve)
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
    assert!(matches!(
        links::decide(
            f.db(),
            &f.services.global,
            actor(MOD),
            id,
            links::Decision::Reject("Not allowed".into()),
            f.now
        )
        .await,
        Err(IdentityError::Forbidden)
    ));
    let queue = f.get("/manage/links?status=pending", Some(&admin)).await;
    assert!(queue.body.contains("Site approved.example.test"));
    let done = f
        .post(&format!("/manage/links/{id}"), &admin, &approve)
        .await;
    assert_eq!(done.location(), format!("/manage/links/{id}?done=saved"));
    let again = f
        .post(&format!("/manage/links/{id}"), &admin, &approve)
        .await;
    assert_eq!(
        again.location(),
        format!("/manage/links/{id}?done=unchanged")
    );
    assert_eq!(f.logs("site.links.approve").await, 1);

    let attributes = "href=\"https://approved.example.test/\" rel=\"nofollow noopener\" referrerpolicy=\"origin\" target=\"_blank\"";
    let link = format!("<a {attributes}");
    let directory = f.get("/links", None).await;
    assert!(
        directory.body.contains(&format!(
            "<a class=\"link-name\" {attributes} data-friend-link>Approved Site</a>"
        )),
        "{}",
        directory.body
    );
    assert!(
        directory.body.contains("data-link-group=\"Tools\"")
            && directory.body.contains("A one-line description")
    );
    let home = f.get("/", None).await;
    assert!(
        home.body.contains(&format!("{link}>Approved Site</a>")),
        "{}",
        home.body
    );
    assert!(home.body.contains("href=\"/links\">All links</a>"));
    // The search page (same home page template) does not show this card.
    let search = f.get("/search?q=x", None).await;
    assert_eq!(search.status, StatusCode::OK);
    assert!(!search.body.contains("data-friend-links"));

    // At most 12 on the home page: add 12 more home page links in the admin area; lower sort values come first.
    for n in 0..12 {
        let created = f
            .post(
                "/manage/links",
                &admin,
                &[
                    ("category", "blog"),
                    ("name", &format!("Home Link {n:02}")),
                    ("url", &format!("https://home{n}.example.test/")),
                    ("displayorder", &format!("{}", n - 20)),
                    ("home", "true"),
                ],
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
    }
    let home = f.get("/", None).await;
    assert_eq!(home.body.matches("referrerpolicy=\"origin\"").count(), 12);
    assert!(
        !home.body.contains("Approved Site</a>"),
        "sort value 5 is pushed out of the first 12"
    );
    assert_eq!(f.logs("site.links.add").await, 12);
    // Adding the same website: the unique key refuses it.
    let duplicate = f
        .post(
            "/manage/links",
            &admin,
            &[
                ("category", "blog"),
                ("name", "Duplicate"),
                ("url", "https://www.home0.example.test/other"),
            ],
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.body);

    // Edit a link on display, then take it down (reason required): it disappears from the directory and the website is
    // freed for new applications.
    let update = f
        .post(
            &format!("/manage/links/{id}"),
            &admin,
            &[
                ("action", "update"),
                ("category", "forum"),
                ("name", "Renamed Site"),
                ("url", "https://approved.example.test/"),
                ("displayorder", "5"),
            ],
        )
        .await;
    assert_eq!(update.status, StatusCode::SEE_OTHER, "{}", update.body);
    assert!(f.get("/links", None).await.body.contains("Renamed Site"));
    assert_eq!(
        f.post(
            &format!("/manage/links/{id}"),
            &admin,
            &[("action", "remove"), ("reason", "")]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let removed = f
        .post(
            &format!("/manage/links/{id}"),
            &admin,
            &[("action", "remove"), ("reason", "They removed our link")],
        )
        .await;
    assert_eq!(removed.status, StatusCode::SEE_OTHER, "{}", removed.body);
    assert!(!f.get("/links", None).await.body.contains("Renamed Site"));
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_friend_link WHERE id={id} AND status='removed' AND live_domain IS NULL"
        ))
        .await,
        1
    );
    // The applicant withdraws their pending one and takes down their displayed one; others' cannot be touched.
    assert_eq!(
        f.apply(&a, "mine.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    let mine = f.link_id("mine.example.test").await;
    assert_eq!(
        f.post(&format!("/links/apply/{mine}/withdraw"), &viewer, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let withdrawn = f
        .post(&format!("/links/apply/{mine}/withdraw"), &a, &[])
        .await;
    assert_eq!(withdrawn.location(), "/links/apply?done=withdrawn");
    assert_eq!(
        f.post(&format!("/links/apply/{mine}/withdraw"), &a, &[])
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(f.logs("site.links.withdraw").await, 1);
    assert_eq!(
        f.apply(&a, "approved.example.test", &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER,
        "a removed link frees the website for a new application"
    );
    // Admin tabs: verified members are under user management, friend links under interface settings.
    let manage = f.get("/manage/links", Some(&admin)).await;
    assert!(manage.body.contains("Link guidelines") && manage.body.contains("Brand images"));
    assert_eq!(
        f.get("/manage/policies/links", Some(&admin)).await.status,
        StatusCode::OK
    );
    f.services.cleanup().await.unwrap();
}

/// The explanation documents are editable in the admin area, and the front end uses the new version after saving; the
/// footer has two entry points.
/// "My applications" always lists the member's live applications: an old active link still taking up one of the
/// member's places stays on the page, with its take-down button, however many withdrawn applications came after it.
#[tokio::test]
async fn my_applications_keep_live_links_in_view() {
    let f = Fixture::new().await;
    let row = |status: &str, domain: String, n: i64| {
        let live = if status == "active" {
            format!("'{domain}'")
        } else {
            "NULL".to_owned()
        };
        format!(
            "('{status}','blog','Site {domain}','https://{domain}/','{domain}',{live},{C},'mine-{n}',{now},{now})",
            now = f.now
        )
    };
    let mut values = vec![row("active", "kept.example.test".into(), 0)];
    values.extend(
        (1..=links::MINE_LIMIT as i64)
            .map(|n| row("withdrawn", format!("gone-{n}.example.test"), n)),
    );
    f.execute(&format!(
        "INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,owner_uid,request_key,dateline,updated_at) VALUES {}",
        values.join(",")
    ))
    .await;
    let kept = f.link_id("kept.example.test").await;
    let member = f.login(C).await;
    let page = f.get("/links/apply", Some(&member)).await;
    assert!(
        page.body.contains("data-my-link=\"active\""),
        "{}",
        page.body
    );
    let take_down = format!("/links/apply/{kept}/withdraw");
    assert!(page.body.contains(&take_down));
    assert_eq!(
        page.body.matches("data-my-link=\"withdrawn\"").count(),
        links::MINE_LIMIT
    );
    f.post(&take_down, &member, &[]).await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_friend_link WHERE id={kept} AND status='removed'"
        ))
        .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

/// The last directory place goes to one administrator (MySQL): with 499 active links, an approval and a direct addition
/// by two administrators at the same time leave 500, not 501. Another connection holds the added domain in an open
/// transaction, so the addition waits inside its own transaction after counting while the approval runs.
#[tokio::test]
async fn the_last_directory_place_goes_to_one_administrator() {
    gono_forum_web::mysql_only!();
    let f = Fixture::new().await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET adminid=1 WHERE uid={A}"
    ))
    .await;
    let values: Vec<String> = (0..links::DIRECTORY_LIMIT - 1)
        .map(|n| {
            format!(
                "('active','other','Site {n}','https://site-{n}.example.test/','site-{n}.example.test','site-{n}.example.test',{now},{now})",
                now = f.now
            )
        })
        .collect();
    f.execute(&format!(
        "INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,dateline,updated_at) VALUES {}",
        values.join(",")
    ))
    .await;
    f.execute(&format!(
        "INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,dateline,updated_at) VALUES('pending','blog','Waiting','https://waiting.example.test/','waiting.example.test','waiting.example.test',{now},{now})",
        now = f.now
    ))
    .await;
    let waiting = f.link_id("waiting.example.test").await;
    let input = |domain: &str| links::LinkInput {
        category: "blog".into(),
        name: format!("Site {domain}"),
        url: format!("https://{domain}/"),
        description: String::new(),
        home: false,
        displayorder: 0,
    };
    let mut holder = f.db().begin_write().await.unwrap();
    db::query("INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,dateline,updated_at) VALUES('pending','blog','Held','https://added.example.test/','added.example.test','added.example.test',1,1)")
        .execute(f.db().observe(&mut *holder))
        .await
        .unwrap();
    let added = tokio::spawn({
        let (db, global, now) = (f.db().clone(), f.services.global.clone(), f.now);
        let input = input("added.example.test");
        async move { links::create(&db, &global, actor(ADMIN), input, now).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let approved = tokio::spawn({
        let (db, global, now) = (f.db().clone(), f.services.global.clone(), f.now);
        let input = input("waiting.example.test");
        async move {
            links::decide(
                &db,
                &global,
                actor(A),
                waiting,
                links::Decision::Approve(input),
                now,
            )
            .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    holder.rollback().await.unwrap();
    let (added, approved) = (added.await.unwrap(), approved.await.unwrap());
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_friend_link WHERE status='active'")
            .await,
        links::DIRECTORY_LIMIT as i64
    );
    assert!(
        added.is_ok() != approved.is_ok(),
        "exactly one of them takes the place: {added:?} {approved:?}"
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn explainers_are_editable_and_the_footer_links_both_pages() {
    let f = Fixture::new().await;
    let admin = f.login(ADMIN).await;
    let default = f.get("/verified", None).await;
    assert!(default.body.contains("How to get verified"));
    // The footer links the two directories, one after the other.
    let verified = default
        .body
        .find("href=\"/verified\">Verified members</a>")
        .unwrap();
    let links = default.body.find("href=\"/links\">Links</a>").unwrap();
    assert!(verified < links);
    let saved = f
        .post(
            "/manage/policies/verified",
            &admin,
            &[
                ("title", "Verification notes"),
                ("body", "Write to **the staff**."),
            ],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let edited = f.get("/verified", None).await;
    assert!(
        edited.body.contains("Verification notes")
            && edited.body.contains("<strong>the staff</strong>")
    );
    f.post(
        "/manage/policies/links",
        &admin,
        &[("title", "Link notes"), ("body", "Add our link first.")],
    )
    .await;
    assert!(f.get("/links", None).await.body.contains("Link notes"));
    assert!(
        f.get("/links/apply", None)
            .await
            .body
            .contains("Add our link first")
    );
    f.services.cleanup().await.unwrap();
}
