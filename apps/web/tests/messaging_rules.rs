//! Private messages, follows, blocks, favorites, likes, requests, revisions and attachments have no triggers; their
//! rules are guaranteed by code (docs/architecture.md, "Data"):
//! - Sending a private message checks the participants under the lock; a conversation's last message only
//!   advances through a conditional update, and a state row's read position and last time only grow;
//! - Post requests and interaction requests are only written through the entry points of `identity::rules`, and keys
//!   and digests are both 64 lowercase hex characters;
//! - The messaging consistency check `tests/consistency/messaging.sql` detects violations written to the database
//!   bypassing code, and all are 0 at the end of each test.
//!
//! Immutable messages, immutable relationship tables, attachment states only taking registered values and so on are
//! pinned by the source scan of `db::source_guards`.

use gono_forum_web::{
    content::writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    db::{self, Database},
    identity::{self, Actor},
    test_support::{
        TestFixture, assert_messaging_consistent, assert_messaging_rejected_or_flagged,
    },
};

async fn execute(db: &Database, statement: &str) {
    db::query(statement)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn scalar(db: &Database, statement: &str) -> i64 {
    db::query_scalar(statement)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn member(db: &Database, uid: i64) {
    execute(db, &format!("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, regdate) VALUES ({uid}, 'rules-{uid}', 'rules{uid}@example.test', 'hash', 1, 1)")).await;
}

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

fn key(c: char) -> String {
    c.to_string().repeat(64)
}

/// Post a thread and return the first post's id.
async fn publish(db: &Database, uid: i64) -> i64 {
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1").await;
    ContentWriter::new(db.clone(), WriteConfig::default())
        .publish(
            uid,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Rules thread".into(),
                body: "Rules body".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .pid
}

#[tokio::test]
async fn sending_checks_the_participants_and_only_moves_forward() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    for uid in [9301, 9302, 9303] {
        member(db, uid).await;
    }
    let thread = gono_forum_web::messages::send(db, actor(9301), 9302, "First message", &key('a'))
        .await
        .unwrap();
    assert_messaging_consistent(db).await;
    let messages = format!("SELECT COUNT(*) FROM gonof_home_message WHERE thread_id={thread}");

    // The conversation has an extra state row for a third person (only writable bypassing code). Sending checks
    // the participants under the lock, rejects it and rolls back the whole send.
    execute(db, &format!("INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,last_dateline) VALUES({thread},9303,9301,100)")).await;
    assert!(matches!(
        gono_forum_web::messages::send(db, actor(9301), 9302, "Second message", &key('b')).await,
        Err(ContentWriteError::Internal(_))
    ));
    assert_eq!(scalar(db, &messages).await, 1);
    execute(
        db,
        &format!("DELETE FROM gonof_home_message_member WHERE thread_id={thread} AND uid=9303"),
    )
    .await;

    // The conversation's last message was moved into the future: the grow-only conditional update matches nothing and
    // the whole send rolls back without moving it back.
    execute(db, &format!("UPDATE gonof_home_message_thread SET last_message_id=last_message_id+1000000 WHERE id={thread}")).await;
    assert!(matches!(
        gono_forum_web::messages::send(db, actor(9301), 9302, "Third message", &key('c')).await,
        Err(ContentWriteError::Internal(_))
    ));
    assert_eq!(scalar(db, &messages).await, 1);
    execute(db, &format!("UPDATE gonof_home_message_thread SET last_message_id=last_message_id-1000000 WHERE id={thread}")).await;

    // The read position only grows: when the sender's read position is already larger elsewhere, sending does not
    // lower it.
    execute(db, &format!("UPDATE gonof_home_message_member SET last_read_id=last_read_id+1000000 WHERE thread_id={thread} AND uid=9301")).await;
    let ahead = scalar(
        db,
        &format!(
            "SELECT last_read_id FROM gonof_home_message_member WHERE thread_id={thread} AND uid=9301"
        ),
    )
    .await;
    gono_forum_web::messages::send(db, actor(9301), 9302, "Fourth message", &key('d'))
        .await
        .unwrap();
    assert_eq!(
        scalar(db, &format!("SELECT last_read_id FROM gonof_home_message_member WHERE thread_id={thread} AND uid=9301")).await,
        ahead
    );
    assert_eq!(scalar(db, &messages).await, 2);
    assert_messaging_consistent(db).await;
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn request_entries_keep_lowercase_hex_keys_and_hashes() {
    use gono_forum_web::identity::rules;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    member(db, 9311).await;
    gono_forum_web::content::seed_forums(db).await.unwrap();
    let pid = publish(db, 9311).await;
    let before_posts = scalar(db, "SELECT COUNT(*) FROM gonof_forum_post_request").await;
    let mut tx = db.begin_write().await.unwrap();
    for (uid, request_key, request_hash, dateline) in [
        (9311, "short".to_owned(), key('a'), 1),
        (9311, key('A'), key('a'), 1),
        (9311, key('b'), "short".to_owned(), 1),
        (9311, key('b'), key('B'), 1),
        (0, key('b'), key('a'), 1),
        (9311, key('b'), key('a'), -1),
    ] {
        assert!(
            rules::insert_post_request(
                db,
                &mut tx,
                uid,
                &request_key,
                &request_hash,
                pid,
                dateline
            )
            .await
            .is_err(),
            "{uid} {request_key} {request_hash} {dateline}"
        );
        assert!(
            rules::insert_interaction_request(
                db,
                &mut tx,
                uid,
                &request_key,
                &request_hash,
                dateline
            )
            .await
            .is_err(),
            "{uid} {request_key} {request_hash} {dateline}"
        );
    }
    rules::insert_post_request(db, &mut tx, 9311, &key('b'), &key('a'), pid, 1)
        .await
        .unwrap();
    rules::insert_interaction_request(db, &mut tx, 9311, &key('b'), &key('a'), 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_forum_post_request").await,
        before_posts + 1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_home_interaction_request WHERE uid=9311"
        )
        .await,
        1
    );
    assert_messaging_consistent(db).await;
    fixture.cleanup().await.unwrap();
}

/// The remaining invalid values the database does not reject (those already covered by the private message, request,
/// attachment, favorite and like, and block tests are not repeated): written to the database bypassing code, each is
/// detected by the matching item of the check script (written in an uncommitted transaction, rolled back at the end).
#[tokio::test]
async fn the_check_script_reports_the_remaining_kinds_of_drift() {
    // Only verifies the check script, which runs on MySQL only.
    gono_forum_web::mysql_only!();
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    member(db, 9321).await;
    gono_forum_web::content::seed_forums(db).await.unwrap();
    let pid = publish(db, 9321).await;
    let tid = scalar(
        db,
        &format!("SELECT tid FROM gonof_forum_post WHERE pid={pid}"),
    )
    .await;
    let fid = scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_post WHERE pid={pid}"),
    )
    .await;
    execute(db, &format!("INSERT INTO gonof_home_interaction_request(uid,request_key,request_hash,dateline) VALUES(9321,'{}','{}',1)", key('c'), key('d'))).await;
    execute(db, &format!("INSERT INTO gonof_forum_post_revision(pid,version,editor_uid,subject,message,dateline) VALUES({pid},1,9321,'Old title','Old body',1)")).await;
    execute(
        db,
        &format!("INSERT INTO gonof_home_thread_follow(uid,tid,dateline) VALUES(9321,{tid},1)"),
    )
    .await;
    execute(
        db,
        &format!("INSERT INTO gonof_home_forum_follow(uid,fid,dateline) VALUES(9321,{fid},1)"),
    )
    .await;
    assert_messaging_consistent(db).await;

    for (statement, item) in [
        (
            "UPDATE gonof_home_interaction_request SET request_key=UPPER(request_key) WHERE uid=9321".to_owned(),
            "interaction requests whose request key or digest is not 64 lowercase hex characters",
        ),
        (
            "UPDATE gonof_home_interaction_request SET dateline=-1 WHERE uid=9321".to_owned(),
            "interaction requests whose request key or digest is not 64 lowercase hex characters",
        ),
        (
            format!("UPDATE gonof_forum_post_revision SET version=0 WHERE pid={pid}"),
            "post revisions with an invalid version or time",
        ),
        (
            "UPDATE gonof_home_thread_follow SET dateline=-1 WHERE uid=9321".to_owned(),
            "thread or forum follows with invalid values",
        ),
        (
            "UPDATE gonof_home_forum_follow SET dateline=-1 WHERE uid=9321".to_owned(),
            "thread or forum follows with invalid values",
        ),
        (
            "UPDATE gonof_forum_post_request SET request_hash=UPPER(request_hash), request_key='short' WHERE uid=9321".to_owned(),
            "post requests whose request key or digest is not 64 characters",
        ),
    ] {
        assert_messaging_rejected_or_flagged(db, &statement, item).await;
    }
    assert_messaging_consistent(db).await;
    fixture.cleanup().await.unwrap();
}
