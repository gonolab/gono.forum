use futures_util::future::join_all;
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::Database,
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
};
use std::{sync::Arc, time::Duration};
const NOW: i64 = 1_800_050_000;
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::query(sql)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}
fn editor(db: &Database) -> ContentEditor {
    ContentEditor::new(db.clone(), WriteConfig::default(), 86_400)
}
// The new-database baseline can be run repeatedly and keeps the application's moderation-state constraints.

async fn setup(db: &Database) -> i64 {
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(
        scalar(db, "SELECT MAX(version) FROM _sqlx_migrations").await,
        gono_forum_web::db::migrations::LATEST
    );
    content::seed_forums(db).await.unwrap();
    for (uid, role) in [(101, 0), (102, 0), (103, 3), (104, 2), (105, 1)] {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(?,?,?,'test',1,?,?)")
            .bind(uid).bind(format!("governance-{uid}")).bind(format!("g{uid}@example.test")).bind(role).bind(NOW-200_000).execute(&db.pool).await.unwrap();
    }
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    execute(
        db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(103,{fid})"),
    )
    .await;
    fid
}
async fn topic(db: &Database, fid: i64) -> Published {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .publish_at(
            105,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Original thread".into(),
                body: "Original first post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}
async fn reply(db: &Database, tid: i64, uid: i64, root: Option<i64>) -> Published {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .reply_at(
            uid,
            NewReply {
                tid,
                root_pid: root,
                reply_to_pid: root,
                body: format!("Reply {uid}"),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 1,
        )
        .await
        .unwrap()
}
async fn edit_request(e: &ContentEditor, pid: i64, uid: i64) -> EditRequest {
    let p = e.editable_at(actor(uid), pid, NOW + 20).await.unwrap();
    EditRequest {
        visibility: None,
        pid,
        expected_post_version: p.version,
        expected_thread_version: p.thread_version,
        title: p.first.then_some(p.title),
        body: Some("Edited content".into()),
        request_key: identity::random_token(),
        reason: if uid == 101 {
            String::new()
        } else {
            "correct the content".into()
        },
    }
}
async fn change_request(
    e: &ContentEditor,
    pid: i64,
    uid: i64,
    action: ContentAction,
) -> ChangeRequest {
    let p = e.editable_at(actor(uid), pid, NOW + 20).await.unwrap();
    ChangeRequest {
        pid,
        expected_post_version: p.version,
        expected_thread_version: p.thread_version,
        request_key: identity::random_token(),
        reason: if uid == 101 {
            String::new()
        } else {
            "rule-breaking content".into()
        },
        action,
    }
}
async fn apply(e: &ContentEditor, pid: i64, uid: i64, action: ContentAction) {
    let r = change_request(e, pid, uid, action).await;
    e.change_at(actor(uid), r, NOW + 3).await.unwrap();
}
async fn contract(db: &Database) {
    let fid = setup(db).await;
    let e = editor(db);
    let t = topic(db, fid).await;
    let reserved_thread = topic(db, fid).await;
    let reserved_reply = reply(db, reserved_thread.tid, 101, None).await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=1 WHERE pid={}",
            reserved_reply.pid
        ),
    )
    .await;
    for action in [ContentAction::Hide, ContentAction::Unhide] {
        let reserved = change_request(&e, reserved_reply.pid, 104, action).await;
        assert!(matches!(
            e.change_at(actor(104), reserved, NOW + 1).await,
            Err(ContentWriteError::Validation(_))
        ));
        assert_eq!(
            scalar(
                db,
                &format!(
                    "SELECT invisible FROM gonof_forum_post WHERE pid={}",
                    reserved_reply.pid
                )
            )
            .await,
            1,
            "the reserved state must not be overwritten"
        );
    }

    assert_eq!(
        scalar(
            db,
            &format!("SELECT version FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        1
    );
    for (column, value, action) in [
        ("displayorder", -2, ContentAction::Pin),
        ("closed", 2, ContentAction::Unlock),
        ("digest", 2, ContentAction::Unfeature),
    ] {
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_thread SET {column}={value} WHERE tid={}",
                reserved_thread.tid
            ),
        )
        .await;
        let reserved = change_request(&e, reserved_thread.pid, 104, action).await;
        assert!(matches!(
            e.change_at(actor(104), reserved, NOW + 1).await,
            Err(ContentWriteError::Validation(_))
        ));
        assert_eq!(
            scalar(
                db,
                &format!(
                    "SELECT {column} FROM gonof_forum_thread WHERE tid={}",
                    reserved_thread.tid
                )
            )
            .await,
            value,
            "the reserved thread state must not be rewritten"
        );
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_thread SET {column}=0 WHERE tid={}",
                reserved_thread.tid
            ),
        )
        .await;
    }
    let root = reply(db, t.tid, 101, None).await;
    let child = reply(db, t.tid, 102, Some(root.pid)).await;
    let edit = edit_request(&e, root.pid, 101).await;
    let mut uppercase = edit.clone();
    uppercase.request_key = uppercase.request_key.to_uppercase();
    assert!(matches!(
        e.edit_at(actor(101), uppercase, NOW + 2).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert!(matches!(
        e.edit_at(actor(102), edit.clone(), NOW + 2).await,
        Err(ContentWriteError::NotFound)
    ));
    let mut stale_actor = actor(101);
    stale_actor.credentials_version = 2;
    assert!(matches!(
        e.edit_at(stale_actor, edit.clone(), NOW + 2).await,
        Err(ContentWriteError::Unauthorized)
    ));
    // Concurrent requests with the same key may write only one revision, log and request; afterwards a different
    // key with the same version gets 409.
    let results = join_all((0..8).map(|_| e.edit_at(actor(101), edit.clone(), NOW + 2))).await;
    let edited: Vec<_> = results.into_iter().map(Result::unwrap).collect();
    assert!(
        edited
            .iter()
            .all(|r| (r.pid, r.tid, r.position) == (root.pid, root.tid, root.position))
    );
    assert_eq!(edited.iter().filter(|r| !r.replayed).count(), 1);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_post_revision WHERE pid={}",
                root.pid
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.edit'"
        )
        .await,
        1
    );
    let mut stale = edit.clone();
    stale.request_key = identity::random_token();
    assert!(matches!(
        e.edit_at(actor(101), stale, NOW + 2).await,
        Err(ContentWriteError::Conflict(_))
    ));
    let mut reused = edit.clone();
    reused.body = Some("Different body".into());
    assert!(matches!(
        e.edit_at(actor(101), reused, NOW + 2).await,
        Err(ContentWriteError::Conflict(_))
    ));
    let current = edit_request(&e, root.pid, 101).await;
    assert!(matches!(
        e.edit_at(actor(101), current.clone(), NOW + 1 + 86_400)
            .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    assert!(e.edit_at(actor(101), current, NOW + 86_400).await.is_ok());
    // Current authorization comes before replay: a freeze cannot be bypassed with a successful request, and after
    // restoring, the request can be replayed.
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=101",
    )
    .await;
    assert!(matches!(
        e.edit_at(actor(101), edit.clone(), NOW + 2).await,
        Err(ContentWriteError::Unauthorized)
    ));
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=101",
    )
    .await;
    assert!(e.edit_at(actor(101), edit, NOW + 2).await.is_ok());

    let withdraw = change_request(&e, root.pid, 101, ContentAction::Withdraw).await;
    e.change_at(actor(101), withdraw.clone(), NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT sub_replies_count FROM gonof_forum_post WHERE pid={}",
                root.pid
            )
        )
        .await,
        1
    );
    let after_withdraw = reply(db, t.tid, 102, Some(root.pid)).await;
    assert_eq!(after_withdraw.position, 4);
    let mut edit_withdrawn = edit_request(&e, root.pid, 101).await;
    edit_withdrawn.body = Some("Still correctable after withdrawal".into());
    e.edit_at(actor(101), edit_withdrawn, NOW + 4)
        .await
        .unwrap();
    assert!(
        scalar(
            db,
            &format!(
                "SELECT deleted_at FROM gonof_forum_post WHERE pid={}",
                root.pid
            )
        )
        .await
            > 0
    );
    apply(&e, root.pid, 101, ContentAction::Restore).await;
    e.change_at(actor(101), withdraw, NOW + 5).await.unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT deleted_at FROM gonof_forum_post WHERE pid={}",
                root.pid
            )
        )
        .await,
        0,
        "an old withdrawal request must not withdraw restored content"
    );
    let inherited = edit_request(&e, child.pid, 104).await;
    let mut child_restore = change_request(&e, child.pid, 102, ContentAction::Restore).await;
    child_restore.reason.clear();
    apply(&e, root.pid, 103, ContentAction::Hide).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT invisible FROM gonof_forum_post WHERE pid={}",
                root.pid
            )
        )
        .await,
        -1
    );
    assert!(
        matches!(
            e.change_at(actor(102), child_restore, NOW + 5).await,
            Err(ContentWriteError::NotFound)
        ),
        "authors cannot restore content under an ancestor hidden by moderators"
    );
    assert!(matches!(
        e.editable_at(actor(101), root.pid, NOW + 5).await,
        Err(ContentWriteError::NotFound)
    ));
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        3
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT invisible FROM gonof_forum_post WHERE pid={}",
                child.pid
            )
        )
        .await,
        0
    );
    assert!(
        e.edit_at(actor(104), inherited, NOW + 5).await.is_ok(),
        "global moderators can edit content under a hidden ancestor"
    );
    apply(&e, child.pid, 103, ContentAction::Hide).await;
    apply(&e, root.pid, 103, ContentAction::Unhide).await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        3
    );
    apply(&e, child.pid, 103, ContentAction::Unhide).await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        3
    );
    // A lock only restricts new additions; withdrawing a thread does not affect the discussion under it; a pin is
    // not lost when it is hidden and then restored.
    let before_lock = edit_request(&e, root.pid, 101).await;
    apply(&e, t.pid, 103, ContentAction::Lock).await;
    assert!(matches!(
        e.edit_at(actor(101), before_lock, NOW + 5).await,
        Err(ContentWriteError::Conflict(_))
    ));
    let mut locked_edit = edit_request(&e, root.pid, 101).await;
    locked_edit.body = Some("Corrected while locked".into());
    e.edit_at(actor(101), locked_edit, NOW + 5).await.unwrap();
    let w = ContentWriter::new(db.clone(), WriteConfig::default());
    assert!(matches!(
        w.publish_actor(
            Actor {
                uid: 105,
                credentials_version: 2,
                api_key: None,
            },
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Expired session".into(),
                body: "must not be written".into(),
                attachment_ids: vec![],
                request_key: identity::random_token()
            }
        )
        .await,
        Err(ContentWriteError::Unauthorized)
    ));
    assert!(matches!(
        w.reply_actor(
            Actor {
                uid: 105,
                credentials_version: 2,
                api_key: None,
            },
            NewReply {
                tid: t.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "must not be written".into(),
                attachment_ids: vec![],
                request_key: identity::random_token()
            }
        )
        .await,
        Err(ContentWriteError::Unauthorized)
    ));
    assert!(matches!(
        w.reply_at(
            102,
            NewReply {
                tid: t.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Added while locked".into(),
                attachment_ids: vec![],
                request_key: identity::random_token()
            },
            NOW + 6
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    apply(&e, t.pid, 103, ContentAction::Unlock).await;
    apply(&e, t.pid, 103, ContentAction::Pin).await;
    apply(&e, t.pid, 103, ContentAction::Feature).await;
    apply(&e, t.pid, 105, ContentAction::Withdraw).await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        3
    );
    let _ = reply(db, t.tid, 102, None).await;
    apply(&e, t.pid, 103, ContentAction::Hide).await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        4
    );
    apply(&e, t.pid, 103, ContentAction::Unhide).await;
    // Hiding sets displayorder to -1 and restoring returns it to 0, so the
    // original pin level is not kept; highlight stays as before.
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT displayorder FROM gonof_forum_thread WHERE tid={}",
                t.tid
            )
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT digest FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT replies FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        4
    );
    apply(&e, t.pid, 103, ContentAction::Restore).await;
    let mut title_edit = edit_request(&e, t.pid, 104).await;
    title_edit.title = Some("Corrected thread title".into());
    e.edit_at(actor(104), title_edit, NOW + 5).await.unwrap();
    let original: String = gono_forum_web::db::query_scalar(
        "SELECT subject FROM gonof_forum_post_revision WHERE pid=?",
    )
    .bind(t.pid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(original, "Original thread");

    // A moderator cannot act across forums; revoking the grant makes the moderator's old successful requests
    // impossible to replay.
    let other_fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='ai'").await;
    let other = topic(db, other_fid).await;
    let p = e.editable_at(actor(104), other.pid, NOW + 5).await.unwrap();
    let invalid = ChangeRequest {
        pid: other.pid,
        expected_post_version: p.version,
        expected_thread_version: p.thread_version,
        request_key: identity::random_token(),
        reason: "outside own forum".into(),
        action: ContentAction::Hide,
    };
    assert!(matches!(
        e.change_at(actor(103), invalid, NOW + 6).await,
        Err(ContentWriteError::NotFound)
    ));
    let mut missing_reason = change_request(&e, t.pid, 104, ContentAction::Pin).await;
    missing_reason.reason.clear();
    assert!(matches!(
        e.change_at(actor(104), missing_reason, NOW + 6).await,
        Err(ContentWriteError::Validation(_))
    ));
    let forbidden_withdraw = change_request(&e, root.pid, 104, ContentAction::Withdraw).await;
    assert!(matches!(
        e.change_at(actor(104), forbidden_withdraw, NOW + 6).await,
        Err(ContentWriteError::NotFound)
    ));
    let expired_admin_withdraw = change_request(&e, t.pid, 105, ContentAction::Withdraw).await;
    assert!(matches!(
        e.change_at(actor(105), expired_admin_withdraw, NOW + 86_400)
            .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    // Unhiding reset the pin level to 0; pin again first, so that the pin below is a duplicate
    // request with no change.
    let repin = change_request(&e, t.pid, 104, ContentAction::Pin).await;
    e.change_at(actor(104), repin, NOW + 6).await.unwrap();
    let no_op = change_request(&e, t.pid, 104, ContentAction::Pin).await;
    let before_logs = scalar(
        db,
        "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.pin'",
    )
    .await;
    let unchanged_version = no_op.expected_thread_version;
    let results = join_all((0..6).map(|_| e.change_at(actor(104), no_op.clone(), NOW + 6))).await;
    assert!(results.into_iter().all(|r| r.is_ok()));
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.pin'"
        )
        .await,
        before_logs + 1
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT version FROM gonof_forum_thread WHERE tid={}", t.tid)
        )
        .await,
        unchanged_version
    );
    let audit_row = gono_forum_web::db::query(
        "SELECT data FROM gonof_common_log WHERE type='content.pin' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let audit: serde_json::Value =
        serde_json::from_str(&gono_forum_web::db::text(&audit_row, "data").unwrap()).unwrap();
    assert_eq!(audit["result"], "unchanged");
    assert_eq!(audit["before"], audit["after"]);
    assert_eq!(audit["before"]["pinned"], true);
    let permission_replay = change_request(&e, t.pid, 103, ContentAction::Unpin).await;
    e.change_at(actor(103), permission_replay.clone(), NOW + 6)
        .await
        .unwrap();
    execute(
        db,
        &format!("DELETE FROM gonof_forum_moderator WHERE uid=103 AND fid={fid}"),
    )
    .await;
    assert!(matches!(
        e.change_at(actor(103), permission_replay, NOW + 6).await,
        Err(ContentWriteError::NotFound)
    ));
    let competing = edit_request(&e, root.pid, 101).await;
    let mut second = competing.clone();
    second.body = Some("Another concurrent version".into());
    second.request_key = identity::random_token();
    let competing_results = join_all([
        e.edit_at(actor(101), competing, NOW + 6),
        e.edit_at(actor(101), second, NOW + 6),
    ])
    .await;
    assert_eq!(competing_results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        competing_results
            .iter()
            .filter(|r| matches!(r, Err(ContentWriteError::Conflict(_))))
            .count(),
        1
    );
    attachment_contract(db, &e, t.tid, root.pid).await;
    concurrent_attachments(db, &e, fid, other_fid).await;
    // Thread and post versions are positive and edit times are not negative. Code guarantees it, not the
    // database: versions only increase by one via `next()`, and the edit and
    // moderation entry points reject non-positive expected versions and negative timestamps (existing data is
    // checked by tests/consistency/content.sql).
    let base = change_request(&e, root.pid, 104, ContentAction::Hide).await;
    for (request, timestamp) in [
        (
            ChangeRequest {
                expected_post_version: 0,
                ..base.clone()
            },
            NOW + 7,
        ),
        (
            ChangeRequest {
                expected_thread_version: 0,
                ..base.clone()
            },
            NOW + 7,
        ),
        (base.clone(), -1),
    ] {
        assert!(matches!(
            e.change_at(actor(104), request, timestamp).await,
            Err(ContentWriteError::Validation(_))
        ));
    }
    for statement in [format!(
        "UPDATE gonof_forum_post_revision SET editor_uid=999999 WHERE pid={}",
        root.pid
    )] {
        assert!(
            gono_forum_web::db::query(&statement)
                .execute(&db.pool)
                .await
                .is_err(),
            "the constraint violation must be rejected: {statement}"
        );
    }
}
async fn attachment(db: &Database, aid: i64, uid: i64) {
    gono_forum_web::db::query(
        "INSERT INTO gonof_forum_attachment(aid,uid,request_key,request_hash) VALUES(?,?,?,?)",
    )
    .bind(aid)
    .bind(uid)
    .bind(identity::random_token())
    .bind(identity::random_token())
    .execute(&db.pool)
    .await
    .unwrap();
    gono_forum_web::db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,'test.png',10,'image/png',?,'test','test','','',?,?,'ready','clean','')")
        .bind(aid).bind("a".repeat(64)).bind(NOW).bind(NOW).execute(&db.pool).await.unwrap();
}
async fn attachment_contract(db: &Database, e: &ContentEditor, tid: i64, pid: i64) {
    attachment(db, 201, 101).await;
    attachment(db, 202, 104).await;
    attachment(db, 203, 102).await;
    let mut add = edit_request(e, pid, 101).await;
    add.body = Some("![own image](attachment:201)".into());
    e.edit_at(actor(101), add, NOW + 7).await.unwrap();
    let mut staff = edit_request(e, pid, 104).await;
    staff.body = Some("Keep ![image](attachment:201) ![staff image](attachment:202)".into());
    e.edit_at(actor(104), staff, NOW + 8).await.unwrap();
    let mut forbidden = edit_request(e, pid, 104).await;
    forbidden.body = Some("![not allowed](attachment:203)".into());
    let version = forbidden.expected_post_version;
    assert!(matches!(
        e.edit_at(actor(104), forbidden, NOW + 9).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(
        scalar(
            db,
            &format!("SELECT version FROM gonof_forum_post WHERE pid={pid}")
        )
        .await,
        version
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE pid={pid}")
        )
        .await,
        2
    );
    let shared = ContentWriter::new(db.clone(), WriteConfig::default())
        .reply_at(
            101,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: "![shared image](attachment:201)".into(),
                attachment_ids: vec![201],
                request_key: identity::random_token(),
            },
            NOW + 9,
        )
        .await
        .unwrap();
    let mut remove = edit_request(e, pid, 104).await;
    remove.body = Some("Remove all attachments".into());
    e.edit_at(actor(104), remove, NOW + 10).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=201"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE aid=201 AND purge_after IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT purge_after FROM gonof_forum_attachment_0 WHERE aid=202"
        )
        .await,
        NOW + 10 + 86_400
    );
    let withdraw = change_request(e, shared.pid, 101, ContentAction::Withdraw).await;
    e.change_at(actor(101), withdraw, NOW + 11).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=201"
        )
        .await,
        1,
        "withdrawal keeps the attachment references for restoring"
    );
}
async fn concurrent_attachments(db: &Database, e: &ContentEditor, fid: i64, other_fid: i64) {
    let a = topic(db, fid).await;
    let b = topic(db, other_fid).await;
    attachment(db, 301, 104).await;
    for pid in [a.pid, b.pid] {
        let mut request = edit_request(e, pid, 104).await;
        request.body = Some("![shared attachment](attachment:301)".into());
        e.edit_at(actor(104), request, NOW + 10).await.unwrap();
    }
    let mut first = edit_request(e, a.pid, 104).await;
    first.body = Some("Remove shared attachment A".into());
    let mut second = edit_request(e, b.pid, 105).await;
    second.body = Some("Remove shared attachment B".into());
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            e.edit_at(actor(104), first, NOW + 12),
            e.edit_at(actor(105), second, NOW + 12)
        )
    })
    .await
    .expect("editing a shared attachment across forums must not deadlock");
    result.0.unwrap();
    result.1.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=301"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            db,
            "SELECT purge_after FROM gonof_forum_attachment_0 WHERE aid=301"
        )
        .await,
        NOW + 12 + 86_400
    );
}

#[tokio::test]
async fn mysql_content_governance_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    contract(&services.db).await;
    services.cleanup().await.unwrap();
}

// Content hidden by moderation cannot be edited or unhidden by its author: moderators and global moderators cannot
// do so through their moderator role either; administrators are the exception.
async fn own_hiding_contract(db: &Database) {
    let fid = setup(db).await;
    let e = editor(db);
    let t = topic(db, fid).await;
    let moderator = reply(db, t.tid, 103, None).await;
    let global = reply(db, t.tid, 104, None).await;
    apply(&e, moderator.pid, 104, ContentAction::Hide).await;
    apply(&e, global.pid, 103, ContentAction::Hide).await;
    apply(&e, t.pid, 104, ContentAction::Hide).await;

    for (pid, uid) in [(moderator.pid, 103), (global.pid, 104)] {
        let r = change_request(&e, pid, uid, ContentAction::Unhide).await;
        assert!(matches!(
            e.change_at(actor(uid), r, NOW + 4).await,
            Err(ContentWriteError::Forbidden(_))
        ));
        // Opening the edit page uses the same check as saving: do not hand out a form that is bound to fail on
        // submit. The thread organize page can still be opened.
        assert!(matches!(
            e.edit_form_at(actor(uid), pid, NOW + 4).await,
            Err(ContentWriteError::Forbidden(_))
        ));
        e.editable_at(actor(uid), pid, NOW + 4).await.unwrap();
        let r = edit_request(&e, pid, uid).await;
        assert!(matches!(
            e.edit_at(actor(uid), r, NOW + 4).await,
            Err(ContentWriteError::Forbidden(_))
        ));
        let state = format!("SELECT invisible FROM gonof_forum_post WHERE pid={pid}");
        assert_eq!(scalar(db, &state).await, -1);
        assert_eq!(
            scalar(
                db,
                &format!("SELECT COUNT(*) FROM gonof_forum_post_revision WHERE pid={pid}")
            )
            .await,
            0
        );
    }
    // Other moderators and administrators unhide as usual; an administrator unhiding their own thread also works
    // as usual.
    apply(&e, moderator.pid, 104, ContentAction::Unhide).await;
    apply(&e, global.pid, 105, ContentAction::Unhide).await;
    apply(&e, t.pid, 105, ContentAction::Unhide).await;
    for pid in [moderator.pid, global.pid, t.pid] {
        let state = format!("SELECT invisible FROM gonof_forum_post WHERE pid={pid}");
        assert_eq!(scalar(db, &state).await, 0);
    }
    // When not hidden, a moderator editing their own content with a reason is unaffected.
    let r = edit_request(&e, moderator.pid, 103).await;
    e.edit_at(actor(103), r, NOW + 5).await.unwrap();
}

#[tokio::test]
async fn mysql_staff_author_cannot_lift_own_hiding() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    own_hiding_contract(&services.db).await;
    services.cleanup().await.unwrap();
}

/// Wait until `sql` has been sent but has not returned yet: this means the locking statement is stuck behind
/// another transaction.
async fn wait_blocked(profile: &RequestProfile, sql: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !profile.snapshot().iter().any(|r| {
            r.source == QuerySource::Application
                && r.sql == sql
                && r.rows_returned == 0
                && r.elapsed.is_zero()
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no waiting statement seen: {sql}"));
}

// Hiding first does a plain read (to locate the post), then queues for the forum lock; a new reply committed while
// it queues must be counted in the valid reply count and the last reply. Under REPEATABLE READ this recount would
// reuse the snapshot from before queuing, miss that reply, and also overwrite the reply's own counts with old values.
async fn hide_waiting_behind_reply_contract(db: &Database) {
    const POST_DISCOVERY: &str = "SELECT tid,fid,root_pid FROM gonof_forum_post WHERE pid=?";
    const FORUM_LOCK: &str = "SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE";
    const FIRST_POST_LOCK: &str =
        "SELECT pid FROM gonof_forum_post WHERE first_tid = ? AND invisible = 0 FOR UPDATE";
    let fid = setup(db).await;
    let e = editor(db);
    let t = topic(db, fid).await;
    let target = reply(db, t.tid, 101, None).await;
    let earlier = reply(db, t.tid, 104, None).await;
    let request = change_request(&e, target.pid, 103, ContentAction::Hide).await;

    // Lock the first post: once the new reply has the forum and thread locks it will stop at the first-post lock.
    let mut blocker = db.begin_write().await.unwrap();
    gono_forum_web::db::query("SELECT pid FROM gonof_forum_post WHERE pid=? FOR UPDATE")
        .bind(t.pid)
        .fetch_one(db.observe(&mut *blocker))
        .await
        .unwrap();
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let reply_profile = Arc::new(RequestProfile::new());
    let late = trace::scope(
        reply_profile.clone(),
        Box::pin(writer.reply_at(
            102,
            NewReply {
                tid: t.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Reply posted while queued".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 6,
        )),
    );
    let hide_profile = Arc::new(RequestProfile::new());
    let hide = async {
        wait_blocked(&reply_profile, FIRST_POST_LOCK).await;
        trace::scope(
            hide_profile.clone(),
            Box::pin(e.change_at(actor(103), request, NOW + 7)),
        )
        .await
    };
    let release = async {
        wait_blocked(&hide_profile, FORUM_LOCK).await;
        assert!(
            hide_profile
                .snapshot()
                .iter()
                .any(|r| r.sql == POST_DISCOVERY && r.rows_returned == 1),
            "hiding must do a plain read first, which takes the snapshot from before queuing"
        );
        blocker.rollback().await.unwrap();
    };
    let (late, hidden, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(late, hide, release)
    })
    .await
    .expect("the reply and the hiding must both finish once the first post is released");
    let late = late.unwrap();
    hidden.unwrap();

    let valid = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE tid={} AND first=0 AND invisible=0 AND deleted_at=0",
            t.tid
        ),
    )
    .await;
    assert_eq!(
        valid, 2,
        "besides the hidden target, the earlier reply and the queued one remain"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT valid_replies FROM gonof_forum_thread WHERE tid={}",
                t.tid
            )
        )
        .await,
        valid
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT last_reply_pid FROM gonof_forum_thread WHERE tid={}",
                t.tid
            )
        )
        .await,
        late.pid,
        "the last reply must not go back to {}",
        earlier.pid
    );
}

#[tokio::test]
async fn mysql_hide_counts_a_reply_committed_while_it_waited() {
    // Only verifies the MySQL row-lock wait orchestration.
    gono_forum_web::mysql_only!();
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    hide_waiting_behind_reply_contract(&services.db).await;
    services.cleanup().await.unwrap();
}
