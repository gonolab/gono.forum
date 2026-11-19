use std::{collections::BTreeMap, time::Duration};

use gono_forum_web::{
    content::{
        self,
        edits::{ContentEditor, EditRequest},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::Database,
    identity::{self, Actor},
    notifications,
};
use serde_json::Value;

const NOW: i64 = 1_800_060_000;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default())
}
async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::raw_sql(sql)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn member(db: &Database, uid: i64) {
    gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate) VALUES(?,?,?,'contract-password',1,?)")
        .bind(uid).bind(format!("content-notify-{uid}")).bind(format!("content-notify-{uid}@example.test"))
        .bind(NOW-172800).execute(db.executor()).await.unwrap();
}
fn thread(fid: i64, body: &str) -> NewThread {
    NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: "Newly published notification target".into(),
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}
fn reply(tid: i64, root_pid: Option<i64>, reply_to_pid: Option<i64>, body: &str) -> NewReply {
    NewReply {
        tid,
        root_pid,
        reply_to_pid,
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}
async fn publish(db: &Database, uid: i64, fid: i64, body: &str) -> Published {
    writer(db)
        .publish_at(uid, thread(fid, body), NOW)
        .await
        .unwrap()
}
async fn edit(db: &Database, uid: i64, pid: i64, body: &str) {
    let editor = ContentEditor::new(db.clone(), Default::default(), 86_400);
    let post = editor.editable_at(actor(uid), pid, NOW + 1).await.unwrap();
    editor
        .edit_at(
            actor(uid),
            EditRequest {
                visibility: None,
                pid,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                title: post.first.then(|| post.title.clone()),
                body: Some(body.into()),
                request_key: identity::random_token(),
                reason: String::new(),
            },
            NOW + 1,
        )
        .await
        .unwrap();
}
async fn rows(db: &Database, pid: i64) -> BTreeMap<i64, String> {
    gono_forum_web::db::query(
        "SELECT uid,type FROM gonof_home_notification WHERE pid=? ORDER BY uid",
    )
    .bind(pid)
    .fetch_all(db.executor())
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get("uid").unwrap(),
            gono_forum_web::db::text(&row, "type").unwrap(),
        )
    })
    .collect()
}
async fn payload(db: &Database, pid: i64) -> Value {
    let row = gono_forum_web::db::query("SELECT payload FROM gonof_common_outbox WHERE event_type='notification.content' AND aggregate_id=?")
        .bind(pid).fetch_one(db.executor()).await.unwrap();
    serde_json::from_str(&gono_forum_web::db::text(&row, "payload").unwrap()).unwrap()
}
async fn drain(db: &Database, timestamp: i64) {
    for _ in 0..10 {
        let report = notifications::deliver_pending(db, timestamp).await.unwrap();
        assert_eq!(
            report.failed, 0,
            "valid events must not rely on error retries: {report:?}"
        );
        if report.claimed == 0 {
            return;
        }
    }
    panic!("the test event queue did not drain within the bounded batches");
}

async fn contract(db: &Database) {
    db.migrate().await.unwrap();
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=130 {
        member(db, uid).await;
    }
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    priority_and_recipients(db, fid).await;
    edits_use_original_current_intersection(db, fid).await;
    current_permissions(db, fid).await;
    recipient_limits(db, fid).await;
    publication_rollback(db, fid).await;
    delivery_rollback_and_lease(db, fid).await;
    invalid_event_boundaries(db, fid).await;
    mysql_publication_candidate_snapshot(db, fid).await;
    cross_thread_quotes(db, fid).await;
}

async fn mysql_publication_candidate_snapshot(db: &Database, fid: i64) {
    // Only verifies MySQL's row lock wait orchestration (processlist shows the publish waiting for the forum lock):
    // SQLite queues write transactions as a whole, so they start only after getting the lock.
    if db.backend() != gono_forum_web::db::Backend::MySql {
        return;
    }
    {}
    let source = publish(db, 26, fid, "Quote source hidden while the post waits").await;
    let mut moderation = db.begin_write().await.unwrap();
    gono_forum_web::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
        .bind(fid)
        .fetch_one(db.observe(&mut *moderation))
        .await
        .unwrap();
    let service = writer(db);
    let request = thread(
        fid,
        &format!(
            "[quote](post:{}) [@frozen-while-waiting](member:28) [@regular-candidate](member:29)",
            source.pid
        ),
    );
    // Posting takes a shared lock on the forum row, so it still waits while moderation holds an
    // exclusive lock.
    let pending =
        tokio::task::spawn_local(async move { service.publish_at(1, request, NOW).await });
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let waiting=scalar(db,"SELECT COUNT(*) FROM information_schema.processlist WHERE DB=DATABASE() AND ID<>CONNECTION_ID() AND INFO LIKE 'SELECT fid FROM gonof_forum_forum WHERE fid=%LOCK IN SHARE MODE'").await;
            if waiting>0 { break; }
            assert!(!pending.is_finished(),"posting must not finish while the forum lock is held");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the post must be seen waiting for the forum lock before collecting candidates");
    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=-1 WHERE pid=?")
        .bind(source.pid)
        .execute(db.observe(&mut *moderation))
        .await
        .unwrap();
    gono_forum_web::db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=28")
        .execute(db.observe(&mut *moderation))
        .await
        .unwrap();
    moderation.commit().await.unwrap();
    let post = pending.await.unwrap().unwrap();
    assert_eq!(
        payload(db, post.pid).await["quote_pids"],
        serde_json::json!([]),
        "a quote source hidden before publishing must not enter the candidates through an earlier MySQL snapshot"
    );
    assert_eq!(
        payload(db, post.pid).await["mention_uids"],
        serde_json::json!([29]),
        "an account frozen before publishing must not enter the mention candidates through an old snapshot"
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            source.pid
        ),
    )
    .await;
    execute(db, "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=28").await;
    drain(db, NOW + 41).await;
    assert_eq!(
        rows(db, post.pid).await,
        BTreeMap::from([(29, "mention".into())])
    );
}

async fn priority_and_recipients(db: &Database, fid: i64) {
    let topic = publish(db, 2, fid, "Body").await;
    let root = writer(db)
        .reply_at(3, reply(topic.tid, None, None, "Top-level reply"), NOW)
        .await
        .unwrap();
    let child = writer(db)
        .reply_at(
            4,
            reply(topic.tid, Some(root.pid), Some(root.pid), "Nested reply"),
            NOW,
        )
        .await
        .unwrap();
    let quoted = publish(
        db,
        5,
        fid,
        "The quoted source snapshot stays out of notifications",
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET subject='The sensitive quoted title stays out of notifications' WHERE tid={}",
            quoted.tid
        ),
    )
    .await;
    drain(db, NOW).await;
    let body = format!(
        "[@self](member:1) [@root](member:3) [@target](member:4) [@quoter](member:5) [@mentioned](member:6) [@repeated](member:6) [quote](post:{}) [quote again](post:{}) [quote the thread](post:{}) [quote the direct target](post:{})",
        quoted.pid, quoted.pid, topic.pid, child.pid
    );
    let request = reply(topic.tid, Some(root.pid), Some(child.pid), &body);
    let service = writer(db);
    let (first, second) = tokio::join!(
        service.reply_at(1, request.clone(), NOW),
        service.reply_at(1, request.clone(), NOW)
    );
    let (post, second) = (first.unwrap(), second.unwrap());
    assert_eq!(
        (second.pid, second.tid, second.position),
        (post.pid, post.tid, post.position)
    );
    assert_ne!(
        post.replayed, second.replayed,
        "of two concurrent requests with one key only one is the first write"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key='notification.content:{}'",
                post.pid
            )
        )
        .await,
        1
    );
    let event = payload(db, post.pid).await;
    let keys: Vec<_> = event
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "actor_uid",
            "mention_uids",
            "pid",
            "quote_pids",
            "recipient_uids"
        ]
    );
    assert_eq!(event["pid"], post.pid);
    assert_eq!(event["actor_uid"], 1);
    assert_eq!(event["recipient_uids"], serde_json::json!([3, 4]));
    assert!(!event.to_string().contains("snapshot"));
    let (a, b) = tokio::join!(
        notifications::deliver_pending(db, NOW),
        notifications::deliver_pending(db, NOW)
    );
    assert_eq!(a.unwrap().failed + b.unwrap().failed, 0);
    assert_eq!(
        rows(db, post.pid).await,
        BTreeMap::from([
            (2, "quote".into()),
            (3, "reply".into()),
            (4, "reply".into()),
            (5, "quote".into()),
            (6, "mention".into())
        ])
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={}",
                post.pid
            )
        )
        .await,
        5,
        "one notification per recipient"
    );
    for uid in 2..=6 {
        let page = notifications::list(db, actor(uid), 1, 100).await.unwrap();
        let item = page
            .items
            .iter()
            .find(|item| item.pid == Some(post.pid))
            .unwrap();
        assert_eq!(item.kind, rows(db, post.pid).await[&uid]);
        assert_eq!(
            notifications::open(db, actor(uid), item.id, NOW + 1)
                .await
                .unwrap(),
            (post.tid, post.pid)
        );
        assert_eq!(item.subject, "Newly published notification target");
    }

    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid IN (2,3,4)",
    )
    .await;
    let frozen_root = writer(db)
        .reply_at(
            1,
            reply(topic.tid, None, None, "[@still-valid](member:7)"),
            NOW,
        )
        .await
        .unwrap();
    let frozen_child = writer(db)
        .reply_at(
            1,
            reply(
                topic.tid,
                Some(root.pid),
                Some(child.pid),
                "[@still-valid](member:7)",
            ),
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(
        payload(db, frozen_root.pid).await["recipient_uids"],
        serde_json::json!([])
    );
    assert_eq!(
        payload(db, frozen_child.pid).await["recipient_uids"],
        serde_json::json!([])
    );
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=0 WHERE uid IN (2,3,4)",
    )
    .await;
    drain(db, NOW).await;
    for pid in [frozen_root.pid, frozen_child.pid] {
        assert_eq!(
            rows(db, pid).await,
            BTreeMap::from([(7, "mention".into())]),
            "reply candidates also leave out frozen accounts at publishing, and unfreezing adds none"
        );
    }
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=124",
    )
    .await;
    let first_post = publish(db, 1, fid, "[@regular](member:7) [@frozen-before-delivery](member:123) [@frozen-at-publishing](member:124) [@about-to-vanish](member:125) [@missing](member:999999) [@self](member:1)").await;
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=124",
    )
    .await;
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=123",
    )
    .await;
    execute(db, "DELETE FROM gonof_common_member WHERE uid=125").await;
    drain(db, NOW).await;
    assert_eq!(
        rows(db, first_post.pid).await,
        BTreeMap::from([(7, "mention".into())])
    );
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=123",
    )
    .await;
    member(db, 125).await;
    drain(db, NOW + 2).await;
    assert_eq!(
        rows(db, first_post.pid).await.len(),
        1,
        "an unfrozen account does not get events already delivered"
    );
}

async fn edits_use_original_current_intersection(db: &Database, fid: i64) {
    let old_quote = publish(db, 11, fid, "Original quote").await;
    let added_quote = publish(db, 13, fid, "Quote added by an edit").await;
    let source = format!(
        "[@old](member:10) [@kept](member:14) [quote](post:{})",
        old_quote.pid
    );
    let post = publish(db, 1, fid, &source).await;
    let original = payload(db, post.pid).await;
    let count = scalar(db, "SELECT COUNT(*) FROM gonof_common_outbox").await;
    edit(
        db,
        1,
        post.pid,
        &format!(
            "[@added](member:12) [@kept](member:14) [quote](post:{})",
            added_quote.pid
        ),
    )
    .await;
    assert_eq!(
        payload(db, post.pid).await,
        original,
        "an edit must not widen the original candidates"
    );
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_common_outbox").await,
        count
    );
    drain(db, NOW + 1).await;
    assert_eq!(
        rows(db, post.pid).await,
        BTreeMap::from([(14, "mention".into())])
    );
    edit(db, 1, post.pid, &source).await;
    drain(db, NOW + 2).await;
    assert_eq!(
        rows(db, post.pid).await.len(),
        1,
        "editing and restoring the body add no notifications to a delivered event"
    );
    let code = publish(db,1,fid,&format!("Only code `[@fake-mention](member:15)` `[fake quote](post:{})`\n\n```markdown\n[@fake-mention](member:16)\n[fake quote](post:{})\n```", old_quote.pid,added_quote.pid)).await;
    drain(db, NOW + 2).await;
    assert!(rows(db, code.pid).await.is_empty());
}

async fn current_permissions(db: &Database, fid: i64) {
    let source = publish(
        db,
        20,
        fid,
        "The title and body of a private quote source never appear in notifications",
    )
    .await;
    let post = publish(
        db,
        1,
        fid,
        &format!("[quote](post:{}) [@still-valid](member:21)", source.pid),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            source.pid
        ),
    )
    .await;
    drain(db, NOW).await;
    assert_eq!(
        rows(db, post.pid).await,
        BTreeMap::from([(21, "mention".into())])
    );
    let initially_hidden = publish(
        db,
        1,
        fid,
        &format!(
            "[hidden at first](post:{}) [@still-valid](member:21)",
            source.pid
        ),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            source.pid
        ),
    )
    .await;
    drain(db, NOW + 1).await;
    assert_eq!(rows(db, post.pid).await.len(), 1);
    assert_eq!(
        rows(db, initially_hidden.pid).await,
        BTreeMap::from([(21, "mention".into())]),
        "becoming visible again before delivery must not widen the original quote candidates"
    );

    let other_fid = scalar(db, "SELECT MAX(fid) FROM gonof_forum_forum").await;
    assert_ne!(other_fid, fid);
    let private_source = publish(db, 30, other_fid, "Source in a forum that becomes private").await;
    let private_quote = publish(
        db,
        1,
        fid,
        &format!(
            "[quote](post:{}) [@still-delivered](member:31)",
            private_source.pid
        ),
    )
    .await;
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='private' WHERE fid={other_fid}"),
    )
    .await;
    drain(db, NOW).await;
    assert_eq!(
        rows(db, private_quote.pid).await,
        BTreeMap::from([(31, "mention".into())])
    );
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='public' WHERE fid={other_fid}"),
    )
    .await;
    let withdrawn_quote = publish(
        db,
        1,
        fid,
        &format!("[quote](post:{}) [@still-valid](member:21)", source.pid),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at={} WHERE pid={}",
            NOW + 1,
            source.pid
        ),
    )
    .await;
    drain(db, NOW + 1).await;
    assert_eq!(
        rows(db, withdrawn_quote.pid).await,
        BTreeMap::from([(21, "mention".into())])
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={}",
            source.pid
        ),
    )
    .await;
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=20").await;
    let frozen_quote = publish(
        db,
        1,
        fid,
        &format!("[quote](post:{}) [@still-valid](member:21)", source.pid),
    )
    .await;
    execute(db, "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=20").await;
    drain(db, NOW + 2).await;
    assert_eq!(
        rows(db, frozen_quote.pid).await,
        BTreeMap::from([(21, "mention".into())]),
        "the source author was frozen at publishing; unfreezing must not add to the original candidates"
    );

    let hidden = publish(db, 1, fid, "[@must-not-receive](member:22)").await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            hidden.pid
        ),
    )
    .await;
    drain(db, NOW).await;
    assert!(rows(db, hidden.pid).await.is_empty());
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            hidden.pid
        ),
    )
    .await;
    drain(db, NOW + 1).await;
    assert!(rows(db, hidden.pid).await.is_empty());

    let visible = publish(
        db,
        1,
        fid,
        &format!("[quote](post:{}) [@target](member:23)", source.pid),
    )
    .await;
    drain(db, NOW).await;
    let id = scalar(
        db,
        &format!(
            "SELECT id FROM gonof_home_notification WHERE uid=23 AND pid={}",
            visible.pid
        ),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET readperm=100 WHERE tid={}",
            visible.tid
        ),
    )
    .await;
    // The read path judges by the sign of displayorder: after raw SQL changes the read permission,
    // recompute by the official rules.
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    let page = notifications::list(db, actor(23), 1, 100).await.unwrap();
    let item = page.items.iter().find(|item| item.id == id).unwrap();
    assert!(
        !item.available
            && item.pid.is_none()
            && item.tid.is_none()
            && item.author.is_empty()
            && item.subject.is_empty()
    );
    assert!(matches!(
        notifications::open(db, actor(23), id, NOW + 1).await,
        Err(ContentWriteError::NotFound)
    ));
    assert_eq!(
        scalar(
            db,
            &format!("SELECT read_at FROM gonof_home_notification WHERE id={id}")
        )
        .await,
        0
    );
    assert!(matches!(
        notifications::mark_read(db, actor(24), id, NOW + 1).await,
        Err(ContentWriteError::NotFound)
    ));
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET readperm=0 WHERE tid={}",
            visible.tid
        ),
    )
    .await;
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    assert_eq!(
        notifications::open(db, actor(23), id, NOW + 2)
            .await
            .unwrap(),
        (visible.tid, visible.pid)
    );
}

async fn recipient_limits(db: &Database, fid: i64) {
    let mentions = (30..80)
        .map(|uid| format!("[@member](member:{uid})"))
        .collect::<Vec<_>>()
        .join(" ");
    let before = scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await;
    assert!(matches!(
        writer(db)
            .publish_at(
                1,
                thread(fid, &format!("{mentions} [@fifty-first](member:80)")),
                NOW
            )
            .await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await,
        before
    );
    let mut quotes = Vec::new();
    for uid in 81..=130 {
        quotes.push(publish(db, uid, fid, "Quote limit source").await.pid);
    }
    let quote_links = quotes
        .iter()
        .map(|pid| format!("[quote](post:{pid})"))
        .collect::<Vec<_>>()
        .join(" ");
    let extra = publish(db, 2, fid, "Quote source over the limit").await;
    let before = scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await;
    assert!(matches!(
        writer(db)
            .publish_at(
                1,
                thread(
                    fid,
                    &format!("{quote_links} [fifty-first](post:{})", extra.pid)
                ),
                NOW
            )
            .await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await,
        before
    );
    let max = publish(db, 1, fid, &format!("{mentions} {quote_links}")).await;
    drain(db, NOW).await;
    let delivered = rows(db, max.pid).await;
    assert_eq!(delivered.len(), 100);
    assert_eq!(
        delivered
            .values()
            .filter(|kind| kind.as_str() == "mention")
            .count(),
        50
    );
    assert_eq!(
        delivered
            .values()
            .filter(|kind| kind.as_str() == "quote")
            .count(),
        50
    );
    let target = publish(db, 2, fid, "Thread with 102 recipients").await;
    let root = writer(db)
        .reply_at(3, reply(target.tid, None, None, "Top-level reply"), NOW)
        .await
        .unwrap();
    let child = writer(db)
        .reply_at(
            4,
            reply(target.tid, Some(root.pid), Some(root.pid), "Nested reply"),
            NOW,
        )
        .await
        .unwrap();
    let max_reply = writer(db)
        .reply_at(
            1,
            reply(
                target.tid,
                Some(root.pid),
                Some(child.pid),
                &format!("{mentions} {quote_links}"),
            ),
            NOW,
        )
        .await
        .unwrap();
    drain(db, NOW).await;
    let delivered = rows(db, max_reply.pid).await;
    assert_eq!(
        delivered.len(),
        102,
        "50 mentions and 50 quotes do not use up the up to 2 reply recipients"
    );
    assert_eq!(delivered[&3], "reply");
    assert_eq!(delivered[&4], "reply");
    let repeated = publish(db, 1, fid, &"[@repeated](member:25) ".repeat(60)).await;
    drain(db, NOW).await;
    assert_eq!(
        rows(db, repeated.pid).await.len(),
        1,
        "the limit counts distinct targets, not repeated syntax"
    );
}

async fn invalid_event_boundaries(db: &Database, fid: i64) {
    for invalid in ["too_many_mentions", "unknown_snapshot", "wrong_actor"] {
        let post = publish(db, 1, fid, "[@event-check-target](member:26)").await;
        let mut event = payload(db, post.pid).await;
        match invalid {
            "too_many_mentions" => {
                event["mention_uids"] = serde_json::json!((2..=52).collect::<Vec<_>>())
            }
            "unknown_snapshot" => {
                event["source_body"] = Value::String(
                    "Sensitive body that must not reach the notification error".into(),
                )
            }
            "wrong_actor" => event["actor_uid"] = Value::from(2),
            _ => unreachable!(),
        }
        gono_forum_web::db::query("UPDATE gonof_common_outbox SET payload=? WHERE event_type='notification.content' AND aggregate_id=?")
            .bind(event.to_string()).bind(post.pid).execute(db.executor()).await.unwrap();
        drain(db, NOW + 40).await;
        assert!(
            rows(db, post.pid).await.is_empty(),
            "the consumer rejects out-of-bounds or unauthorized payloads: {invalid}"
        );
        let row=gono_forum_web::db::query("SELECT status,last_error FROM gonof_common_outbox WHERE event_type='notification.content' AND aggregate_id=?")
            .bind(post.pid).fetch_one(db.executor()).await.unwrap();
        assert_eq!(gono_forum_web::db::text(&row, "status").unwrap(), "failed");
        let error = gono_forum_web::db::text(&row, "last_error").unwrap();
        assert!(!error.is_empty() && !error.contains("Sensitive body"));
    }
}

async fn install_failure(db: &Database, name: &str, table: &str, operation: &str, condition: &str) {
    gono_forum_web::test_support::fail_when(
        db,
        name,
        &format!("BEFORE {operation}"),
        table,
        condition,
        "notification contract failure",
    )
    .await
    .unwrap()
}
async fn drop_failure(db: &Database, name: &str, _table: &str) {
    {
        gono_forum_web::test_support::drop_trigger(db, name)
            .await
            .unwrap();
    }
}

async fn publication_rollback(db: &Database, fid: i64) {
    let topic = publish(db, 2, fid, "Rollback thread").await;
    let counts = [
        "gonof_forum_thread",
        "gonof_forum_post",
        "gonof_forum_post_request",
        "gonof_common_outbox",
        "gonof_common_log",
    ];
    let mut before = Vec::new();
    for table in counts {
        before.push(scalar(db, &format!("SELECT COUNT(*) FROM {table}")).await);
    }
    let position = scalar(
        db,
        &format!(
            "SELECT maxposition FROM gonof_forum_thread WHERE tid={}",
            topic.tid
        ),
    )
    .await;
    install_failure(
        db,
        "reject_content_event",
        "gonof_common_outbox",
        "INSERT",
        "NEW.event_type='notification.content'",
    )
    .await;
    assert!(
        writer(db)
            .publish_at(1, thread(fid, "[@queued-atomically](member:26)"), NOW)
            .await
            .is_err()
    );
    assert!(
        writer(db)
            .reply_at(
                1,
                reply(topic.tid, None, None, "[@reply-rolls-back](member:26)"),
                NOW
            )
            .await
            .is_err()
    );
    for (index, table) in counts.into_iter().enumerate() {
        assert_eq!(
            scalar(db, &format!("SELECT COUNT(*) FROM {table}")).await,
            before[index],
            "publishing and queueing the notification roll back together: {table}"
        );
    }
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT maxposition FROM gonof_forum_thread WHERE tid={}",
                topic.tid
            )
        )
        .await,
        position
    );
    drop_failure(db, "reject_content_event", "gonof_common_outbox").await;
}

async fn delivery_rollback_and_lease(db: &Database, fid: i64) {
    let source = publish(db, 27, fid, "Quote source").await;
    let target = publish(db, 28, fid, "Lease and rollback target").await;
    let post = writer(db)
        .reply_at(
            1,
            reply(
                target.tid,
                None,
                None,
                &format!(
                    "[@same-reply-recipient](member:28) [quote](post:{})",
                    source.pid
                ),
            ),
            NOW,
        )
        .await
        .unwrap();
    let key = format!("notification.content:{}", post.pid);
    install_failure(
        db,
        "reject_content_ack",
        "gonof_common_outbox",
        "UPDATE",
        &format!("NEW.status='sent' AND NEW.event_key='{key}'"),
    )
    .await;
    assert_eq!(
        notifications::deliver_pending(db, NOW)
            .await
            .unwrap()
            .failed,
        1
    );
    assert!(
        rows(db, post.pid).await.is_empty(),
        "inserts for all recipients and the Outbox ACK roll back as a whole"
    );
    // The failure reason must be kept as it is, not a fixed "temporarily unavailable".
    let row = gono_forum_web::db::query(
        "SELECT status,last_error FROM gonof_common_outbox WHERE event_key=?",
    )
    .bind(&key)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(gono_forum_web::db::text(&row, "status").unwrap(), "pending");
    assert!(
        gono_forum_web::db::text(&row, "last_error")
            .unwrap()
            .contains("notification contract failure"),
        "last_error records the actual error"
    );
    drop_failure(db, "reject_content_ack", "gonof_common_outbox").await;
    execute(db,&format!("UPDATE gonof_common_outbox SET status='processing',lease_token='active-other-worker',lease_until={},available_at={} WHERE event_key='{key}'",NOW+30,NOW)).await;
    drain(db, NOW + 29).await;
    assert!(rows(db, post.pid).await.is_empty());
    drain(db, NOW + 30).await;
    assert_eq!(rows(db, post.pid).await.len(), 2);
    let id = scalar(
        db,
        &format!(
            "SELECT id FROM gonof_home_notification WHERE uid=28 AND pid={}",
            post.pid
        ),
    )
    .await;
    notifications::mark_read(db, actor(28), id, NOW + 31)
        .await
        .unwrap();
    execute(db,&format!("UPDATE gonof_common_outbox SET status='pending',available_at={},lease_until=0 WHERE event_key='{key}'",NOW+32)).await;
    // A confirmed event re-queued and redelivered concurrently must not add notifications or wipe out the read status.
    let (a, b) = tokio::join!(
        notifications::deliver_pending(db, NOW + 32),
        notifications::deliver_pending(db, NOW + 32)
    );
    assert_eq!(a.unwrap().failed + b.unwrap().failed, 0);
    assert_eq!(rows(db, post.pid).await.len(), 2);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={}",
                post.pid
            )
        )
        .await,
        2,
        "redelivering the same event leaves no duplicate notifications"
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT read_at FROM gonof_home_notification WHERE id={id}")
        )
        .await,
        NOW + 31
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={} AND event_key='{key}'",
                post.pid
            )
        )
        .await,
        2
    );
}

async fn cross_thread_quotes(db: &Database, fid: i64) {
    let a = publish(db, 2, fid, "Cross quote A").await;
    let b = publish(db, 3, fid, "Cross quote B").await;
    let source_a = writer(db)
        .reply_at(10, reply(a.tid, None, None, "Quote source in A"), NOW)
        .await
        .unwrap();
    let source_b = writer(db)
        .reply_at(11, reply(b.tid, None, None, "Quote source in B"), NOW)
        .await
        .unwrap();
    drain(db, NOW).await;
    let request_a = reply(
        a.tid,
        None,
        None,
        &format!("[quote across threads to B](post:{})", source_b.pid),
    );
    let request_b = reply(
        b.tid,
        None,
        None,
        &format!("[quote across threads to A](post:{})", source_a.pid),
    );
    let writer_a = writer(db);
    let writer_b = writer(db);
    let (posted_a, posted_b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            writer_a.reply_at(4, request_a, NOW),
            writer_b.reply_at(5, request_b, NOW)
        )
    })
    .await
    .expect("queueing quotes across threads must not wait on locks in opposite order");
    let (posted_a, posted_b) = (posted_a.unwrap(), posted_b.unwrap());
    let (delivered_a, delivered_b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            notifications::deliver_pending(db, NOW),
            notifications::deliver_pending(db, NOW)
        )
    })
    .await
    .expect("delivering quotes across threads must take locks in a consistent order");
    assert_eq!(delivered_a.unwrap().failed + delivered_b.unwrap().failed, 0);
    assert_eq!(
        rows(db, posted_a.pid).await,
        BTreeMap::from([(2, "reply".into()), (11, "quote".into())])
    );
    assert_eq!(
        rows(db, posted_b.pid).await,
        BTreeMap::from([(3, "reply".into()), (10, "quote".into())])
    );
}

#[tokio::test]
async fn mysql_content_notifications_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    tokio::task::LocalSet::new()
        .run_until(contract(&services.db))
        .await;
    services.cleanup().await.unwrap();
}

/// A mention the page shows as plain text, because it is nested past `markdown::MAX_NESTING`, notifies no one; one
/// in the same post within the limit still does.
#[tokio::test]
async fn mentions_flattened_past_the_nesting_limit_notify_no_one() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    let db = &services.db;
    tokio::task::LocalSet::new()
        .run_until(async {
            db.migrate().await.unwrap();
            content::seed_forums(db).await.unwrap();
            for uid in 1..=3 {
                member(db, uid).await;
            }
            let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
            let body = format!(
                "[@shown](member:2)\n\n{} [@flattened](member:3)",
                "> ".repeat(34)
            );
            let post = publish(db, 1, fid, &body).await;
            drain(db, NOW).await;
            assert_eq!(
                rows(db, post.pid).await,
                BTreeMap::from([(2, "mention".into())])
            );
        })
        .await;
    services.cleanup().await.unwrap();
}
