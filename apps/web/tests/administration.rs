#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    administration as admin, content, db,
    identity::{self, Actor},
    runtime,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fixture() -> (TestFixture, Actor, Actor) {
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    content::seed_fixture(&f.db).await.unwrap();
    for (uid, role) in [(9901, 1), (9902, 0), (9903, 3)] {
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
            .bind(uid).bind(format!("admin-fixture-{uid}")).bind(format!("fixture-{uid}@example.test")).bind(role).execute(f.db.executor()).await.unwrap();
    }
    (
        f,
        Actor {
            uid: 9901,
            credentials_version: 1,
            api_key: None,
        },
        Actor {
            uid: 9902,
            credentials_version: 1,
            api_key: None,
        },
    )
}
async fn get(app: &Router, path: &str, cookie: Option<&str>) -> (StatusCode, String) {
    // Like a browser, every request names its host: the cookie names follow the connection's port.
    let mut req = Request::builder().uri(path).header(
        header::HOST,
        gono_forum_web::test_support::host_of(gono_forum_web::test_support::TEST_SITE_URL),
    );
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, format!("gonof_session__port58833={cookie}"));
    }
    let r = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    (
        r.status(),
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap(),
    )
}

async fn post(
    app: &Router,
    path: &str,
    session: &identity::SessionGrant,
    fields: &[(&str, String)],
) -> (StatusCode, String) {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    body.append_pair("_csrf", &session.csrf_token);
    for (name, value) in fields {
        body.append_pair(name, value);
    }
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(
            header::HOST,
            gono_forum_web::test_support::host_of(gono_forum_web::test_support::TEST_SITE_URL),
        )
        .header(
            header::COOKIE,
            format!("gonof_session__port58833={}", session.session_token),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.finish()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    (
        response.status(),
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap(),
    )
}
/// Each status filter of the invite list returns exactly the codes the list labels with that status: an available
/// or frozen code past its end time is "expired" (and not "available" or "frozen"), and a revoked code is "revoked"
/// whatever its end time. Totals follow the same set.
#[tokio::test]
async fn invite_filters_match_the_status_shown() {
    let (f, actor, _) = fixture().await;
    let now = identity::now();
    for (code, status, endtime, fuid) in [
        ("filter-available", "available", now + 3600, None),
        ("filter-available-late", "available", now - 3600, None),
        ("filter-frozen", "frozen", now + 3600, None),
        ("filter-frozen-late", "frozen", now - 3600, None),
        ("filter-revoked", "revoked", now + 3600, None),
        ("filter-revoked-late", "revoked", now - 3600, None),
        ("filter-used-late", "used", now - 3600, Some(9902_i64)),
    ] {
        db::query("INSERT INTO gonof_common_invite(uid,code,request_key,fuid,dateline,endtime,status) VALUES(9903,?,?,?,?,?,?)")
            .bind(code)
            .bind(code)
            .bind(fuid)
            .bind(now - 7200)
            .bind(endtime)
            .bind(status)
            .execute(f.db.executor())
            .await
            .unwrap();
    }
    let t = gono_forum_web::i18n::current();
    for (filter, label, expected) in [
        (
            "available",
            t.account_manage_invite_available(),
            &["filter-available"][..],
        ),
        (
            "frozen",
            t.account_manage_invite_frozen(),
            &["filter-frozen"][..],
        ),
        (
            "expired",
            t.account_manage_invite_expired(),
            &["filter-available-late", "filter-frozen-late"][..],
        ),
        (
            "revoked",
            t.account_manage_invite_revoked(),
            &["filter-revoked", "filter-revoked-late"][..],
        ),
        (
            "used",
            t.account_manage_invite_used(),
            &["filter-used-late"][..],
        ),
    ] {
        let (rows, total) = admin::invites(
            &f.db,
            actor,
            &admin::ListQuery {
                uid: Some(9903),
                status: filter.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let mut codes = Vec::new();
        for id in &ids {
            let code: String = db::query_scalar("SELECT code FROM gonof_common_invite WHERE id=?")
                .bind(id)
                .fetch_one(f.db.executor())
                .await
                .unwrap();
            codes.push(code);
        }
        codes.sort();
        assert_eq!(codes, expected, "{filter}");
        assert_eq!(total, expected.len() as i64, "{filter}");
        assert!(
            rows.iter().all(|row| row.status_label() == label),
            "{filter}: every row is labeled as filtered"
        );
    }
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn administrator_invites_are_bounded_idempotent_and_audited() {
    let (f, actor, member) = fixture().await;
    let key = identity::random_token();
    assert!(
        admin::create_invites(&f.db, member, 1, &key, "no permission")
            .await
            .is_err()
    );
    assert!(
        admin::create_invites(&f.db, actor, 101, &key, "over the limit")
            .await
            .is_err()
    );
    admin::create_invites(&f.db, actor, 21, &key, "batch invite")
        .await
        .unwrap();
    admin::create_invites(&f.db, actor, 21, &key, "batch invite")
        .await
        .unwrap();
    assert!(
        admin::create_invites(&f.db, actor, 20, &key, "batch invite")
            .await
            .is_err()
    );
    let (first, total) = admin::invites(
        &f.db,
        actor,
        &admin::ListQuery {
            uid: Some(actor.uid),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 21);
    assert_eq!(first.len(), 20);
    assert_eq!(
        admin::invites(
            &f.db,
            actor,
            &admin::ListQuery {
                uid: Some(actor.uid),
                page: Some(2),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .0
        .len(),
        1
    );
    let id = first[0].id;
    let freeze_key = identity::random_token();
    let auth = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        f.global.clone(),
    );
    let administrator = http_auth::login(&auth, &f.db, actor.uid).await;
    let app = gono_forum_web::router::build(f.config.clone(), &f.root, f.db.clone());
    let (status, html) = post(
        &app,
        &format!("/manage/invites/{id}"),
        &administrator,
        &[
            ("expected", "available".into()),
            ("desired", "frozen".into()),
            ("request_key", freeze_key.clone()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{html}");
    assert!(
        admin::change_invite(
            &f.db,
            actor,
            id,
            "available",
            "revoked",
            &identity::random_token(),
        )
        .await
        .is_err()
    );
    admin::change_invite(
        &f.db,
        actor,
        id,
        "frozen",
        "available",
        &identity::random_token(),
    )
    .await
    .unwrap();
    admin::change_invite(&f.db, actor, id, "available", "frozen", &freeze_key)
        .await
        .unwrap();
    assert_eq!(
        db::query_scalar::<String>("SELECT status FROM gonof_common_invite WHERE id=?")
            .bind(id)
            .fetch_one(f.db.executor())
            .await
            .unwrap(),
        "available",
        "replaying an old freeze request must not override the later unfreeze"
    );
    assert!(
        admin::change_invite(&f.db, actor, id, "available", "revoked", &freeze_key)
            .await
            .is_err()
    );
    admin::change_invite(
        &f.db,
        actor,
        id,
        "available",
        "revoked",
        &identity::random_token(),
    )
    .await
    .unwrap();
    assert!(
        admin::change_invite(
            &f.db,
            actor,
            id,
            "frozen",
            "available",
            &identity::random_token(),
        )
        .await
        .is_err()
    );
    runtime::update(&f.db, &f.global, actor, "site.settings.save", |s| {
        s.invite.enabled = false;
    })
    .await
    .unwrap();
    assert!(
        admin::create_invites(&f.db, actor, 1, &identity::random_token(), "invites closed",)
            .await
            .is_err()
    );
    let (logs, _) = gono_forum_web::manage::logs_filtered(
        &f.db,
        actor,
        &gono_forum_web::manage::LogFilter {
            kind: "invite".into(),
            operator: actor.uid.to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(logs.len(), 4);
    let changes: Vec<_> = logs
        .iter()
        .filter(|log| log.operation == "invite.status.change")
        .map(|log| (log.result.as_str(), log.reason.as_str()))
        .collect();
    assert_eq!(
        changes,
        [("revoked", ""), ("available", ""), ("frozen", "")]
    );
    assert!(logs.iter().all(|log| !log.reason.contains(&key)));
    let moderator = Actor {
        uid: 9903,
        credentials_version: 1,
        api_key: None,
    };
    assert_eq!(
        gono_forum_web::manage::logs_filtered(
            &f.db,
            moderator,
            &gono_forum_web::manage::LogFilter {
                kind: "invite".into(),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .1,
        0
    );
    f.cleanup().await.unwrap();
}
#[tokio::test]
async fn site_settings_documents_and_safe_task_retries() {
    let (f, actor, member) = fixture().await;
    assert!(
        runtime::update(&f.db, &f.global, member, "site.settings.save", |_| {})
            .await
            .is_err(),
        "a non-administrator cannot save settings"
    );
    runtime::update(&f.db, &f.global, actor, "site.settings.save", |s| {
        s.signup.mode = "open".into();
        s.site.read_only = true;
    })
    .await
    .unwrap();
    let stored = runtime::read(&f.db).await.unwrap();
    assert!(stored.site.read_only && stored.signup.mode == "open");
    // No optimistic locking: the later save overwrites the earlier one, but only the fields changed on this page (saves
    // are per tab and only write rows that changed).
    runtime::update(&f.db, &f.global, actor, "site.settings.save", |s| {
        s.signup.mode = "invite".into();
        s.site.read_only = false;
    })
    .await
    .unwrap();
    let restored = runtime::read(&f.db).await.unwrap();
    assert!(!restored.site.read_only && restored.signup.mode == "invite");
    let navigation = runtime::Document {
        title: "Navigation".into(),
        body: "Discussion | forum:1\nRules | /rules".into(),
        ..Default::default()
    };
    let saved = runtime::save_document(&f.db, &f.global, actor, "navigation", navigation.clone())
        .await
        .unwrap();
    assert!(saved.updated_at > 0);
    // Likewise, saving the same navigation again no longer reports a version conflict.
    runtime::save_document(&f.db, &f.global, actor, "navigation", navigation)
        .await
        .unwrap();
    assert!(
        runtime::save_document(
            &f.db,
            &f.global,
            actor,
            "navigation",
            runtime::Document {
                body: "Invalid | forum:999999".into(),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    let now = identity::now();
    let id=db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,status,attempts,available_at,lease_until,dateline) VALUES(?,'email.verify','member',?,'{\"secret\":\"never-expose\"}','pending',2,?,0,?)")
        .bind(identity::random_token()).bind(member.uid).bind(now+600).bind(now).execute(f.db.executor()).await.unwrap().last_insert_id() as i64;
    admin::retry_task(
        &f.db,
        actor,
        id,
        2,
        now + 600,
        0,
        "pending",
        "service restored",
    )
    .await
    .unwrap();
    assert!(
        admin::retry_task(
            &f.db,
            actor,
            id,
            2,
            now + 600,
            0,
            "pending",
            "stale task snapshot"
        )
        .await
        .is_err()
    );
    let tasks = admin::tasks(
        &f.db,
        actor,
        &admin::ListQuery {
            uid: Some(id),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert_eq!(tasks[0].attempts, 2);
    assert!(tasks[0].error.is_empty());
    db::query("UPDATE gonof_common_outbox SET status='sent' WHERE eventid=?")
        .bind(id)
        .execute(f.db.executor())
        .await
        .unwrap();
    assert!(
        admin::retry_task(
            &f.db,
            actor,
            id,
            2,
            tasks[0].available_at,
            0,
            "sent",
            "resend forbidden"
        )
        .await
        .is_err()
    );
    db::query("UPDATE gonof_common_outbox SET status='failed',last_error='unsafe secret detail' WHERE eventid=?").bind(id).execute(f.db.executor()).await.unwrap();
    assert!(
        admin::retry_task(
            &f.db,
            actor,
            id,
            2,
            tasks[0].available_at,
            0,
            "failed",
            "a refused task cannot be resent"
        )
        .await
        .is_err()
    );
    let failed = admin::tasks(
        &f.db,
        actor,
        &admin::ListQuery {
            uid: Some(id),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert!(!failed[0].can_retry());
    assert!(!failed[0].error.contains("unsafe"));
    assert_eq!(failed[0].status_label(), "Stopped");
    // A failed task whose retries are exhausted can be scheduled for retry, and the page only
    // shows a redacted label.
    db::query("UPDATE gonof_common_outbox SET attempts=20,last_error=? WHERE eventid=?")
        .bind(gono_forum_web::outbox::exhausted_error(
            20,
            "unsafe secret detail",
        ))
        .bind(id)
        .execute(f.db.executor())
        .await
        .unwrap();
    let exhausted = admin::tasks(
        &f.db,
        actor,
        &admin::ListQuery {
            uid: Some(id),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .0;
    assert!(exhausted[0].can_retry());
    assert_eq!(exhausted[0].status_label(), "Out of retries");
    assert!(
        !exhausted[0].error.contains("unsafe") && !exhausted[0].error.contains("gave up after")
    );
    admin::retry_task(
        &f.db,
        actor,
        id,
        20,
        exhausted[0].available_at,
        0,
        "failed",
        "SMTP fixed",
    )
    .await
    .unwrap();
    let status: String = db::query_scalar("SELECT status FROM gonof_common_outbox WHERE eventid=?")
        .bind(id)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(status, "pending");
    let attempts: i64 =
        db::query_scalar("SELECT attempts FROM gonof_common_outbox WHERE eventid=?")
            .bind(id)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(attempts, 0, "a retry starts counting again");
    db::query("UPDATE gonof_common_member SET account_state='deleting' WHERE uid=?")
        .bind(actor.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    assert!(
        admin::users(&f.db, actor, &Default::default())
            .await
            .is_err()
    );
    f.cleanup().await.unwrap();
}
/// Admin forms keep only validation and conflict messages inside the form; database and other internal failures go to
/// the common error page (500, logged, redacted) instead of being stuffed into the form as "account service error: ..."
/// with 422/409.
#[tokio::test]
async fn admin_forms_send_internal_failures_to_the_error_page() {
    let (f, actor, _) = fixture().await;
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let administrator = http_auth::login(&identity, &f.db, actor.uid).await;
    let app = gono_forum_web::router::build(f.config.clone(), &f.root, f.db.clone());
    let template = |body: &str| {
        vec![
            ("subject", "Hello {{username}}".to_owned()),
            ("body", body.to_owned()),
            ("operation", "save".to_owned()),
        ]
    };
    // A text validation failure is still an in-form message.
    let (status, html) = post(
        &app,
        "/manage/mail/templates/content",
        &administrator,
        &template("No link here"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("The body must keep the"), "{html}");

    // The audit table is temporarily unavailable: both invite code generation and mail template saving hit a database
    // error.
    db::query("RENAME TABLE gonof_common_log TO gonof_common_log_offline")
        .sqlite("ALTER TABLE gonof_common_log RENAME TO gonof_common_log_offline")
        .execute(f.db.executor())
        .await
        .unwrap();
    let invite = post(
        &app,
        "/manage/invites",
        &administrator,
        &[
            ("request_key", identity::random_token()),
            ("count", "1".into()),
            ("reason", "internal error".into()),
        ],
    )
    .await;
    let mail = post(
        &app,
        "/manage/mail/templates/content",
        &administrator,
        &template("Please open {{link}}"),
    )
    .await;
    db::query("RENAME TABLE gonof_common_log_offline TO gonof_common_log")
        .sqlite("ALTER TABLE gonof_common_log_offline RENAME TO gonof_common_log")
        .execute(f.db.executor())
        .await
        .unwrap();
    // The failure is the common error page, not the form again with the error inside it.
    for (name, (status, html), field) in [
        ("invite", invite, "name=\"count\""),
        ("mail template", mail, "name=\"subject\""),
    ] {
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{name}: {html}");
        assert!(html.contains("data-error-message"), "{name}: {html}");
        assert!(!html.contains(field), "{name}: {html}");
    }
    f.cleanup().await.unwrap();
}

/// Brand image saving is split the same way as the admin forms above: validation and conflicts stay in the form;
/// database and other internal errors produce the 500 error page, with the raw text only in the log. Previously every
/// failure was stuffed into the form as 422, and internal errors showed as "The attachment service is temporarily
/// unavailable". A nonexistent image ID used to find no row and become an internal error; now it is a validation error
/// (`router/site.rs`).
#[tokio::test]
async fn brand_forms_keep_validation_in_the_form_and_send_internal_failures_to_the_error_page() {
    let (f, actor, _) = fixture().await;
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let administrator = http_auth::login(&identity, &f.db, actor.uid).await;
    let app = gono_forum_web::router::build(f.config.clone(), &f.root, f.db.clone());
    let save =
        |aid: &str, version: &str| vec![("aid", aid.to_owned()), ("version", version.to_owned())];
    let (status, html) = post(
        &app,
        "/manage/branding/logo",
        &administrator,
        &save("999999", "0"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{html}");
    assert!(
        html.contains("Choose an image you uploaded that passed its check"),
        "{html}"
    );
    let (status, html) = post(
        &app,
        "/manage/branding/logo",
        &administrator,
        &save("0", "7"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{html}");
    assert!(
        html.contains("The brand image was changed meanwhile"),
        "{html}"
    );

    db::query("RENAME TABLE gonof_common_site_asset TO gonof_common_site_asset_offline")
        .sqlite("ALTER TABLE gonof_common_site_asset RENAME TO gonof_common_site_asset_offline")
        .execute(f.db.executor())
        .await
        .unwrap();
    let failed = post(
        &app,
        "/manage/branding/logo",
        &administrator,
        &save("0", "0"),
    )
    .await;
    db::query("RENAME TABLE gonof_common_site_asset_offline TO gonof_common_site_asset")
        .sqlite("ALTER TABLE gonof_common_site_asset_offline RENAME TO gonof_common_site_asset")
        .execute(f.db.executor())
        .await
        .unwrap();
    let (status, html) = failed;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{html}");
    assert!(
        html.contains("The attachment service is temporarily unavailable"),
        "{html}"
    );
    assert!(
        !html.contains("gonof_common_site_asset"),
        "the page must not carry the raw error"
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn management_pages_runtime_branding_and_public_metadata_are_connected() {
    let (f, actor, member) = fixture().await;
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let administrator = http_auth::login(&identity, &f.db, actor.uid).await;
    let ordinary = http_auth::login(&identity, &f.db, member.uid).await;
    let app = gono_forum_web::router::build_with_global(
        f.config.clone(),
        &f.root,
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    );
    // Brand images skip the session middleware: anonymous requests are not issued a CSRF cookie either, and the cache
    // header is not rewritten to private.
    let brand = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/brand/logo/999999/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(brand.status(), StatusCode::NOT_FOUND);
    assert!(brand.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(brand.headers()[header::CACHE_CONTROL], "no-store");
    for path in [
        "/manage",
        "/manage/users?role=&uid=&page=",
        "/manage/users/9902",
        "/manage/invites",
        "/manage/forums",
        "/manage/settings",
        "/manage/navigation",
        "/manage/policies/terms",
        "/manage/policies/privacy",
        "/manage/policies/rules",
        "/manage/branding",
        "/manage/mail/templates/content",
        "/manage/tasks",
        "/manage/logs?kind=site",
        "/manage/data-requests",
    ] {
        let (status, html) = get(&app, path, Some(&administrator.session_token)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {html}");
        assert!(html.contains("Sample Forum"), "{path}");
        assert_eq!(
            get(&app, path, Some(&ordinary.session_token)).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    // An account without an e-mail address (as the initial administrator is) leaves no empty leading segment in the
    // user list.
    db::query("INSERT INTO gonof_common_member(uid,username,password,adminid,regdate) VALUES(9950,'no-email-admin','synthetic-hash',1,1)")
        .execute(f.db.executor())
        .await
        .unwrap();
    let (_, html) = get(
        &app,
        "/manage/users?q=no-email-admin",
        Some(&administrator.session_token),
    )
    .await;
    assert!(html.contains("no-email-admin"), "{html}");
    assert!(
        !html.contains("<p> · "),
        "a user row without an email must not start with an empty segment"
    );
    // The gate for brand image uploads is on AdminPage, and AdminPage is FromRequestParts, which
    // axum guarantees runs before the Multipart that takes the body: a regular member is stopped before even reading
    // the form, regardless of whether the request body is valid multipart.
    assert_eq!(
        post(&app, "/manage/branding/upload", &ordinary, &[])
            .await
            .0,
        StatusCode::FORBIDDEN,
        "a regular member must not upload brand images"
    );
    for path in [
        "/",
        "/thread-103",
        "/ai",
        "/terms",
        "/privacy",
        "/rules",
        "/my/data",
        "/my/preferences",
        "/my/blocked-users",
        "/my/following/threads",
        "/my/following/forums",
    ] {
        let (status, html) = get(&app, path, Some(&ordinary.session_token)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {html}");
        if path == "/thread-103" {
            assert!(html.contains("/api/thread-103/follow"));
        }
    }
    let batch_key = identity::random_token();
    let (status, failed) = post(
        &app,
        "/manage/invites",
        &administrator,
        &[
            ("request_key", batch_key.clone()),
            ("count", "101".into()),
            ("reason", "Kept invite reason".into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(failed.contains("value=\"101\""));
    assert!(failed.contains(&batch_key) && failed.contains("Kept invite reason"));
    let (status, failed) = post(
        &app,
        "/manage/navigation",
        &administrator,
        &[
            ("title", "Kept navigation title".into()),
            ("body", "Invalid | https://example.com".into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(failed.contains("Kept navigation title") && failed.contains("https://example.com"));
    assert!(
        get(&app, "/robots.txt", None)
            .await
            .1
            .contains("Disallow: /")
    );
    assert!(
        !get(&app, "/sitemap.xml", None)
            .await
            .1
            .contains("thread-103")
    );
    runtime::update(&f.db, &f.global, actor, "site.settings.save", |s| {
        s.signup.mode = "open".into();
        s.seo.indexing = true;
    })
    .await
    .unwrap();
    let register = get(&app, "/register", None).await.1;
    assert!(register.contains("Sign-ups are open; an invite code is optional"));
    // Open sign-up: the invite field is optional.
    let invite = register
        .split_once("name=\"invite\"")
        .expect("the sign-up page has an invite field")
        .1;
    assert!(!invite[..invite.find('>').unwrap()].contains("required"));
    assert!(
        get(&app, "/sitemap.xml?page=1", None)
            .await
            .1
            .contains("thread-103")
    );
    // Withdrawn threads are not in the sitemap: both markers, first post withdrawn and thread withdrawn, must be
    // excluded.
    for (withdraw, restore) in [
        (
            "UPDATE gonof_forum_post SET deleted_at=1 WHERE tid=103 AND first=1",
            "UPDATE gonof_forum_post SET deleted_at=0 WHERE tid=103 AND first=1",
        ),
        (
            "UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid=103",
            "UPDATE gonof_forum_thread SET deleted_at=0 WHERE tid=103",
        ),
    ] {
        db::query(withdraw).execute(f.db.executor()).await.unwrap();
        let (status, xml) = get(&app, "/sitemap.xml?page=1", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!xml.contains("/thread-103<"), "{withdraw}");
        db::query(restore).execute(f.db.executor()).await.unwrap();
    }
    // Pages are split by thread ID, and the page count is determined by the largest ID: fixture IDs are all below 5000,
    // so there is only 1 page.
    let max_tid: i64 = db::query_scalar("SELECT MAX(tid) FROM gonof_forum_thread")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert!(max_tid < 5000, "fixture thread IDs must all be on page 1");
    let index = get(&app, "/sitemap.xml", None).await.1;
    assert_eq!(index.matches("<sitemap>").count(), 1);
    assert_eq!(
        get(&app, "/sitemap.xml?page=2", None).await.0,
        StatusCode::NOT_FOUND
    );
    db::query("UPDATE gonof_forum_forum SET visibility='members' WHERE fid=1")
        .execute(f.db.executor())
        .await
        .unwrap();
    // After changing the forum with raw SQL, sync by the production rules, then clear this process's forum cache.
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    f.global.clear_local();
    let anonymous = get(&app, "/sitemap.xml?page=1", None).await.1;
    let tid: i64 = db::query_scalar("SELECT MIN(tid) FROM gonof_forum_thread WHERE fid=1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert!(!anonymous.contains(&format!("thread-{tid}<")));
    let parent=db::query("INSERT INTO gonof_forum_forum(name,slug,visibility,description,guidance) VALUES('Members Parent','seo-members-parent','members','','')").execute(f.db.executor()).await.unwrap().last_insert_id() as i64;
    let child_fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=103")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    db::query("UPDATE gonof_forum_forum SET parent_fid=? WHERE fid=?")
        .bind(parent)
        .bind(child_fid)
        .execute(f.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    f.global.clear_local();
    let (_, xml) = get(&app, "/sitemap.xml?page=1", None).await;
    assert!(
        !xml.contains("thread-103</loc>") && !xml.contains("seo-members-parent</loc>"),
        "the sitemap must not list forums and threads that inherit members-only visibility"
    );
    runtime::update(&f.db, &f.global, actor, "site.settings.save", |s| {
        s.signup.mode = "closed".into();
    })
    .await
    .unwrap();
    assert!(
        get(&app, "/register", None)
            .await
            .1
            .contains("Sign-ups are closed for now")
    );
    let (_, manifest) = get(&app, "/site.webmanifest", None).await;
    assert!(manifest.contains("/static/img/app-192.png"));
    f.cleanup().await.unwrap();
}

/// All three sections of the site settings page must actually take effect: the URL goes into external links, the time
/// zone and date format into display, and the contact e-mail is stored but not shown.
#[tokio::test]
async fn site_settings_take_effect_on_links_and_timestamps() {
    let (f, actor, _) = fixture().await;
    let identity = identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let administrator = http_auth::login(&identity, &f.db, actor.uid).await;
    let app = gono_forum_web::router::build(f.config.clone(), &f.root, f.db.clone());

    // The fields of all three sections are on the page; registration, invites, API, indexing and description have moved
    // to their own tabs.
    let (status, html) = get(&app, "/manage/settings", Some(&administrator.session_token)).await;
    assert_eq!(status, StatusCode::OK);
    for field in [
        "name=\"contact_email\"",
        "name=\"site_url\"",
        "name=\"timezone_offset\"",
        "name=\"date_format\"",
        "name=\"relative_hours\"",
    ] {
        assert!(html.contains(field), "settings page lacks field {field}");
    }
    // The brand words are settings (docs/architecture.md, "Languages"): the header brand text, the points name and
    // the activity index name are editable fields, not a read-only header brand.
    for field in [
        "name=\"brand_text\" value=\"Sample Forum\"",
        "name=\"credit_name\" value=\"Coins\"",
        "name=\"index_name\" value=\"Popularity Index\"",
    ] {
        assert!(
            html.contains(field),
            "settings page lacks brand word {field}"
        );
    }
    assert!(!html.contains("readonly"));

    // Each section is named by its own heading, so a screen reader landing in it can announce which section it is.
    for anchor in ["section-site", "section-domains", "section-time"] {
        assert!(
            html.contains(&format!(
                "id=\"{anchor}\" aria-labelledby=\"{anchor}-title\""
            )),
            "{anchor} has no matching section"
        );
        assert!(
            html.contains(&format!("id=\"{anchor}-title\"")),
            "{anchor} heading has no id, so aria-labelledby points nowhere"
        );
    }

    let save = async |fields: Vec<(&'static str, String)>| {
        let mut all: Vec<(&str, String)> = vec![
            ("site_name", "Sample Community".into()),
            ("read_only_message", "Read-only for now".into()),
        ];
        // A field with the same name must be replaced, not appended: the form is urlencoded and only the first of
        // repeated keys is parsed, so an appended copy is as good as not written.
        for (key, value) in fields {
            match all.iter_mut().find(|(name, _)| *name == key) {
                Some(slot) => slot.1 = value,
                None => all.push((key, value)),
            }
        }
        post(&app, "/manage/settings", &administrator, &all).await
    };

    // An invalid URL is rejected and nothing is written. A validation failure is 422, not the 409 of a version
    // conflict: the causes differ; for the former the administrator just corrects the input, for the latter the current
    // values must be fetched again.
    let (status, html) = save(vec![("site_url", "not-a-url".into())]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("The forum URL must be an HTTP(S) URL with a host name"));
    // Filled-in values must be refilled, so the administrator does not have to retype the whole form.
    assert!(html.contains("value=\"not-a-url\""));
    // The fixture has already written the forum URL (instead of the manual write before deployment); an invalid URL does
    // not change it.
    assert_eq!(
        runtime::read(&f.db).await.unwrap().site.url,
        gono_forum_web::test_support::TEST_SITE_URL
    );

    // A valid URL takes effect: canonical and the sitemap follow it, while cookies and same-origin checks are unaffected.
    let (status, _) = save(vec![
        ("site_url", "https://links.example.test".into()),
        ("contact_email", "hello@example.test".into()),
    ])
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let stored = runtime::read(&f.db).await.unwrap();
    assert_eq!(stored.site.url, "https://links.example.test");
    assert_eq!(stored.site.contact_email, "hello@example.test");

    // Each field reports its own error, instead of one combined sentence that leaves the administrator guessing.
    for (field, value, expected) in [
        (
            "site_name",
            "x".repeat(81),
            // The name may be left empty (docs/architecture.md, "Languages").
            "The site name can be at most 80 characters (leave it empty to show Gono Forum)",
        ),
        (
            "read_only_message",
            "x".repeat(501),
            "The read-only notice can be at most 500 characters",
        ),
        (
            "contact_email",
            "not-an-email".to_owned(),
            "Leave the contact email empty, or give an email address of at most 190 bytes",
        ),
        (
            "site_url",
            format!("https://{}.test", "a".repeat(260)),
            "The forum URL can be at most 255 characters",
        ),
        (
            "date_format",
            "klingon".to_owned(),
            // The date style `locale` follows the language (docs/architecture.md, "Languages").
            "The date format must be ymd-dash, ymd-cn, mdy-slash or locale",
        ),
        (
            "relative_hours",
            "0".to_owned(),
            "Relative times must cover 1 to 720 hours",
        ),
        (
            "timezone_offset",
            "9999".to_owned(),
            "The site time zone offset must be -720 to 840 minutes",
        ),
    ] {
        let (status, html) = save(vec![(field, value)]).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert!(
            html.contains(expected),
            "{field} should report \"{expected}\""
        );
    }
    // The two moved items report their own errors on their own tabs.
    let (status, html) = post(
        &app,
        "/manage/settings/search/seo",
        &administrator,
        &[("description", "x".repeat(501))],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("The site description can be at most 500 characters"));
    let (status, html) = post(
        &app,
        "/manage/settings/registration",
        &administrator,
        &[("registration", "whatever".into())],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("The registration mode must be closed, invite or open"));

    // After a successful save the feedback must be visible, not a page that looks unchanged after clicking, leaving it
    // unclear whether it saved.
    let (_, saved) = get(
        &app,
        "/manage/settings?saved=1",
        Some(&administrator.session_token),
    )
    .await;
    assert!(
        saved.contains("Settings saved."),
        "no success notice after saving"
    );
    assert!(
        saved.contains("role=\"status\""),
        "the success notice must not use alert"
    );
    let (_, plain) = get(&app, "/manage/settings", Some(&administrator.session_token)).await;
    assert!(
        !plain.contains("Settings saved."),
        "a plain visit must not show the saved notice"
    );

    let (_, html) = get(&app, "/terms", None).await;
    assert!(
        html.contains("rel=\"canonical\" href=\"https://links.example.test/terms\""),
        "canonical must follow the site URL"
    );
    // A policy page never saved uses the built-in text and shows no update date; after saving it does. The document
    // version number was removed together with optimistic locking, so this line uses updated_at, which was always
    // stored but never read.
    assert!(
        !html.contains("Last updated"),
        "a terms page never saved must not show an update date"
    );
    let (status, _) = post(
        &app,
        "/manage/policies/terms",
        &administrator,
        &[
            ("title", "Terms of Use".into()),
            ("body", "Custom terms body for this site".into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, html) = get(&app, "/terms", None).await;
    assert!(html.contains("Custom terms body for this site"));
    assert!(
        html.contains("Last updated:"),
        "a saved terms page must show its update date: {html}"
    );
    // The sitemap URL in robots.txt comes through another value path (not via Page, reading the in-process cache),
    // and both must follow the same URL. The indexing switch is in the SEO section of "Search and
    // performance".
    let (status, _) = post(
        &app,
        "/manage/settings/search/seo",
        &administrator,
        &[
            ("indexing_enabled", "true".into()),
            ("description", "Test description".into()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, robots) = get(&app, "/robots.txt", None).await;
    assert!(
        robots.contains("https://links.example.test/sitemap.xml"),
        "the address in robots.txt must follow the site URL, got: {robots}"
    );
    // The contact e-mail is stored but not shown: it must not leak into any public page.
    assert!(!html.contains("hello@example.test"));

    // Time zone and date format reach the display layer. The registration date is 1, which is 1970-01-01 in UTC and
    // still the 1st in UTC+8, but falls back to 1969-12-31 in UTC-6: crossing midnight proves the offset really takes
    // effect.
    let (_, profile) = get(
        &app,
        "/manage/users/9902",
        Some(&administrator.session_token),
    )
    .await;
    assert!(
        profile.contains("1970-01-01"),
        "the default offset is UTC+8"
    );
    let (status, _) = save(vec![
        ("site_url", "https://links.example.test".into()),
        ("timezone_offset", "-360".into()),
        ("date_format", "ymd-cn".into()),
    ])
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, profile) = get(
        &app,
        "/manage/users/9902",
        Some(&administrator.session_token),
    )
    .await;
    assert!(
        // "\u{5e74}", "\u{6708}" and "\u{65e5}" are the Chinese signs for year, month and day that `ymd-cn` writes.
        profile.contains("1969\u{5e74}12\u{6708}31\u{65e5}"),
        "both the time zone and the date format must take effect, got: {}",
        &profile[..profile.len().min(400)]
    );

    f.cleanup().await.unwrap();
}
