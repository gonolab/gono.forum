use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use gono_forum_web::{
    config::Config,
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::Database,
    identity::{self, Actor, Identity, IdentityError, RoleAssignment},
    manage, project_root,
};

fn actor(uid: i64, credentials_version: i64) -> Actor {
    Actor {
        uid,
        credentials_version,
        api_key: None,
    }
}
fn request(
    target_uid: i64,
    adminid: i64,
    forum_ids: Vec<i64>,
    expected_version: i64,
) -> RoleAssignment {
    RoleAssignment {
        target_uid,
        adminid,
        forum_ids,
        expected_version,
        request_key: identity::random_token(),
        reason: "Adjust forum moderation duties".into(),
    }
}
async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::query(sql)
        .execute(db.executor())
        .await
        .unwrap();
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn contract(db: &Database) {
    db.migrate().await.unwrap();
    let config = Config::load_test(&project_root()).unwrap();
    content::seed_forums(db).await.unwrap();
    let forums: Vec<i64> =
        gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid")
            .fetch_all(db.executor())
            .await
            .unwrap();
    let (a, b) = (forums[0], forums[1]);
    let password = Argon2::default()
        .hash_password(
            b"Role-test-password",
            &SaltString::encode_b64(b"role-test-salt").unwrap(),
        )
        .unwrap()
        .to_string();
    for (uid, role) in [(1, 1), (2, 0), (3, 2), (4, 3), (5, 1), (6, 0)] {
        gono_forum_web::db::query("INSERT INTO gonof_common_member (uid,username,email,password,emailstatus,adminid,regdate) VALUES (?,?,?,?,1,?,1)")
            .bind(uid).bind(format!("role-{uid}")).bind(format!("role-{uid}@example.test")).bind(&password).bind(role).execute(db.executor()).await.unwrap();
    }
    execute(
        db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(4,{a})"),
    )
    .await;
    let identity = Identity::new(
        db.clone(),
        config.identity,
        project_root(),
        gono_forum_web::test_support::detached_global(),
    );
    assert!(matches!(
        manage::staff(db, actor(2, 1)).await,
        Err(IdentityError::Forbidden)
    ));
    assert!(identity::can_manage_forum(db, 4, a).await.unwrap());
    assert!(!identity::can_manage_forum(db, 4, b).await.unwrap());
    assert!(identity::can_manage_forum(db, 3, b).await.unwrap());
    assert!(matches!(
        identity
            .assign_role(actor(3, 1), request(2, 2, vec![], 1))
            .await,
        Err(IdentityError::Forbidden)
    ));
    assert!(matches!(
        identity
            .assign_role(actor(1, 1), request(1, 0, vec![], 1))
            .await,
        Err(IdentityError::Validation(_))
    ));

    let session = identity
        .login("role-2", "Role-test-password")
        .await
        .unwrap()
        .session_token;
    let now = identity::now();
    assert!(identity.session(&session).await.unwrap().is_some());
    let mut appointment = request(2, 3, vec![b, b], 1);
    assert_eq!(
        identity
            .assign_role(actor(1, 1), appointment.clone())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        scalar(
            db,
            "SELECT credentials_version FROM gonof_common_member WHERE uid=2"
        )
        .await,
        2
    );
    assert!(identity.session(&session).await.unwrap().is_none());
    assert!(!identity::valid_actor(db, actor(2, 1)).await.unwrap());
    assert!(identity::valid_actor(db, actor(2, 2)).await.unwrap());
    assert!(identity::can_manage_forum(db, 2, b).await.unwrap());
    assert!(!identity::can_manage_forum(db, 2, a).await.unwrap());
    appointment.forum_ids = vec![b];
    assert_eq!(
        identity
            .assign_role(actor(1, 1), appointment.clone())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='identity.role.assign'"
        )
        .await,
        1
    );
    appointment.reason = "Different content with the same key".into();
    assert!(matches!(
        identity.assign_role(actor(1, 1), appointment).await,
        Err(IdentityError::Conflict(_))
    ));
    assert!(matches!(
        identity
            .assign_role(actor(1, 1), request(2, 2, vec![], 1))
            .await,
        Err(IdentityError::Conflict(_))
    ));

    let no_op = request(2, 3, vec![b], 2);
    identity
        .assign_role(actor(1, 1), no_op.clone())
        .await
        .unwrap();
    identity.assign_role(actor(1, 1), no_op).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT credentials_version FROM gonof_common_member WHERE uid=2"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='identity.role.assign'"
        )
        .await,
        2
    );
    let invalid = request(2, 3, vec![9999999], 2);
    assert!(matches!(
        identity.assign_role(actor(1, 1), invalid).await,
        Err(IdentityError::Validation(_))
    ));
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_common_member_role_request").await,
        2
    );

    let left = request(6, 3, vec![a], 1);
    let right = request(6, 3, vec![b], 1);
    let results = tokio::join!(
        identity.assign_role(actor(1, 1), left),
        identity.assign_role(actor(5, 1), right)
    );
    assert_eq!(
        usize::from(results.0.is_ok()) + usize::from(results.1.is_ok()),
        1
    );
    assert!(
        matches!(results.0, Err(IdentityError::Conflict(_)))
            || matches!(results.1, Err(IdentityError::Conflict(_)))
    );
    assert_eq!(
        scalar(
            db,
            "SELECT credentials_version FROM gonof_common_member WHERE uid=6"
        )
        .await,
        2
    );
    // The same admin's two identical requests serialize and replay the committed result.
    let duplicate = request(2, 2, vec![], 2);
    let (one, two) = tokio::join!(
        identity.assign_role(actor(1, 1), duplicate.clone()),
        identity.assign_role(actor(1, 1), duplicate)
    );
    assert_eq!(one.unwrap(), two.unwrap());
    assert_eq!(
        scalar(
            db,
            "SELECT credentials_version FROM gonof_common_member WHERE uid=2"
        )
        .await,
        3
    );
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_forum_moderator WHERE uid=2").await,
        0
    );

    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let mut posts = Vec::new();
    for (fid, title) in [
        (a, "Forum A confidential source"),
        (b, "Forum B confidential source"),
    ] {
        posts.push(
            writer
                .publish(
                    1,
                    NewThread {
                        visibility: Default::default(),
                        activity: Default::default(),
                        fid,
                        title: title.into(),
                        body: "<script>hidden-source</script>".into(),
                        attachment_ids: vec![],
                        request_key: identity::random_token(),
                    },
                )
                .await
                .unwrap(),
        );
    }
    let page = manage::content(db, actor(4, 1), &manage::ContentQuery::default())
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.rows[0].pid, posts[0].pid);
    assert!(matches!(
        manage::post(db, actor(4, 1), posts[1].pid, 1).await,
        Err(IdentityError::NotFound)
    ));
    for post in &posts {
        let fid = if post.pid == posts[0].pid { a } else { b };
        // Content log entries write the forum and post into the fid and pid columns in the same statement;
        // moderators filter by those columns.
        gono_forum_web::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline,pid,fid) VALUES(1,1,'content.hide',?,'web',?,?,?)")
            .bind(serde_json::json!({"fid":fid,"pid":post.pid,"reason":"review reason","result":"applied"}).to_string()).bind(now)
            .bind(post.pid).bind(fid).execute(db.executor()).await.unwrap();
    }
    let (logs, total) = manage::logs(db, actor(4, 1), 1).await.unwrap();
    assert!(total >= 1);
    assert!(
        logs.iter()
            .all(|log| log.target != format!("PID {}", posts[1].pid))
    );
    assert!(
        logs.iter()
            .all(|log| log.operation != "identity.role.assign")
    );
    assert!(matches!(
        manage::members(db, actor(3, 1), "", 1).await,
        Err(IdentityError::Forbidden)
    ));
    assert!(matches!(
        manage::content(
            db,
            actor(4, 1),
            &manage::ContentQuery {
                page: Some(i64::MAX),
                ..Default::default()
            }
        )
        .await,
        Err(IdentityError::Validation(_))
    ));
    withdrawal_visibility(db, &writer, posts[0].pid, posts[0].tid).await;
    // Account revocation is checked before replaying an earlier successful request.
    execute(
        db,
        "UPDATE gonof_common_member SET adminid=2,credentials_version=2 WHERE uid=1",
    )
    .await;
    assert!(matches!(
        identity
            .assign_role(actor(1, 1), request(2, 0, vec![], 3))
            .await,
        Err(IdentityError::Unauthorized)
    ));
    assert!(matches!(
        identity
            .assign_role(actor(1, 2), request(2, 0, vec![], 3))
            .await,
        Err(IdentityError::Forbidden)
    ));
}

async fn withdrawal_visibility(db: &Database, writer: &ContentWriter, first_pid: i64, tid: i64) {
    let root = writer
        .reply(
            1,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Parent floor for the admin visibility test".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let child = writer
        .reply(
            1,
            NewReply {
                tid,
                root_pid: Some(root.pid),
                reply_to_pid: Some(root.pid),
                body: "Standalone child floor for admin visibility".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 7 * 86400);
    for (pid, action, hidden) in [
        (root.pid, ContentAction::Withdraw, false),
        (first_pid, ContentAction::Withdraw, false),
        (root.pid, ContentAction::Hide, true),
        (root.pid, ContentAction::Unhide, false),
        (first_pid, ContentAction::Hide, true),
    ] {
        let (target, _) = manage::post(db, actor(1, 1), pid, 1).await.unwrap();
        editor
            .change(
                actor(1, 1),
                ChangeRequest {
                    pid,
                    expected_post_version: target.post_version,
                    expected_thread_version: target.thread_version,
                    request_key: identity::random_token(),
                    reason: if action == ContentAction::Withdraw {
                        String::new()
                    } else {
                        "Checks the admin state filter".into()
                    },
                    action,
                },
            )
            .await
            .unwrap();
        for (state, expected) in [
            ("visible", i64::from(!hidden)),
            ("hidden", i64::from(hidden)),
            ("deleted", 0),
        ] {
            let page = manage::content(
                db,
                actor(4, 1),
                &manage::ContentQuery {
                    kind: "reply".into(),
                    state: state.into(),
                    q: "Standalone child floor for admin visibility".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                page.total, expected,
                "after {action:?} the {state} filter disagrees for the child floor"
            );
            if let Some(row) = page.rows.first() {
                assert_eq!(row.pid, child.pid);
                assert_eq!(
                    row.state,
                    if hidden {
                        "Hidden / not public"
                    } else {
                        "Public"
                    }
                );
                assert!(
                    !row.deleted,
                    "withdrawing an ancestor must not mark the child withdrawn"
                );
                assert!(
                    !row.hidden,
                    "hiding an ancestor must not rewrite the child's own moderation state"
                );
            }
        }
        let (detail, _) = manage::post(db, actor(4, 1), child.pid, 1).await.unwrap();
        assert_eq!(
            detail.state,
            if hidden {
                "Hidden / not public"
            } else {
                "Public"
            }
        );
        assert!(!detail.hidden);
        assert!(!detail.deleted);
    }
}

#[tokio::test]
async fn mysql_roles_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    contract(&services.db).await;
    services.cleanup().await.unwrap();
}

/// A thread-move log entry is visible to the moderators of both the source and the target forum, and not to the
/// moderators of unrelated forums.
#[tokio::test]
async fn thread_move_logs_reach_moderators_of_both_forums() {
    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = &services.db;
    content::seed_forums(db).await.unwrap();
    content::seed_fixture(db).await.unwrap();
    let source = scalar(db, "SELECT fid FROM gonof_forum_thread WHERE tid=103").await;
    let forums: Vec<i64> = gono_forum_web::db::query_scalar(
        "SELECT fid FROM gonof_forum_forum WHERE fid<>? AND parent_fid IS NULL ORDER BY fid LIMIT 2",
    )
    .bind(source)
    .fetch_all(db.executor())
    .await
    .unwrap();
    let (target, unrelated) = (forums[0], forums[1]);
    for (uid, role) in [(31, 1), (32, 3), (33, 3), (34, 3)] {
        gono_forum_web::db::query("INSERT INTO gonof_common_member (uid,username,email,password,emailstatus,adminid,regdate) VALUES (?,?,?,'synthetic-hash',1,?,1)")
            .bind(uid).bind(format!("move-log-{uid}")).bind(format!("move-log-{uid}@example.test")).bind(role).execute(db.executor()).await.unwrap();
    }
    for (uid, fid) in [(32, source), (33, target), (34, unrelated)] {
        execute(
            db,
            &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES({uid},{fid})"),
        )
        .await;
    }
    let version = scalar(db, "SELECT version FROM gonof_forum_thread WHERE tid=103").await;
    gono_forum_web::organization::move_thread(
        db,
        actor(31, 1),
        103,
        target,
        version,
        &identity::random_token(),
        "Move to a better forum",
    )
    .await
    .unwrap();
    for (uid, visible) in [(31, true), (32, true), (33, true), (34, false)] {
        let (logs, _) = manage::logs_filtered(db, actor(uid, 1), &Default::default())
            .await
            .unwrap();
        assert_eq!(
            logs.iter()
                .any(|log| log.operation == "content.thread.move"),
            visible,
            "UID {uid}"
        );
    }
    services.cleanup().await.unwrap();
}
