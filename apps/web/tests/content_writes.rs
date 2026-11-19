use futures_util::future::join_all;
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    identity,
};

const NOW: i64 = 1_800_050_000;
/// "\u{8349}" is a Chinese character ("grass"): one character, three bytes in UTF-8, so length limits counted in
/// characters and in bytes differ.
const WIDE: &str = "\u{8349}";

async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::query(sql)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn rejected_sql(db: &Database, sql: &str) {
    assert!(
        gono_forum_web::db::query(sql)
            .execute(&db.pool)
            .await
            .is_err(),
        "a write that breaks a constraint must be rejected: {sql}"
    );
}

async fn member(db: &Database, uid: i64, regdate: i64, role: i64, verified: bool, frozen: bool) {
    gono_forum_web::db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, `freeze`, regdate) VALUES (?, ?, ?, 'test-password-hash', ?, ?, ?, ?)")
        .bind(uid).bind(format!("writer-{uid}")).bind(format!("writer-{uid}@example.test"))
        .bind(i64::from(verified)).bind(role).bind(i64::from(frozen)).bind(regdate)
        .execute(&db.pool).await.unwrap();
}

fn topic(fid: i64) -> NewThread {
    NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: "Transaction thread🍓".into(),
        body: "First post **Markdown**\n\nKept body".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

fn reply(tid: i64) -> NewReply {
    NewReply {
        tid,
        root_pid: None,
        reply_to_pid: None,
        body: "Top-level reply🍓".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

fn child(tid: i64, root: i64, target: i64) -> NewReply {
    NewReply {
        root_pid: Some(root),
        reply_to_pid: Some(target),
        ..reply(tid)
    }
}

fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default())
}

async fn write_counts(db: &Database) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "gonof_forum_thread",
        "gonof_forum_post_tableid",
        "gonof_forum_post",
        "gonof_forum_post_request",
        "gonof_forum_attachment_reference",
        "gonof_common_log",
        "gonof_common_outbox",
    ] {
        counts.push(scalar(db, &format!("SELECT COUNT(*) FROM {table}")).await);
    }
    counts
}

async fn initialize_baseline(db: &Database) {
    db.migrate().await.unwrap();
}

// The MySQL transactional business contract; running the baseline again proves the author, thread and first post are
// kept.
async fn content_contract(db: &Database) {
    initialize_baseline(db).await;
    content::seed_forums(db).await.unwrap();
    content::seed_fixture(db).await.unwrap();
    let before_row =
        gono_forum_web::db::query("SELECT message FROM gonof_forum_post WHERE tid=103 AND first=1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let before = db::text(&before_row, "message").unwrap();
    let original_counts = write_counts_before_upgrade(db).await;
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(
        scalar(db, "SELECT MAX(version) FROM _sqlx_migrations").await,
        gono_forum_web::db::migrations::LATEST
    );
    assert_eq!(write_counts_before_upgrade(db).await, original_counts);
    assert_eq!(
        gono_forum_web::test_support::thread_with_all_pages(db, 103)
            .await
            .unwrap()
            .unwrap()
            .body,
        vec![before]
    );
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug = 'daily'").await;
    for (uid, role, verified, frozen) in [
        (1001, 1, false, false),
        (1002, 0, true, false),
        (1003, 0, false, false),
        (1004, 0, true, true),
        (1005, 2, false, false),
    ] {
        member(db, uid, NOW - 200_000, role, verified, frozen).await;
    }
    assert_permissions_and_validation(db, fid).await;
    assert_idempotency(db, fid).await;
    assert_parallel_floors(db, fid).await;
    assert_last_reply_summary(db, fid).await;
    assert_quota_boundaries(db, fid).await;
    assert_targets_and_visibility(db, fid).await;
    assert_attachments_and_rollback(db, fid).await;
    assert_constraints(db, fid).await;
}

async fn write_counts_before_upgrade(db: &Database) -> Vec<i64> {
    let mut counts = vec![];
    for table in [
        "gonof_common_member",
        "gonof_forum_thread",
        "gonof_forum_post",
        "gonof_forum_post_tableid",
    ] {
        counts.push(scalar(db, &format!("SELECT COUNT(*) FROM {table}")).await);
    }
    counts
}

async fn assert_permissions_and_validation(db: &Database, fid: i64) {
    let writer = writer(db);
    let before = write_counts(db).await;
    assert!(matches!(
        writer.publish_at(99999, topic(fid), NOW).await,
        Err(ContentWriteError::Unauthorized)
    ));
    for uid in [1003, 1004, 1005] {
        assert!(matches!(
            writer.publish_at(uid, topic(fid), NOW).await,
            Err(ContentWriteError::Forbidden(_))
        ));
    }
    for title in [
        " ".to_owned(),
        "🍓".repeat(256),
        "Title\nnewline".to_owned(),
    ] {
        let mut request = topic(fid);
        request.title = title;
        assert!(matches!(
            writer.publish_at(1001, request, NOW).await,
            Err(ContentWriteError::Validation(_))
        ));
    }
    // The body limit counts characters: `WIDE` is three bytes in UTF-8, so the longest body accepted below is
    // 60,000 bytes.
    for body in [" ".to_owned(), WIDE.repeat(20_001), "Null\0char".to_owned()] {
        let mut request = topic(fid);
        request.body = body;
        assert!(matches!(
            writer.publish_at(1001, request, NOW).await,
            Err(ContentWriteError::Validation(_))
        ));
    }
    let mut invalid_key = topic(fid);
    invalid_key.request_key = "g".repeat(64);
    assert!(matches!(
        writer.publish_at(1001, invalid_key, NOW).await,
        Err(ContentWriteError::Validation(_))
    ));
    // Uppercase hex is invalid too: request keys are 64 lowercase characters at every entry point.
    let mut uppercase_key = topic(fid);
    uppercase_key.request_key = uppercase_key.request_key.to_uppercase();
    assert!(matches!(
        writer.publish_at(1001, uppercase_key, NOW).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(write_counts(db).await, before);
    let request = NewThread {
        body: WIDE.repeat(20_000),
        title: format!(" {} ", "🍓".repeat(255)),
        ..topic(fid)
    };
    let published = writer.publish_at(1001, request, NOW).await.unwrap();
    let row = gono_forum_web::db::query("SELECT t.subject, p.message FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.tid = t.tid AND p.first = 1 WHERE t.tid = ?")
        .bind(published.tid).fetch_one(&db.pool).await.unwrap();
    assert_eq!(db::text(&row, "subject").unwrap(), "🍓".repeat(255));
    assert_eq!(db::text(&row, "message").unwrap().len(), 60_000);
    // Administrators are exempt from verification and quotas, but once frozen even an existing idempotent request
    // cannot obtain write permission.
    let request = topic(fid);
    let published = writer.publish_at(1001, request.clone(), NOW).await.unwrap();
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = 1001",
    )
    .await;
    assert!(matches!(
        writer.publish_at(1001, request, NOW).await,
        Err(ContentWriteError::Forbidden(_))
    ));
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze` = 0 WHERE uid = 1001",
    )
    .await;
    assert_eq!(published.position, 1);
}

/// Concurrent requests with the same key: all get the same floor, and only one is the first write; the rest are marked
/// as in-transaction replays.
fn assert_single_write(results: &[Result<Published, ContentWriteError>], expected: Published) {
    let key = |p: &Published| (p.pid, p.tid, p.position);
    let published: Vec<Published> = results.iter().map(|r| *r.as_ref().unwrap()).collect();
    assert!(published.iter().all(|p| key(p) == key(&expected)));
    assert_eq!(published.iter().filter(|p| !p.replayed).count(), 1);
}

async fn assert_idempotency(db: &Database, fid: i64) {
    let writer = writer(db);
    let request = topic(fid);
    let before = write_counts(db).await;
    let results = join_all((0..12).map(|_| writer.publish_at(1002, request.clone(), NOW))).await;
    let first = *results[0].as_ref().unwrap();
    assert_single_write(&results, first);
    let after = write_counts(db).await;
    assert_eq!(
        after
            .iter()
            .zip(&before)
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>(),
        vec![1, 1, 1, 1, 0, 1, 0]
    );
    let mut changed = request.clone();
    changed.body.push('!');
    assert!(matches!(
        writer.publish_at(1002, changed, NOW).await,
        Err(ContentWriteError::Conflict(_))
    ));
    assert_eq!(write_counts(db).await, after);
    let reply_request = reply(first.tid);
    let results =
        join_all((0..12).map(|_| writer.reply_at(1002, reply_request.clone(), NOW))).await;
    let reply_result = *results[0].as_ref().unwrap();
    assert_single_write(&results, reply_result);
    assert_eq!(reply_result.position, 2);
    let child_request = child(first.tid, reply_result.pid, reply_result.pid);
    let results =
        join_all((0..12).map(|_| writer.reply_at(1002, child_request.clone(), NOW))).await;
    let child_result = *results[0].as_ref().unwrap();
    assert_single_write(&results, child_result);
    assert_eq!(child_result.position, 3);
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.content' AND aggregate_id IN ({},{})",reply_result.pid,child_result.pid)).await,
        2,
        "repeated replies and nested replies each write one notification event"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT sub_replies_count FROM gonof_forum_post WHERE pid = {}",
                reply_result.pid
            )
        )
        .await,
        1
    );
    let changed = NewReply {
        request_key: request.request_key,
        ..reply(first.tid)
    };
    assert!(matches!(
        writer.reply_at(1002, changed, NOW).await,
        Err(ContentWriteError::Conflict(_))
    ));
}

async fn assert_parallel_floors(db: &Database, fid: i64) {
    let writer = writer(db);
    let thread = writer.publish_at(1001, topic(fid), NOW).await.unwrap();
    for uid in 1100..1106 {
        member(db, uid, NOW - 200_000, 0, true, false).await;
    }
    let requests = (0..60)
        .map(|index| (1100 + index % 6, reply(thread.tid)))
        .collect::<Vec<_>>();
    let results = join_all(
        requests
            .into_iter()
            .map(|(uid, request)| writer.reply_at(uid, request, NOW)),
    )
    .await;
    let mut floors = results
        .into_iter()
        .map(|result| result.unwrap().position)
        .collect::<Vec<_>>();
    floors.sort_unstable();
    assert_eq!(floors, (2..=61).collect::<Vec<_>>());
    let root = scalar(
        db,
        &format!(
            "SELECT pid FROM gonof_forum_post WHERE tid = {} AND position = 2",
            thread.tid
        ),
    )
    .await;
    let results =
        join_all((0..24).map(|index| {
            writer.reply_at(1100 + index % 6, child(thread.tid, root, root), NOW + 1)
        }))
        .await;
    let mut floors = results
        .into_iter()
        .map(|result| result.unwrap().position)
        .collect::<Vec<_>>();
    floors.sort_unstable();
    assert_eq!(floors, (62..=85).collect::<Vec<_>>());
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT replies FROM gonof_forum_thread WHERE tid = {}",
                thread.tid
            )
        )
        .await,
        84
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT maxposition FROM gonof_forum_thread WHERE tid = {}",
                thread.tid
            )
        )
        .await,
        85
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT lastpost FROM gonof_forum_thread WHERE tid = {}",
                thread.tid
            )
        )
        .await,
        NOW + 1
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT sub_replies_count FROM gonof_forum_post WHERE pid = {root}")
        )
        .await,
        24
    );
    let target = scalar(
        db,
        &format!(
            "SELECT pid FROM gonof_forum_post WHERE tid = {} AND position = 62",
            thread.tid
        ),
    )
    .await;
    let nested = writer
        .reply_at(1001, child(thread.tid, root, target), NOW)
        .await
        .unwrap();
    assert_eq!(nested.position, 86);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT lastpost FROM gonof_forum_thread WHERE tid = {}",
                thread.tid
            )
        )
        .await,
        NOW + 1,
        "a server clock going back does not move the last reply time back"
    );
}

async fn assert_last_reply_summary(db: &Database, fid: i64) {
    let writer = writer(db);
    let topic = writer.publish_at(1001, topic(fid), NOW).await.unwrap();
    member(db, 1301, NOW - 200_000, 0, true, false).await;
    execute(
        db,
        "UPDATE gonof_common_member SET username='summarystar🍓' WHERE uid=1301",
    )
    .await;
    let latest = writer
        .reply_at(
            1301,
            NewReply {
                body: "ab🍓".repeat(90),
                ..reply(topic.tid)
            },
            NOW + 2,
        )
        .await
        .unwrap();
    let earlier = writer
        .reply_at(
            1001,
            NewReply {
                body: "An earlier reply must not replace the latest summary".into(),
                ..reply(topic.tid)
            },
            NOW + 1,
        )
        .await
        .unwrap();
    let expected_summary = "ab🍓".repeat(85);
    let check = async || {
        let row = gono_forum_web::db::query("SELECT last_reply_pid,last_reply_author,last_reply_summary,lastpost FROM gonof_forum_thread WHERE tid=?")
            .bind(topic.tid).fetch_one(db.executor()).await.unwrap();
        assert_eq!(row.get::<i64, _>("last_reply_pid"), latest.pid);
        assert_eq!(row.get::<String, _>("last_reply_author"), "summarystar🍓");
        assert_eq!(row.get::<String, _>("last_reply_summary"), expected_summary);
        assert_eq!(row.get::<i64, _>("lastpost"), NOW + 2);
    };
    check().await;
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86_400);
    let actor = identity::Actor {
        uid: 1001,
        credentials_version: 1,
        api_key: None,
    };
    let editable = editor
        .editable_at(actor, earlier.pid, NOW + 3)
        .await
        .unwrap();
    editor
        .change_at(
            actor,
            ChangeRequest {
                pid: earlier.pid,
                action: ContentAction::Hide,
                expected_post_version: editable.version,
                expected_thread_version: editable.thread_version,
                request_key: identity::random_token(),
                reason: "Moderation refresh keeps the same Unicode summary".into(),
            },
            NOW + 3,
        )
        .await
        .unwrap();
    check().await;
}

async fn assert_quota_boundaries(db: &Database, fid: i64) {
    let writer = writer(db);
    let registered = NOW - 100;
    member(db, 1200, registered, 0, true, false).await;
    let requests = (0..10).map(|_| topic(fid)).collect::<Vec<_>>();
    let results = join_all(
        requests
            .into_iter()
            .map(|request| writer.publish_at(1200, request, NOW)),
    )
    .await;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter(|result| result.is_err())
            .all(|result| matches!(result, Err(ContentWriteError::Quota(..))))
    );
    let tid = results.into_iter().find_map(|r| r.ok()).unwrap().tid;
    let request = reply(tid);
    let first_reply = writer.reply_at(1200, request.clone(), NOW).await.unwrap();
    let results = join_all((0..12).map(|_| writer.reply_at(1200, reply(tid), NOW))).await;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 4);
    assert!(
        results
            .iter()
            .filter(|result| result.is_err())
            .all(|result| matches!(result, Err(ContentWriteError::Quota(..))))
    );
    assert_eq!(
        writer.reply_at(1200, request, NOW).await.unwrap(),
        Published {
            replayed: true,
            ..first_reply
        },
        "the same request still returns the original result after the quota is reached"
    );
    assert!(
        matches!(
            writer
                .publish_at(1200, topic(fid), registered + 86_399)
                .await,
            Err(ContentWriteError::Quota(_, 1))
        ),
        "the new-member allowance lasts one more second"
    );
    writer
        .publish_at(1200, topic(fid), registered + 86_400)
        .await
        .unwrap();
    // The calendar-day boundary in the site time zone (seeded as UTC+8), not a rolling 24 hours and not UTC midnight.
    let day_start =
        gono_forum_web::clock::day_start(NOW, gono_forum_web::test_support::SEEDED_OFFSET_MINUTES);
    assert_ne!(day_start, NOW - NOW.rem_euclid(86_400));
    member(db, 1201, day_start - 200_000, 0, true, false).await;
    let results =
        join_all((0..12).map(|_| writer.publish_at(1201, topic(fid), day_start + 86_399))).await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 5);
    assert!(
        results
            .iter()
            .filter(|r| r.is_err())
            .all(|r| matches!(r, Err(ContentWriteError::Quota(_, 1)))),
        "the daily allowance starts again in one second, at the site's midnight"
    );
    writer
        .publish_at(1201, topic(fid), day_start + 86_400)
        .await
        .unwrap();
    member(db, 1202, day_start - 200_000, 0, true, false).await;
    let results =
        join_all((0..26).map(|_| writer.reply_at(1202, reply(tid), day_start + 86_399))).await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 20);
    assert!(
        results
            .iter()
            .filter(|r| r.is_err())
            .all(|r| matches!(r, Err(ContentWriteError::Quota(..))))
    );
    writer
        .reply_at(1202, reply(tid), day_start + 86_400)
        .await
        .unwrap();
    // After switching the site time zone to UTC the day boundary moves to UTC midnight: 23:59 on the previous UTC day
    // and 00:00 on the current day are the same day in UTC+8 but two days on a UTC site. The quota check reads the
    // settings in the write gate, so changing the database takes effect immediately.
    let set_offset = |minutes: &'static str| {
        gono_forum_web::db::query(
            "UPDATE gonof_common_settings SET svalue=? WHERE skey='settings.time.offset_minutes'",
        )
        .bind(minutes)
        .execute(&db.pool)
    };
    set_offset("0").await.unwrap();
    let utc_midnight = NOW - NOW.rem_euclid(86_400) + 2 * 86_400;
    assert_eq!(
        gono_forum_web::clock::day_start(utc_midnight - 1, 480),
        gono_forum_web::clock::day_start(utc_midnight, 480),
        "both moments are the same day in UTC+8"
    );
    member(db, 1203, utc_midnight - 400_000, 0, true, false).await;
    let results =
        join_all((0..7).map(|_| writer.publish_at(1203, topic(fid), utc_midnight - 1))).await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 5);
    writer
        .publish_at(1203, topic(fid), utc_midnight)
        .await
        .expect("a new day on a UTC site");
    set_offset("480").await.unwrap();
    assert!(
        matches!(
            writer.publish_at(1203, topic(fid), utc_midnight + 1).await,
            Err(ContentWriteError::Quota(_, 57_599))
        ),
        "back in UTC+8 both moments are the same day and the quota is used up until the next UTC+8 midnight"
    );
    // Deleting/hiding does not refund the quota.
    execute(
        db,
        "UPDATE gonof_forum_post SET invisible = 1, deleted_at = 1 WHERE authorid = 1201",
    )
    .await;
    assert!(matches!(
        writer
            .publish_at(1201, topic(fid), day_start + 86_399)
            .await,
        Err(ContentWriteError::Quota(..))
    ));
    let results = join_all((0..12).map(|_| writer.publish_at(1001, topic(fid), NOW))).await;
    assert!(
        results.into_iter().all(|r| r.is_ok()),
        "administrators are not limited by the posting quota"
    );
}

async fn assert_targets_and_visibility(db: &Database, fid: i64) {
    let writer = writer(db);
    let thread = writer.publish_at(1001, topic(fid), NOW).await.unwrap();
    let root = writer.reply_at(1001, reply(thread.tid), NOW).await.unwrap();
    let second_root = writer.reply_at(1001, reply(thread.tid), NOW).await.unwrap();
    let child_post = writer
        .reply_at(1001, child(thread.tid, root.pid, root.pid), NOW)
        .await
        .unwrap();
    let other_thread = writer.publish_at(1001, topic(fid), NOW).await.unwrap();
    let before = write_counts(db).await;
    for request in [
        child(thread.tid, thread.pid, thread.pid),
        child(thread.tid, child_post.pid, child_post.pid),
        child(thread.tid, root.pid, second_root.pid),
        child(other_thread.tid, root.pid, root.pid),
        child(thread.tid, root.pid, other_thread.pid),
    ] {
        assert!(matches!(
            writer.reply_at(1001, request, NOW).await,
            Err(ContentWriteError::NotFound)
        ));
    }
    let mut invalid = reply(thread.tid);
    invalid.reply_to_pid = Some(root.pid);
    assert!(matches!(
        writer.reply_at(1001, invalid, NOW).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(write_counts(db).await, before);
    for (table, id, value, reset) in [
        (
            "gonof_forum_thread",
            format!("tid = {}", thread.tid),
            "closed = 1",
            "closed = 0",
        ),
        (
            "gonof_forum_thread",
            format!("tid = {}", thread.tid),
            "visibility = 'private'",
            "visibility = 'public'",
        ),
        (
            "gonof_forum_thread",
            format!("tid = {}", thread.tid),
            "publication_state = 'draft'",
            "publication_state = 'published'",
        ),
        (
            "gonof_forum_thread",
            format!("tid = {}", thread.tid),
            "readperm = 1",
            "readperm = 0",
        ),
        (
            "gonof_forum_thread",
            format!("tid = {}", thread.tid),
            "displayorder = -3",
            "displayorder = 0",
        ),
        (
            "gonof_forum_post",
            format!("pid = {}", thread.pid),
            "invisible = 1",
            "invisible = 0",
        ),
        (
            "gonof_forum_forum",
            format!("fid = {fid}"),
            "status = 0",
            "status = 1",
        ),
        (
            "gonof_forum_forum",
            format!("fid = {fid}"),
            "visibility = 'private'",
            "visibility = 'public'",
        ),
    ] {
        execute(db, &format!("UPDATE {table} SET {value} WHERE {id}")).await;
        assert!(matches!(
            writer.reply_at(1001, reply(thread.tid), NOW).await,
            Err(ContentWriteError::NotFound | ContentWriteError::Forbidden(_))
        ));
        execute(db, &format!("UPDATE {table} SET {reset} WHERE {id}")).await;
    }
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible = 1 WHERE pid = {}",
            root.pid
        ),
    )
    .await;
    assert!(matches!(
        writer
            .reply_at(1001, child(thread.tid, root.pid, child_post.pid), NOW)
            .await,
        Err(ContentWriteError::NotFound)
    ));
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible = 0 WHERE pid = {}",
            root.pid
        ),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at = 1 WHERE pid = {}",
            child_post.pid
        ),
    )
    .await;
    assert_eq!(write_counts(db).await, before);
    assert!(
        writer
            .reply_at(1001, child(thread.tid, root.pid, child_post.pid), NOW)
            .await
            .is_ok(),
        "withdrawal only masks the target body and still allows answering the discussion"
    );
    // Permanent erasure differs from withdrawal: the page no longer offers the "Reply" entry on erased floors, and the
    // server must refuse too, whether the erased one is the reply target or the parent floor.
    let second_child = writer
        .reply_at(
            1001,
            child(thread.tid, second_root.pid, second_root.pid),
            NOW,
        )
        .await
        .unwrap();
    for (erased, request) in [
        (child_post.pid, child(thread.tid, root.pid, child_post.pid)),
        (
            second_root.pid,
            child(thread.tid, second_root.pid, second_child.pid),
        ),
    ] {
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_post SET message='',deleted_at={NOW},erased_at={NOW},version=version+1 WHERE pid = {erased}"
            ),
        )
        .await;
        let before = write_counts(db).await;
        assert!(
            matches!(
                writer.reply_at(1001, request, NOW).await,
                Err(ContentWriteError::NotFound)
            ),
            "the erased floor {erased} cannot be answered any more"
        );
        assert_eq!(write_counts(db).await, before);
    }
}

async fn attachment(
    db: &Database,
    aid: i64,
    uid: i64,
    state: &str,
    scan: &str,
    purge: Option<i64>,
) {
    gono_forum_web::db::query(
        "INSERT INTO gonof_forum_attachment (aid,uid,request_key,request_hash) VALUES (?,?,?,?)",
    )
    .bind(aid)
    .bind(uid)
    .bind(identity::random_token())
    .bind("b".repeat(64))
    .execute(&db.pool)
    .await
    .unwrap();
    gono_forum_web::db::query("INSERT INTO gonof_forum_attachment_0 (aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,width,height,description,alt_text,dateline,updated_at,checked_at,purge_after,lifecycle,scan_status,scan_detail) VALUES (?,'coins.png',123,'image/png',? ,?,'test',10,10,'','alt text',?,?,?, ?,?,?, '')")
        .bind(aid).bind("c".repeat(64)).bind(format!("uploads/{aid}"))
        .bind(NOW-20).bind(NOW-10).bind(NOW-10).bind(purge).bind(state).bind(scan)
        .execute(&db.pool).await.unwrap();
}

async fn assert_attachments_and_rollback(db: &Database, fid: i64) {
    let writer = writer(db);
    for (aid, uid, state, scan, purge) in [
        (1, 1001, "ready", "clean", Some(NOW + 100)),
        (2, 1002, "ready", "clean", Some(NOW + 100)),
        (3, 1001, "processing", "pending", Some(NOW + 100)),
        (4, 1001, "ready", "clean", Some(NOW)),
        (5, 1001, "ready", "rejected", Some(NOW + 100)),
        (6, 1001, "purging", "clean", None),
        (7, 1001, "ready", "clean", Some(NOW + 100)),
    ] {
        attachment(db, aid, uid, state, scan, purge).await;
    }
    for aid in [2, 3, 4, 5, 6, 999] {
        let before = write_counts(db).await;
        let request = NewThread {
            attachment_ids: vec![1, aid],
            ..topic(fid)
        };
        assert!(matches!(
            writer.publish_at(1001, request, NOW).await,
            Err(ContentWriteError::Validation(_))
        ));
        assert_eq!(
            write_counts(db).await,
            before,
            "a failed attachment binding must roll back the thread, first post, PID allocation, idempotency and log"
        );
        assert_eq!(
            scalar(
                db,
                "SELECT purge_after FROM gonof_forum_attachment_0 WHERE aid = 1"
            )
            .await,
            NOW + 100,
            "the retention update of the attachment locked before must roll back too"
        );
    }
    let request = NewThread {
        attachment_ids: vec![7, 1, 1],
        body: "![image](attachment:1) [file](attachment:7)".into(),
        ..topic(fid)
    };
    let first = writer.publish_at(1001, request.clone(), NOW).await.unwrap();
    let reordered = NewThread {
        attachment_ids: vec![1, 7],
        ..request
    };
    assert_eq!(
        writer.publish_at(1001, reordered, NOW).await.unwrap(),
        Published {
            replayed: true,
            ..first
        }
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE pid = {}",
                first.pid
            )
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE aid IN (1,7) AND purge_after IS NULL"
        )
        .await,
        2
    );
    let root = writer.reply_at(1001, reply(first.tid), NOW).await.unwrap();
    let before = write_counts(db).await;
    let positions = scalar(
        db,
        &format!(
            "SELECT maxposition FROM gonof_forum_thread WHERE tid = {}",
            first.tid
        ),
    )
    .await;
    let request = NewReply {
        attachment_ids: vec![1, 3],
        ..child(first.tid, root.pid, root.pid)
    };
    assert!(matches!(
        writer.reply_at(1001, request, NOW).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(write_counts(db).await, before);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT maxposition FROM gonof_forum_thread WHERE tid = {}",
                first.tid
            )
        )
        .await,
        positions
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT sub_replies_count FROM gonof_forum_post WHERE pid = {}",
                root.pid
            )
        )
        .await,
        0
    );
    // Two posts of the same account may reference the same attachment; reference keys still cannot repeat.
    let with_attachment = NewReply {
        attachment_ids: vec![1],
        ..child(first.tid, root.pid, root.pid)
    };
    writer.reply_at(1001, with_attachment, NOW).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid = 1"
        )
        .await,
        2
    );
}

async fn assert_constraints(db: &Database, fid: i64) {
    let writer = writer(db);
    let upper = NewThread {
        request_key: "A".repeat(64),
        ..topic(fid)
    };
    let lower = NewThread {
        request_key: "a".repeat(64),
        ..topic(fid)
    };
    // Request keys only accept 64 lowercase hex characters (the same rule as interactions, reports, data requests and
    // other entry points); uppercase is no longer accepted as a different key.
    assert!(matches!(
        writer.publish_at(1001, upper, NOW).await,
        Err(ContentWriteError::Validation(_))
    ));
    let first = writer.publish_at(1001, lower, NOW).await.unwrap();
    // Foreign keys are still rejected by the database.
    for statement in [
        "UPDATE gonof_forum_post_request SET pid = 999999999 WHERE uid = 1002".into(),
        "UPDATE gonof_forum_attachment_reference SET pid = 999999999 WHERE aid = 1".into(),
        "DELETE FROM gonof_forum_attachment WHERE aid = 1".into(),
        format!("DELETE FROM gonof_forum_post WHERE pid = {}", first.pid),
    ] {
        rejected_sql(db, &statement).await;
    }
    // Request and attachment values are not rejected by the database: requests are only written through the
    // `identity::rules` entry points and attachments only on upload once checks pass, checked before writing; anything
    // written by bypassing the entry points is found by the check scripts (`tests/consistency/`).
    for (statement, item) in [
        (
            "UPDATE gonof_forum_post_request SET request_key = 'short' WHERE uid = 1002 LIMIT 1",
            "post requests whose request key or digest is not 64 characters",
        ),
        (
            "UPDATE gonof_forum_post_request SET request_hash = 'short' WHERE uid = 1002",
            "post requests whose request key or digest is not 64 characters",
        ),
        (
            "UPDATE gonof_forum_attachment SET request_key = 'short' WHERE aid = 1",
            "attachments whose request key or digest is not 64 characters",
        ),
        (
            "UPDATE gonof_forum_attachment_0 SET lifecycle = 'missing' WHERE aid = 1",
            "attachment rows with invalid values",
        ),
        (
            "UPDATE gonof_forum_attachment_0 SET scan_status = 'missing' WHERE aid = 1",
            "attachment rows with invalid values",
        ),
        (
            "UPDATE gonof_forum_attachment_0 SET width = NULL WHERE aid = 1",
            "attachment rows with invalid values",
        ),
        (
            "UPDATE gonof_forum_attachment_0 SET filesize = -1 WHERE aid = 1",
            "attachment rows with invalid values",
        ),
        (
            "UPDATE gonof_forum_attachment_0 SET updated_at = dateline - 1 WHERE aid = 1",
            "attachment rows dated before their upload",
        ),
    ] {
        gono_forum_web::test_support::assert_messaging_rejected_or_flagged(db, statement, item)
            .await;
    }
    let fk_problem = { 0 };
    assert_eq!(fk_problem, 0);
}

#[tokio::test]
async fn mysql_content_writes_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    content_contract(&services.db).await;
    services.cleanup().await.unwrap();
}
