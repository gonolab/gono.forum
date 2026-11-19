//! The database contract of private messages: conversations are unique by pair_key, one state row per party, bodies
//! cannot be rewritten (values and rewrites are guaranteed by code and check scripts).
use gono_forum_web::{
    db::{self, Database},
    test_support::TestFixture,
};
use std::{sync::Arc, time::Duration};

async fn members(db: &Database) {
    for uid in [9201_i64, 9202, 9203] {
        db::query("INSERT INTO gonof_common_member(uid,username,regdate) VALUES(?,?,1)")
            .bind(uid)
            .bind(format!("message-{uid}"))
            .execute(db.executor())
            .await
            .unwrap();
    }
}

async fn ok(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

/// Gives all members a verified email `username@example.test`: the email is assembled in code and written one by one
/// (the same statement on both backends).
async fn verified_emails(db: &Database) {
    let members = db::query("SELECT uid,username FROM gonof_common_member")
        .fetch_all(db.executor())
        .await
        .unwrap();
    for member in members {
        db::query("UPDATE gonof_common_member SET password='synthetic-hash',email=?,emailstatus=1 WHERE uid=?")
            .bind(format!("{}@example.test", db::text(&member, "username").unwrap()))
            .bind(member.try_get::<i64, _>("uid").unwrap())
            .execute(db.executor())
            .await
            .unwrap();
    }
}

async fn rejected(db: &Database, sql: &str) {
    assert!(
        db::query(sql).execute(db.executor()).await.is_err(),
        "should have been rejected: {sql}"
    );
}

/// The database contract of the three private message tables: unique keys and foreign keys are rejected by the
/// database; other invalid values are never written by code (`messages::rules` and the send entry points), anything
/// written to the database bypassing code is found by the messaging check script (`tests/consistency/messaging.sql`),
/// and the immutable, append-only columns of messages are pinned by a source scan.
#[tokio::test]
async fn message_schema_keeps_unique_keys_and_the_check_script_catches_the_rest() {
    use gono_forum_web::test_support::{
        assert_messaging_consistent, assert_messaging_rejected_or_flagged as flagged,
    };
    let fixture = TestFixture::migrated().await.unwrap();
    let db = fixture.db.clone();
    members(&db).await;
    verified_emails(&db).await;
    let sender = gono_forum_web::identity::Actor {
        uid: 9202,
        credentials_version: 1,
        api_key: None,
    };
    let thread = gono_forum_web::messages::send(&db, sender, 9201, "Hello", &"a".repeat(64))
        .await
        .unwrap();
    let key: String = db::query_scalar("SELECT pair_key FROM gonof_home_message_thread WHERE id=?")
        .bind(thread)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(
        key, "9201:9202",
        "the pair key is <smaller uid>:<larger uid>"
    );
    assert_messaging_consistent(&db).await;

    // Still rejected by the database: one conversation per pair of members, one row per party, state rows point to an
    // existing conversation, event_key unique.
    rejected(
        &db,
        "INSERT INTO gonof_home_message_thread(pair_key,dateline) VALUES('9201:9202',101)",
    )
    .await;
    rejected(&db, &format!("INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,last_dateline) VALUES({thread},9201,9202,100)")).await;
    rejected(&db, "INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,last_dateline) VALUES(999999,9203,9201,100)").await;
    let event_key: String =
        db::query_scalar("SELECT event_key FROM gonof_home_message WHERE thread_id=?")
            .bind(thread)
            .fetch_one(db.executor())
            .await
            .unwrap();
    rejected(&db, &format!("INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key) VALUES({thread},9201,'Duplicate',101,'{event_key}')")).await;

    // Invalid conversation identifiers the database does not reject: reversed order, sending to oneself, missing
    // separator, leading zeros, non-numeric, three parts.
    for bad in [
        "9203:9202",
        "9201:9201",
        "9201",
        "09201:9203",
        "a:9203",
        "9201:9202:9203",
    ] {
        flagged(
            &db,
            &format!(
                "INSERT INTO gonof_home_message_thread(pair_key,dateline) VALUES('{bad}',100)"
            ),
            "conversations whose pair key is malformed or out of order",
        )
        .await;
    }
    // Rewriting the conversation identifier, swapping the other party, a state row for a third person: the state rows
    // and the conversation's two parties do not match.
    for statement in [
        format!("UPDATE gonof_home_message_thread SET pair_key='9201:9203' WHERE id={thread}"),
        format!(
            "UPDATE gonof_home_message_member SET counterpart_uid=9203 WHERE thread_id={thread} AND uid=9201"
        ),
        format!(
            "INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,last_dateline) VALUES({thread},9203,9201,100)"
        ),
    ] {
        flagged(
            &db,
            &statement,
            "conversations whose state rows disagree with the two members of the pair key",
        )
        .await;
    }
    // The last message moves backwards.
    flagged(
        &db,
        &format!(
            "UPDATE gonof_home_message_thread SET last_message_id=last_message_id-1 WHERE id={thread}"
        ),
        "conversations whose last message is not their newest message",
    )
    .await;
    // The other party is oneself; unread count negative.
    for statement in [
        format!(
            "UPDATE gonof_home_message_member SET counterpart_uid=9201 WHERE thread_id={thread} AND uid=9201"
        ),
        format!(
            "UPDATE gonof_home_message_member SET unread=-1 WHERE thread_id={thread} AND uid=9201"
        ),
    ] {
        flagged(
            &db,
            &statement,
            "private message state rows with invalid values",
        )
        .await;
    }
    // Body of 1 to 2000 characters (the body is TEXT, with no column width as a backstop). "\u{5b57}" is the Chinese
    // character for "character", three bytes in UTF-8: 2000 of them pass, so the limit counts characters, not bytes.
    for body in [String::new(), "\u{5b57}".repeat(2001)] {
        flagged(
            &db,
            &format!("INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key) VALUES({thread},9201,'{body}',100,'k-raw')"),
            "private messages with an invalid body, event key or time",
        )
        .await;
    }
    ok(&db, &format!("INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key) VALUES({thread},9201,'{}',100,'k-max')", "\u{5b57}".repeat(2000))).await;
    db::query("DELETE FROM gonof_home_message WHERE event_key='k-max'")
        .execute(db.executor())
        .await
        .unwrap();
    // The sender is replaced by someone outside the conversation.
    flagged(
        &db,
        &format!("UPDATE gonof_home_message SET sender_uid=9203 WHERE thread_id={thread}"),
        "private messages whose sender is not one of the two members of the conversation",
    )
    .await;
    // Not stopped by the database: "changing the body, time or idempotency key after sending" and "the read position
    // moving backwards"; the code has no statement that changes message rows, and the read position and last time of
    // state rows are only written with GREATEST, pinned by `db::source_guards`; the entry points reject messages to
    // oneself and overlong bodies.
    let me = gono_forum_web::identity::Actor {
        uid: 9201,
        credentials_version: 1,
        api_key: None,
    };
    for (to, body) in [
        (9201, "To myself"),
        (9202, &*"x".repeat(2001)),
        (9202, "  "),
    ] {
        assert!(matches!(
            gono_forum_web::messages::send(&db, me, to, body, &"b".repeat(64)).await,
            Err(gono_forum_web::content::writes::ContentWriteError::Validation(_))
        ));
    }

    // The conversation list is fetched by the member's own row, without OR or CASE.
    let inbox: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_home_message_member WHERE uid=9201 ORDER BY last_dateline DESC",
    )
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(inbox, 1);
    assert_messaging_consistent(&db).await;
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn replaying_messages_preserves_unread_and_the_latest_message() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let sender = gono_forum_web::identity::Actor {
        uid: 9201,
        credentials_version: 1,
        api_key: None,
    };
    let first_key = "a".repeat(64);
    let second_key = "b".repeat(64);
    assert!(matches!(
        gono_forum_web::messages::send(db, sender, 9202, "Uppercase request key", &"A".repeat(64))
            .await,
        Err(gono_forum_web::content::writes::ContentWriteError::Validation(_))
    ));
    let thread = gono_forum_web::messages::send(db, sender, 9202, "First message", &first_key)
        .await
        .unwrap();
    gono_forum_web::messages::send(db, sender, 9202, "Second message", &second_key)
        .await
        .unwrap();
    gono_forum_web::messages::send(db, sender, 9202, "Second message", &second_key)
        .await
        .unwrap();
    let unread: i64 = db::query_scalar(
        "SELECT unread FROM gonof_home_message_member WHERE thread_id=? AND uid=9202",
    )
    .bind(thread)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(
        unread, 2,
        "replaying the latest message must not add to the unread count again"
    );

    gono_forum_web::messages::conversation(db, 9202, thread, Some(1), 20)
        .await
        .unwrap();
    assert_eq!(
        gono_forum_web::messages::send(db, sender, 9202, "First message", &first_key)
            .await
            .unwrap(),
        thread,
        "replaying an older message returns the conversation without moving the latest message back"
    );
    let (inbox, _) = gono_forum_web::messages::inbox(db, 9202, 1, 20)
        .await
        .unwrap();
    assert_eq!(inbox[0].preview, "Second message");
    assert_eq!(
        inbox[0].unread, 0,
        "a replay must not mark a read conversation unread again"
    );
    let count: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_home_message WHERE thread_id=?")
        .bind(thread)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(count, 2);
    // The same request key with a different body: report a conflict, never silently treat it as a successful replay.
    assert!(matches!(
        gono_forum_web::messages::send(db, sender, 9202, "Edited second message", &second_key)
            .await,
        Err(gono_forum_web::content::writes::ContentWriteError::Conflict(_))
    ));
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn opening_a_conversation_cannot_clear_messages_it_did_not_read() {
    // Only verifies the orchestration of MySQL row lock waits.
    gono_forum_web::mysql_only!();
    use gono_forum_web::debug::trace::{self, RequestProfile};

    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let thread = gono_forum_web::messages::send(
        db,
        gono_forum_web::identity::Actor {
            uid: 9201,
            credentials_version: 1,
            api_key: None,
        },
        9202,
        "Existing message",
        &"c".repeat(64),
    )
    .await
    .unwrap();

    // Simulates a send transaction that holds the conversation and recipient state locks and has not committed yet.
    let mut sending = db.begin_write().await.unwrap();
    db::query("SELECT id FROM gonof_home_message_thread WHERE id=? FOR UPDATE")
        .bind(thread)
        .fetch_one(db.observe(&mut *sending))
        .await
        .unwrap();
    db::query(
        "SELECT uid FROM gonof_home_message_member WHERE thread_id=? AND uid=9202 FOR UPDATE",
    )
    .bind(thread)
    .fetch_one(db.observe(&mut *sending))
    .await
    .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let reading = trace::scope(
        profile.clone(),
        gono_forum_web::messages::conversation(db, 9202, thread, Some(1), 20),
    );
    let finish_sending = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if profile.snapshot().iter().any(|record| {
                    record.sql.contains("UPDATE gonof_home_message_member")
                        || (record.sql.contains("FROM gonof_home_message_thread")
                            && record.sql.contains("FOR UPDATE"))
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the read must reach the locks held by the send transaction");
        let now = gono_forum_web::identity::now();
        let message = db::query("INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key) VALUES(?,9201,'Concurrent message',?,'concurrent-message')")
            .bind(thread).bind(now).execute(db.observe(&mut *sending)).await.unwrap();
        db::query("UPDATE gonof_home_message_thread SET last_message_id=? WHERE id=?")
            .bind(message.last_insert_id() as i64)
            .bind(thread)
            .execute(db.observe(&mut *sending))
            .await
            .unwrap();
        db::query(
            "UPDATE gonof_home_message_member SET unread=unread+1 WHERE thread_id=? AND uid=9202",
        )
        .bind(thread)
        .execute(db.observe(&mut *sending))
        .await
        .unwrap();
        sending.commit().await.unwrap();
    };
    let (view, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(reading, finish_sending)
    })
    .await
    .unwrap();
    let view = view.unwrap();
    let unread = gono_forum_web::messages::unread_conversations(db, 9202)
        .await
        .unwrap();
    assert!(
        view.messages
            .iter()
            .any(|message| message.body == "Concurrent message")
            || unread > 0,
        "the message must appear in this read, or the unread notice must remain"
    );
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn reopening_a_read_conversation_takes_no_write_transaction() {
    use gono_forum_web::debug::trace::{self, RequestProfile};

    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let thread = gono_forum_web::messages::send(
        db,
        gono_forum_web::identity::Actor {
            uid: 9201,
            credentials_version: 1,
            api_key: None,
        },
        9202,
        "Existing message",
        &"c".repeat(64),
    )
    .await
    .unwrap();
    // The first open has to advance the read position, still through the conversation lock.
    let first = Arc::new(RequestProfile::new());
    trace::scope(
        first.clone(),
        gono_forum_web::messages::conversation(db, 9202, thread, None, 20),
    )
    .await
    .unwrap();
    // The debug record is the text actually executed: on SQLite the locking read is removed by the rewriter (the whole
    // write transaction queues), so look at the write that resets the count.
    let locked_clear = |record: &gono_forum_web::debug::trace::QueryRecord| {
        if db.backend() == gono_forum_web::db::Backend::MySql {
            record.sql.contains("FROM gonof_home_message_thread")
                && record.sql.contains("FOR UPDATE")
        } else {
            record.sql.contains("UPDATE gonof_home_message_member")
        }
    };
    assert!(
        first.snapshot().iter().any(locked_clear),
        "unread messages are cleared under the conversation lock"
    );

    // Once the read position has caught up, reopening or paging has nothing to write, and should no longer open a write
    // transaction or contend for the conversation lock.
    for page in [None, Some(1)] {
        let again = Arc::new(RequestProfile::new());
        let view = trace::scope(
            again.clone(),
            gono_forum_web::messages::conversation(db, 9202, thread, page, 20),
        )
        .await
        .unwrap();
        assert_eq!(view.messages.len(), 1);
        let sql = again
            .snapshot()
            .into_iter()
            .map(|record| record.sql)
            .collect::<Vec<_>>();
        assert!(
            !sql.iter().any(|sql| sql.contains("FOR UPDATE")),
            "reopening a read conversation must not lock the conversation row: {sql:?}"
        );
        assert!(
            !sql.iter()
                .any(|sql| sql.contains("UPDATE gonof_home_message_member")),
            "an unchanged read position must not write the state row: {sql:?}"
        );
    }
    // After the other party sends another message, the next open goes back to the locking reset path.
    gono_forum_web::messages::send(
        db,
        gono_forum_web::identity::Actor {
            uid: 9201,
            credentials_version: 1,
            api_key: None,
        },
        9202,
        "New message",
        &"d".repeat(64),
    )
    .await
    .unwrap();
    let after = Arc::new(RequestProfile::new());
    trace::scope(
        after.clone(),
        gono_forum_web::messages::conversation(db, 9202, thread, None, 20),
    )
    .await
    .unwrap();
    assert!(
        after
            .snapshot()
            .iter()
            .any(|record| record.sql.contains("UPDATE gonof_home_message_member")),
        "a new message must be cleared again on the next open"
    );
    assert_eq!(
        gono_forum_web::messages::unread_conversations(db, 9202)
            .await
            .unwrap(),
        0
    );
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn message_pages_reject_out_of_range_offsets_without_marking_read() {
    use gono_forum_web::content::writes::ContentWriteError;

    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    ok(db, "INSERT INTO gonof_home_message_thread(id,pair_key,last_message_id,dateline) VALUES(1,'9201:9202',1,1)").await;
    ok(db, "INSERT INTO gonof_home_message_member(thread_id,uid,counterpart_uid,unread,last_dateline) VALUES(1,9202,9201,1,1)").await;
    ok(db, "INSERT INTO gonof_home_message(id,thread_id,sender_uid,body,dateline,event_key) VALUES(1,1,9201,'Unread message',1,'pagination')").await;
    for page in [usize::MAX, 2] {
        assert!(matches!(
            gono_forum_web::messages::inbox(db, 9202, page, 20).await,
            Err(ContentWriteError::NotFound)
        ));
        assert!(matches!(
            gono_forum_web::messages::conversation(db, 9202, 1, Some(page), 20).await,
            Err(ContentWriteError::NotFound)
        ));
    }
    assert_eq!(
        gono_forum_web::messages::unread_conversations(db, 9202)
            .await
            .unwrap(),
        1
    );
    fixture.cleanup().await.unwrap();
}

/// When the conversation already has a later time (clock rollback, multi-machine skew), new messages are
/// still sent and the time does not move backwards.
#[tokio::test]
async fn sending_after_a_clock_step_back_keeps_time_monotonic() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let sender = gono_forum_web::identity::Actor {
        uid: 9201,
        credentials_version: 1,
        api_key: None,
    };
    let thread = gono_forum_web::messages::send(db, sender, 9202, "First message", &"c".repeat(64))
        .await
        .unwrap();
    let ahead = gono_forum_web::identity::now() + 3600;
    db::query("UPDATE gonof_home_message_member SET last_dateline=? WHERE thread_id=?")
        .bind(ahead)
        .bind(thread)
        .execute(db.executor())
        .await
        .unwrap();
    gono_forum_web::messages::send(db, sender, 9202, "Second message", &"d".repeat(64))
        .await
        .expect("sending after a clock step back must not be rejected");
    let datelines: Vec<i64> =
        db::query_scalar("SELECT last_dateline FROM gonof_home_message_member WHERE thread_id=?")
            .bind(thread)
            .fetch_all(db.executor())
            .await
            .unwrap();
    assert!(datelines.iter().all(|value| *value >= ahead));
    let latest: i64 = db::query_scalar(
        "SELECT dateline FROM gonof_home_message WHERE thread_id=? ORDER BY id DESC LIMIT 1",
    )
    .bind(thread)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert!(
        latest >= ahead,
        "the new message is dated no earlier than the existing time"
    );
    fixture.cleanup().await.unwrap();
}

/// Persistent deadlocks retry the whole operation 3 times and then report a conflict (409), not 404 or
/// 500.
#[tokio::test]
async fn persistent_lock_conflicts_retry_then_report_a_conflict() {
    // Only verifies faking MySQL's deadlock error number with SIGNAL.
    gono_forum_web::mysql_only!();
    use gono_forum_web::content::writes::ContentWriteError;
    use gono_forum_web::debug::trace::{self, RequestProfile};
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    gono_forum_web::db::raw_sql("CREATE TRIGGER message_deadlock_contract BEFORE INSERT ON gonof_home_message FOR EACH ROW BEGIN SIGNAL SQLSTATE '40001' SET MYSQL_ERRNO=1213, MESSAGE_TEXT='message deadlock contract'; END")
        .execute(&db.pool)
        .await
        .unwrap();
    let sender = gono_forum_web::identity::Actor {
        uid: 9201,
        credentials_version: 1,
        api_key: None,
    };
    let profile = Arc::new(RequestProfile::new());
    let result = trace::scope(
        profile.clone(),
        gono_forum_web::messages::send(db, sender, 9202, "Deadlocked message", &"e".repeat(64)),
    )
    .await;
    assert!(
        matches!(result, Err(ContentWriteError::Conflict(_))),
        "{result:?}"
    );
    let inserts = profile
        .snapshot()
        .iter()
        .filter(|record| record.sql.contains("INSERT INTO gonof_home_message("))
        .count();
    assert_eq!(inserts, 3, "the whole send is tried 3 times");
    fixture.cleanup().await.unwrap();
}

fn actor(uid: i64) -> gono_forum_web::identity::Actor {
    gono_forum_web::identity::Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

// What an open shows stops at the conversation's last message as read with the read position: a message committed
// after that is not shown on the read-only path either, so a message on the page is never left unread.
#[tokio::test]
async fn an_open_shows_no_message_newer_than_its_read_position() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let thread =
        gono_forum_web::messages::send(db, actor(9201), 9202, "Existing message", &"a".repeat(64))
            .await
            .unwrap();
    gono_forum_web::messages::conversation(db, 9202, thread, None, 20)
        .await
        .unwrap();
    // A send between the open's state read and its page read: its message row is there, while the conversation's
    // last message and the recipient's unread are what the open read (the send's own updates are left out here, to
    // hold that moment).
    db::query("INSERT INTO gonof_home_message(thread_id,sender_uid,body,dateline,event_key) VALUES(?,9201,'Committed after the state read',?,'late-message')")
        .bind(thread)
        .bind(gono_forum_web::identity::now())
        .execute(db.executor())
        .await
        .unwrap();
    for page in [None, Some(1)] {
        let view = gono_forum_web::messages::conversation(db, 9202, thread, page, 20)
            .await
            .unwrap();
        assert_eq!(
            view.messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["Existing message"],
            "page {page:?}"
        );
        assert_eq!(view.total, 1);
    }
    fixture.cleanup().await.unwrap();
}

// Clearing unread holds the conversation lock only to read the last message id and advance the read position: the
// messages are counted and read after the commit.
#[tokio::test]
async fn clearing_unread_counts_and_reads_after_the_lock() {
    use gono_forum_web::debug::trace::{self, RequestProfile};

    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    verified_emails(db).await;
    let mut thread = 0;
    for n in 0..25 {
        thread = gono_forum_web::messages::send(
            db,
            actor(9201),
            9202,
            &format!("Message {n}"),
            &format!("{n:064x}"),
        )
        .await
        .unwrap();
    }
    let profile = Arc::new(RequestProfile::new());
    let view = trace::scope(
        profile.clone(),
        gono_forum_web::messages::conversation(db, 9202, thread, None, 20),
    )
    .await
    .unwrap();
    assert_eq!((view.page, view.total, view.messages.len()), (2, 25, 5));
    assert_eq!(view.messages[0].body, "Message 20");
    assert_eq!(view.messages[4].body, "Message 24");
    assert_eq!(
        gono_forum_web::messages::unread_conversations(db, 9202)
            .await
            .unwrap(),
        0
    );
    let sql = profile
        .snapshot()
        .into_iter()
        .map(|record| record.sql)
        .collect::<Vec<_>>();
    let position = |what: &str, test: &dyn Fn(&str) -> bool| {
        sql.iter()
            .position(|sql| test(sql))
            .unwrap_or_else(|| panic!("no {what}: {sql:?}"))
    };
    let clear = position("unread reset", &|sql| {
        sql.contains("UPDATE gonof_home_message_member")
    });
    let count = position("message count", &|sql| {
        sql.contains("COUNT(*) FROM gonof_home_message WHERE")
    });
    let page = position("page read", &|sql| {
        sql.contains("SELECT id,sender_uid,body,dateline FROM gonof_home_message")
    });
    assert!(
        clear < count && clear < page,
        "the count and the page are read inside the locking transaction: {sql:?}"
    );
    fixture.cleanup().await.unwrap();
}

// Conversations active in the same second are listed by their latest message, also across page boundaries.
#[tokio::test]
async fn conversations_of_the_same_second_follow_their_latest_message() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    members(db).await;
    ok(
        db,
        "INSERT INTO gonof_common_member(uid,username,regdate) VALUES(9204,'message-9204',1)",
    )
    .await;
    verified_emails(db).await;
    let send = |from: i64, n: u8| async move {
        let key = char::from(b'a' + n).to_string().repeat(64);
        gono_forum_web::messages::send(db, actor(from), 9204, "In the same second", &key).await
    };
    // Conversation ids in creation order; the first one gets the latest message.
    let first = send(9201, 0).await.unwrap();
    let second = send(9202, 1).await.unwrap();
    let third = send(9203, 2).await.unwrap();
    assert_eq!(send(9201, 3).await.unwrap(), first);
    ok(
        db,
        "UPDATE gonof_home_message_member SET last_dateline=1000000 WHERE uid=9204",
    )
    .await;
    let order = |page: usize, size: usize| async move {
        gono_forum_web::messages::inbox(db, 9204, page, size)
            .await
            .unwrap()
            .0
            .into_iter()
            .map(|conversation| conversation.thread_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(1, 20).await, [first, third, second]);
    assert_eq!(order(1, 2).await, [first, third]);
    assert_eq!(order(2, 2).await, [second]);
    for (page, expected) in [(1, first), (2, third), (3, second)] {
        assert_eq!(order(page, 1).await, [expected], "page {page} of one");
    }
    // A newer second still comes first, whatever the message ids.
    ok(
        db,
        &format!("UPDATE gonof_home_message_member SET last_dateline=1000001 WHERE uid=9204 AND thread_id={second}"),
    )
    .await;
    assert_eq!(order(1, 20).await, [second, first, third]);
    assert_eq!(order(2, 2).await, [third]);
    fixture.cleanup().await.unwrap();
}
