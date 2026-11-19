use std::sync::Arc;

use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::Database,
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
    notifications,
};

const NOW: i64 = 1_800_050_000;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::raw_sql(sql)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn assert_unread_projection(db: &Database) {
    let members = gono_forum_web::db::query(
        "SELECT uid,unread_notifications FROM gonof_common_member ORDER BY uid",
    )
    .fetch_all(db.executor())
    .await
    .unwrap();
    for member in members {
        let uid: i64 = member.try_get("uid").unwrap();
        let unread: i64 = member.try_get("unread_notifications").unwrap();
        let actual: i64 = gono_forum_web::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid=? AND read_at=0",
        )
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(
            unread, actual,
            "the unread notification count of member {uid} must match the committed rows"
        );
    }
}
fn reply(tid: i64, root: Option<i64>, target: Option<i64>) -> NewReply {
    NewReply {
        tid,
        root_pid: root,
        reply_to_pid: target,
        body: "Notification test reply".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}
fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default())
}

async fn contract(db: &Database) {
    db.migrate().await.unwrap();
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    for uid in 1..=5 {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(?,?,?,'test',1,?,?)")
            .bind(uid).bind(format!("notify-{uid}")).bind(format!("notify-{uid}@example.test"))
            .bind(i64::from(uid==1)).bind(NOW-172800).execute(db.executor()).await.unwrap();
    }
    let writer = writer(db);
    let topic = writer
        .publish_at(
            1,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Secret notification visibility title".into(),
                body: "First post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.content'"
        )
        .await,
        0
    );
    let root = writer
        .reply_at(2, reply(topic.tid, None, None), NOW)
        .await
        .unwrap();
    let child = writer
        .reply_at(3, reply(topic.tid, Some(root.pid), Some(root.pid)), NOW)
        .await
        .unwrap();
    let request = reply(topic.tid, Some(root.pid), Some(child.pid));
    let nested = writer.reply_at(4, request.clone(), NOW).await.unwrap();
    assert_eq!(
        writer.reply_at(4, request, NOW).await.unwrap(),
        Published {
            replayed: true,
            ..nested
        }
    );
    writer
        .reply_at(2, reply(topic.tid, Some(root.pid), Some(root.pid)), NOW)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.content'"
        )
        .await,
        4
    );
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_home_notification").await,
        0
    );
    let (a, b) = tokio::join!(
        notifications::deliver_pending(db, NOW),
        notifications::deliver_pending(db, NOW)
    );
    a.unwrap();
    b.unwrap();
    assert_unread_projection(db).await;
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_home_notification").await,
        4
    );
    assert_eq!(scalar(db,"SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.content' AND status='sent'").await,4);
    assert_eq!(notifications::unread_count(db, actor(1)).await.unwrap(), 1);
    assert_eq!(notifications::unread_count(db, actor(2)).await.unwrap(), 2);
    assert_eq!(notifications::unread_count(db, actor(3)).await.unwrap(), 1);
    assert_eq!(notifications::unread_count(db, actor(4)).await.unwrap(), 0);
    assert!(matches!(
        notifications::list(db, actor(0), 1, 20).await,
        Err(ContentWriteError::Unauthorized)
    ));
    assert!(matches!(
        notifications::list(
            db,
            Actor {
                uid: 2,
                credentials_version: 99,
                api_key: None,
            },
            1,
            20
        )
        .await,
        Err(ContentWriteError::Unauthorized)
    ));

    let profile = Arc::new(RequestProfile::new());
    let page = trace::scope(profile.clone(), notifications::list(db, actor(2), 1, 20))
        .await
        .unwrap();
    let reads: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .collect();
    assert_eq!(
        reads.len(),
        4,
        "the notification page runs four queries: member check, newest id, page count, current page"
    );
    assert!(reads[0].sql.contains("unread_notifications"));
    assert!(
        reads[1]
            .sql
            .contains("SELECT MAX(id) FROM gonof_home_notification")
    );
    assert!(
        reads[2]
            .sql
            .contains("SELECT COUNT(*) FROM (SELECT 1 FROM gonof_home_notification n")
            && reads[2].sql.contains("LIMIT ?) capped"),
        "the page count stops at the page cap: {}",
        reads[2].sql
    );
    assert!(
        !reads[2].sql.contains("read_at"),
        "the unread count comes from the member row"
    );
    assert_eq!((page.total, page.unread, page.items.len()), (2, 2, 2));
    let empty = notifications::list(db, actor(4), 1, 20).await.unwrap();
    assert_eq!((empty.total, empty.unread), (0, 0));
    assert_eq!(
        empty.max_id, page.max_id,
        "the bound is the newest notification of the site"
    );
    assert!(empty.items.is_empty());
    assert!(page.items.iter().all(|n| n.available
        && n.author.starts_with("notify-")
        && n.subject == "Secret notification visibility title"));
    let child_id = page
        .items
        .iter()
        .find(|n| n.pid == Some(child.pid))
        .unwrap()
        .id;
    let nested_id = page
        .items
        .iter()
        .find(|n| n.pid == Some(nested.pid))
        .unwrap()
        .id;
    assert!(matches!(
        notifications::open(db, actor(3), child_id, NOW + 1).await,
        Err(ContentWriteError::NotFound)
    ));
    assert!(matches!(
        notifications::open_url(db, actor(3), child_id, NOW + 1).await,
        Err(ContentWriteError::NotFound)
    ));
    assert!(matches!(
        notifications::mark_read(db, actor(3), child_id, NOW + 1).await,
        Err(ContentWriteError::NotFound)
    ));
    assert_eq!(
        notifications::unread_count(db, actor(2)).await.unwrap(),
        2,
        "GET and requests for another member mark nothing read"
    );
    execute(
        db,
        "UPDATE gonof_common_member SET emailstatus=0 WHERE uid=2",
    )
    .await;
    assert_eq!(notifications::unread_count(db, actor(2)).await.unwrap(), 2);
    for result in [
        notifications::mark_read(db, actor(2), child_id, NOW + 1).await,
        notifications::mark_all_read(db, actor(2), page.max_id, "", NOW + 1).await,
    ] {
        assert!(matches!(result, Err(ContentWriteError::Forbidden(_))));
    }
    assert!(matches!(
        notifications::open(db, actor(2), child_id, NOW + 1).await,
        Err(ContentWriteError::Forbidden(_))
    ));
    execute(
        db,
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid=2",
    )
    .await;
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=2").await;
    assert!(matches!(
        notifications::mark_read(db, actor(2), child_id, NOW + 1).await,
        Err(ContentWriteError::Forbidden(_))
    ));
    execute(db, "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=2").await;

    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            root.pid
        ),
    )
    .await;
    // The nested reply's parent-floor copy root_invisible and the first-post-hidden displayorder are maintained by
    // code: after raw SQL, recompute them by the regular rules.
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    let hidden = notifications::list(db, actor(2), 1, 20).await.unwrap();
    assert_eq!(hidden.unread, 2);
    assert!(hidden.items.iter().all(|n| !n.available
        && n.pid.is_none()
        && n.tid.is_none()
        && n.author.is_empty()
        && n.subject.is_empty()));
    assert!(matches!(
        notifications::open(db, actor(2), nested_id, NOW + 2).await,
        Err(ContentWriteError::NotFound)
    ));
    assert!(matches!(
        notifications::open_url(db, actor(2), nested_id, NOW + 2).await,
        Err(ContentWriteError::NotFound)
    ));
    assert_eq!(notifications::unread_count(db, actor(2)).await.unwrap(), 2);
    notifications::mark_read(db, actor(2), child_id, NOW + 3)
        .await
        .unwrap();
    notifications::mark_read(db, actor(2), child_id, NOW + 4)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            &format!("SELECT read_at FROM gonof_home_notification WHERE id={child_id}")
        )
        .await,
        NOW + 3
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0,deleted_at={} WHERE pid={}",
            NOW + 3,
            root.pid
        ),
    )
    .await;
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    assert!(
        notifications::list(db, actor(2), 1, 20)
            .await
            .unwrap()
            .items
            .iter()
            .all(|n| n.available),
        "withdrawing the parent reply does not hide its nested replies"
    );
    let expected_location = content::post_location(
        db,
        topic.tid as u64,
        nested.pid,
        false,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap()
    .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let location = trace::scope(
        profile.clone(),
        notifications::open_url(db, actor(2), nested_id, NOW + 5),
    )
    .await
    .unwrap();
    assert_eq!(location, expected_location);
    let reads: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|record| {
            record.source == QuerySource::Application && record.sql.starts_with("SELECT")
        })
        .collect();
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.sql.contains("FROM gonof_common_member WHERE uid"))
            .count(),
        1,
        "opening an ordinary notification locks and checks the member once"
    );
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.sql.contains("FROM gonof_home_notification WHERE uid"))
            .count(),
        1,
        "the owner, kind and target of a notification are read in one query"
    );
    assert_eq!(notifications::unread_count(db, actor(2)).await.unwrap(), 0);
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={}",
            root.pid
        ),
    )
    .await;
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_home_notification").await,
        4
    );

    // Mark all as read only covers the account-level upper bound at render time; notifications delivered later stay
    // unread.
    let cutoff = notifications::list(db, actor(3), 1, 20)
        .await
        .unwrap()
        .max_id;
    let later = writer
        .reply_at(
            4,
            reply(topic.tid, Some(root.pid), Some(child.pid)),
            NOW + 10,
        )
        .await
        .unwrap();
    notifications::deliver_pending(db, NOW + 10).await.unwrap();
    notifications::mark_all_read(db, actor(3), cutoff, "", NOW + 11)
        .await
        .unwrap();
    notifications::mark_all_read(db, actor(3), cutoff, "", NOW + 12)
        .await
        .unwrap();
    assert_eq!(notifications::unread_count(db, actor(3)).await.unwrap(), 1);
    let later_id = scalar(
        db,
        &format!(
            "SELECT id FROM gonof_home_notification WHERE uid=3 AND pid={}",
            later.pid
        ),
    )
    .await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT read_at FROM gonof_home_notification WHERE id={later_id}")
        )
        .await,
        0
    );
    let admin_id = scalar(db, "SELECT id FROM gonof_home_notification WHERE uid=1").await;
    execute(
        db,
        "UPDATE gonof_common_member SET emailstatus=0 WHERE uid=1",
    )
    .await;
    notifications::mark_read(db, actor(1), admin_id, NOW + 12)
        .await
        .unwrap();

    // Hide first, then deliver the terminating event; restoring does not resend; delivered content shows the original
    // row again once restored.
    let suppressed = writer
        .reply_at(
            4,
            reply(topic.tid, Some(root.pid), Some(child.pid)),
            NOW + 20,
        )
        .await
        .unwrap();
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            suppressed.pid
        ),
    )
    .await;
    notifications::deliver_pending(db, NOW + 20).await.unwrap();
    assert_eq!(scalar(db,&format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE aggregate_id={} AND event_type='notification.content' AND status='failed'",suppressed.pid)).await,1);
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            suppressed.pid
        ),
    )
    .await;
    notifications::deliver_pending(db, NOW + 30).await.unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={}",
                suppressed.pid
            )
        )
        .await,
        0
    );
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='private' WHERE fid={fid}"),
    )
    .await;
    // The read path looks at the forum copies on thread rows: after changing the forum with raw SQL, sync by the
    // regular rules.
    gono_forum_web::test_support::sync_forum_copies(db, fid)
        .await
        .unwrap();
    assert!(
        notifications::list(db, actor(3), 1, 20)
            .await
            .unwrap()
            .items
            .iter()
            .all(|n| !n.available)
    );
    assert_eq!(notifications::unread_count(db, actor(3)).await.unwrap(), 1);
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='public' WHERE fid={fid}"),
    )
    .await;
    // The read path looks at the forum copies on thread rows: after changing the forum with raw SQL, sync by the
    // regular rules.
    gono_forum_web::test_support::sync_forum_copies(db, fid)
        .await
        .unwrap();

    // A valid lease cannot be taken over; it recovers after expiry; retrying a read notification does not reset it.
    execute(db,&format!("UPDATE gonof_common_outbox SET status='processing',lease_token='crashed',lease_until={} WHERE event_type='notification.content' AND aggregate_id={}",NOW+100,later.pid)).await;
    notifications::deliver_pending(db, NOW + 99).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE lease_token='crashed'"
        )
        .await,
        1
    );
    let before = scalar(db, "SELECT COUNT(*) FROM gonof_home_notification").await;
    notifications::deliver_pending(db, NOW + 100).await.unwrap();
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_home_notification").await,
        before
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT read_at FROM gonof_home_notification WHERE id={child_id}")
        )
        .await,
        NOW + 3
    );

    // Mail/media events stay pending; invalid notification payloads are terminated and only a safe error is recorded.
    for kind in ["email_verification", "media.process", "media.purge"] {
        gono_forum_web::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'post',1,'{}',?,?)")
            .bind(format!("untouched:{kind}")).bind(kind).bind(NOW).bind(NOW).execute(db.executor()).await.unwrap();
    }
    execute(db,&format!("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES('malformed','notification.content','post',1,'{{}}',{NOW},{NOW})")).await;
    notifications::deliver_pending(db, NOW + 101).await.unwrap();
    assert_eq!(scalar(db,"SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key LIKE 'untouched:%' AND status='pending'").await,3);
    assert_eq!(scalar(db,"SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key='malformed' AND status='failed' AND last_error='content notification event is malformed'").await,1);

    atomic_delivery(db, &writer, topic.tid, root.pid, child.pid).await;
    constraints(db, root.pid).await;
    assert_unread_projection(db).await;
}

async fn atomic_delivery(db: &Database, writer: &ContentWriter, tid: i64, root: i64, child: i64) {
    let post = writer
        .reply_at(4, reply(tid, Some(root), Some(child)), NOW + 200)
        .await
        .unwrap();
    let key = format!("notification.content:{}", post.pid);
    gono_forum_web::test_support::fail_when(
        db,
        "reject_notification_ack",
        "BEFORE UPDATE",
        "gonof_common_outbox",
        &format!("NEW.status='sent' AND NEW.event_key='{key}'"),
        "test ack failure",
    )
    .await
    .unwrap();
    let report = notifications::deliver_pending(db, NOW + 200).await.unwrap();
    assert_eq!(report.failed, 1);
    assert_unread_projection(db).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={}",
                post.pid
            )
        )
        .await,
        0,
        "inserting notifications and completing the Outbox event roll back together"
    );
    assert_eq!(scalar(db,&format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key='{key}' AND status='pending'")).await,1);
    {
        gono_forum_web::test_support::drop_trigger(db, "reject_notification_ack")
            .await
            .unwrap();
    }
    notifications::deliver_pending(db, NOW + 220).await.unwrap();
    assert_unread_projection(db).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_notification WHERE pid={}",
                post.pid
            )
        )
        .await,
        2
    );
}

/// The database does not validate notification values: value rules are checked before writing by
/// `notifications::record` (remove that check and the writes below marked "must be rejected" would be stored), and the
/// database keeps only unique keys, foreign keys and `NOT NULL`.
async fn constraints(db: &Database, pid: i64) {
    use gono_forum_web::notifications::record::{Kind, NewNotice, Target};
    let notice =
        |kind: Kind, target: Target, event_key: &'static str, reason: &'static str| NewNotice {
            uid: 1,
            authorid: 2,
            kind,
            target,
            event_key,
            reason,
            dateline: NOW,
        };
    let insert = |notice: NewNotice<'static>| async move {
        gono_forum_web::test_support::insert_notification(db, &notice).await
    };
    let unread = || async {
        scalar(
            db,
            "SELECT unread_notifications FROM gonof_common_member WHERE uid=1",
        )
        .await
    };
    for key in ["CASE-Key", "case-key"] {
        insert(notice(Kind::Reply, Target::Post(pid), key, ""))
            .await
            .unwrap();
    }
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_home_notification WHERE event_key IN ('CASE-Key','case-key')"
        )
        .await,
        2
    );
    let before = unread().await;
    let rows = || async {
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid=1",
        )
        .await
    };
    let rows_before = rows().await;
    for blank in [
        "",
        " ",
        "\t",
        "\n",
        "\r",
        "\u{b}",
        "\u{c}",
        " \t\n\u{b}\u{c}\r ",
    ] {
        assert!(
            insert(notice(Kind::Reply, Target::Post(pid), blank, ""))
                .await
                .is_err(),
            "a blank notification key must be rejected: {blank:?}"
        );
    }
    for (kind, target, reason) in [
        (Kind::Moderation, Target::Post(pid), " "),
        (Kind::ReportResult, Target::Post(pid), "Reason"),
        (Kind::Reply, Target::Sanction(1), ""),
    ] {
        assert!(
            insert(notice(kind, target, "bad-shape", reason))
                .await
                .is_err(),
            "must be rejected: {kind:?} {target:?} reason {reason:?}"
        );
    }
    assert_eq!(
        unread().await,
        before,
        "a rejected write must not change the unread count"
    );
    assert_eq!(
        rows().await,
        rows_before,
        "a rejected write must not be stored"
    );
    // Only all-whitespace is excluded; whitespace inside a valid key is not forbidden, and key values are not
    // normalized on write.
    let meaningful = " \t\n\u{b}\u{c}\r key \t\n\u{b}\u{c}\r ";
    insert(notice(Kind::Reply, Target::Post(pid), meaningful, ""))
        .await
        .unwrap();
    let stored: String = gono_forum_web::db::query_scalar(
        "SELECT event_key FROM gonof_home_notification WHERE uid=1 AND event_key=?",
    )
    .bind(meaningful)
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(stored, meaningful);
    // Protections still at the database layer: unique (uid, event_key), the post foreign key, and a non-null author.
    assert!(
        insert(notice(Kind::Reply, Target::Post(pid), "CASE-Key", ""))
            .await
            .is_err()
    );
    for statement in [
        "UPDATE gonof_home_notification SET pid=99999999 WHERE event_key='CASE-Key'",
        "UPDATE gonof_home_notification SET authorid=NULL WHERE event_key='CASE-Key'",
    ] {
        assert!(
            gono_forum_web::db::query(statement)
                .execute(db.executor())
                .await
                .is_err(),
            "must be rejected: {statement}"
        );
    }
}

#[tokio::test]
async fn mysql_notifications_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    contract(&services.db).await;
    services.cleanup().await.unwrap();
}

// A notification list stops at 100 pages: the count is capped there and later pages are 404, however many
// notifications the member has.
#[tokio::test]
async fn notification_lists_stop_at_the_page_cap() {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    content::seed_fixture(db).await.unwrap();
    for uid in [9701_i64, 9702] {
        gono_forum_web::db::query(
            "INSERT INTO gonof_common_member(uid,username,regdate) VALUES(?,?,1)",
        )
        .bind(uid)
        .bind(format!("capped-{uid}"))
        .execute(db.executor())
        .await
        .unwrap();
    }
    let pid = scalar(db, "SELECT pid FROM gonof_forum_post ORDER BY pid LIMIT 1").await;
    let rows = notifications::MAX_PAGES * notifications::PAGE_SIZE + 15;
    let values = (0..rows)
        .map(|n| format!("(9701,9702,{pid},'reply','capped-{n}',{},1)", 1000 + n))
        .collect::<Vec<_>>();
    execute(
        db,
        &format!(
            "INSERT INTO gonof_home_notification(uid,authorid,pid,type,event_key,dateline,read_at) VALUES {}",
            values.join(",")
        ),
    )
    .await;
    let last = notifications::list(
        db,
        actor(9701),
        notifications::MAX_PAGES,
        notifications::PAGE_SIZE,
    )
    .await
    .unwrap();
    assert_eq!(
        last.total as usize,
        notifications::MAX_PAGES * notifications::PAGE_SIZE
    );
    assert_eq!(last.items.len(), notifications::PAGE_SIZE);
    assert!(matches!(
        notifications::list(
            db,
            actor(9701),
            notifications::MAX_PAGES + 1,
            notifications::PAGE_SIZE
        )
        .await,
        Err(ContentWriteError::NotFound)
    ));
    fixture.cleanup().await.unwrap();
}
