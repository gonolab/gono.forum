use std::sync::Arc;

use gono_forum_web::{
    content::writes::ContentWriteError,
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::Actor,
    notifications::{
        self,
        record::{Kind, NewNotice, Target},
    },
    test_support::{self, TestFixture},
};

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

/// The thread copies and the first post's `first_tid` are supplied by the writer (the forum scope copies and the
/// top-level forum id have no defaults).
async fn seed(db: &Database) {
    execute(db, "INSERT INTO gonof_common_member(uid,username,password,email,emailstatus) VALUES(1,'counter-author','fixture','author@example.test',1),(2,'counter-reader','fixture','reader@example.test',1),(3,'counter-other','fixture','other@example.test',1)").await;
    execute(db, "INSERT INTO gonof_forum_forum(fid,slug,name,description,guidance) VALUES(1,'counter','Counter','','')").await;
    execute(db, "INSERT INTO gonof_forum_thread(tid,fid,authorid,subject,author,family_fid) VALUES(1,1,1,'Counter','counter-author',1)").await;
    execute(db, "INSERT INTO gonof_forum_post_tableid(pid) VALUES(1)").await;
    execute(db, "INSERT INTO gonof_forum_post(pid,fid,tid,authorid,first,first_tid,position,message,dateline) VALUES(1,1,1,1,1,1,1,'Counter',1)").await;
}

async fn assert_projection(db: &Database, expected: [i64; 3]) {
    let rows = db::query("SELECT uid,unread_notifications FROM gonof_common_member ORDER BY uid")
        .fetch_all(db.executor())
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    for (row, value) in rows.iter().zip(expected) {
        let uid: i64 = row.try_get("uid").unwrap();
        let stored: i64 = row.try_get("unread_notifications").unwrap();
        let actual: i64 = db::query_scalar(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid=? AND read_at=0",
        )
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert_eq!(
            (stored, actual),
            (value, value),
            "member {uid}: the unread projection equals the detail rows"
        );
    }
}

/// The unread count is maintained only by `notifications::record`: incremented on insert, decremented by the number
/// of rows actually changed when marking read, and reset on account deletion (account deletion: see
/// `data_requests.rs`). Raw SQL changes to notifications do not sync the count, so only code entry points are used
/// here; removing any one sync makes the projection assertions fail.
#[tokio::test]
async fn unread_projection_is_maintained_by_notification_code() {
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    seed(db).await;
    let revision = scalar(
        db,
        "SELECT revision FROM gonof_common_content_revision WHERE id=1",
    )
    .await;
    let notice = |uid: i64, event_key: &'static str| NewNotice {
        uid,
        authorid: 1,
        kind: Kind::Reply,
        target: Target::Post(1),
        event_key,
        reason: "",
        dateline: 1,
    };
    let id = test_support::insert_notification(db, &notice(2, "counter-event"))
        .await
        .unwrap();
    assert_projection(db, [0, 1, 0]).await;
    // Replaying the same event key: a unique key error rolls back the whole operation, and the count is not inflated.
    assert!(
        test_support::insert_notification(db, &notice(2, "counter-event"))
            .await
            .is_err()
    );
    assert_projection(db, [0, 1, 0]).await;
    // Concurrent repeated mark-read only decrements once: the second one reads it as read after the member row lock
    // and changes 0 rows.
    let (first, second) = tokio::join!(
        notifications::mark_read(db, actor(2), id, 2),
        notifications::mark_read(db, actor(2), id, 3)
    );
    first.unwrap();
    second.unwrap();
    assert_projection(db, [0, 0, 0]).await;
    // Mark-all-read decrements by the number of rows actually changed; clicking again does not decrement again.
    for key in ["other-1", "other-2"] {
        test_support::insert_notification(db, &notice(3, key))
            .await
            .unwrap();
    }
    assert_projection(db, [0, 0, 2]).await;
    for now in [5, 6] {
        notifications::mark_all_read(db, actor(3), i64::MAX, "", now)
            .await
            .unwrap();
    }
    assert_projection(db, [0, 0, 0]).await;
    // Notification and count share one transaction, so a rollback undoes both.
    let mut tx = db.begin_write().await.unwrap();
    test_support::insert_notification_in(db, &mut tx, &notice(2, "rollback-event"))
        .await
        .unwrap();
    let within: i64 =
        db::query_scalar("SELECT unread_notifications FROM gonof_common_member WHERE uid=2")
            .fetch_one(db.observe(&mut *tx))
            .await
            .unwrap();
    assert_eq!(within, 1);
    tx.rollback().await.unwrap();
    assert_projection(db, [0, 0, 0]).await;
    // When the count is too small to decrement, the conditional update refuses and never writes a negative number.
    // Raw SQL creates an unread notification without a count to simulate drift: marking it read fails, the
    // notification stays unread, and the count stays 0.
    execute(db, "INSERT INTO gonof_home_notification(uid,authorid,pid,event_key,dateline) VALUES(2,1,1,'drifted',1)").await;
    let drifted = scalar(
        db,
        "SELECT id FROM gonof_home_notification WHERE event_key='drifted'",
    )
    .await;
    assert!(
        notifications::mark_read(db, actor(2), drifted, 4)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            db,
            "SELECT unread_notifications FROM gonof_common_member WHERE uid=2"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            db,
            "SELECT read_at FROM gonof_home_notification WHERE event_key='drifted'"
        )
        .await,
        0,
        "marking read and the counter roll back in one transaction"
    );
    execute(
        db,
        "DELETE FROM gonof_home_notification WHERE event_key='drifted'",
    )
    .await;
    assert_eq!(
        scalar(
            db,
            "SELECT revision FROM gonof_common_content_revision WHERE id=1"
        )
        .await,
        revision,
        "an unread change must not invalidate the thread display projection"
    );

    let profile = Arc::new(RequestProfile::new());
    assert_eq!(
        trace::scope(profile.clone(), notifications::unread_count(db, actor(2)))
            .await
            .unwrap(),
        0
    );
    let application: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|r| r.source == QuerySource::Application)
        .collect();
    assert_eq!(application.len(), 1);
    assert!(!application[0].sql.contains("COUNT("));
    assert!(application[0].sql.contains("WHERE uid=?"));
    for invalid in [
        actor(99),
        Actor {
            uid: 2,
            credentials_version: 99,
            api_key: None,
        },
    ] {
        assert!(matches!(
            notifications::unread_count(db, invalid).await,
            Err(ContentWriteError::Unauthorized)
        ));
    }
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=2").await;
    assert!(matches!(
        notifications::unread_count(db, actor(2)).await,
        Err(ContentWriteError::Unauthorized)
    ));
    services.cleanup().await.unwrap();
}
