use std::time::Duration;

use futures_util::future::join_all;
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::Database,
    identity::{self, Actor},
    interaction::{self, Interaction},
};

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
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    gono_forum_web::db::query_scalar(sql)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}
async fn reject(db: &Database, sql: &str) {
    assert!(
        gono_forum_web::db::query(sql)
            .execute(&db.pool)
            .await
            .is_err(),
        "the database must reject it: {sql}"
    );
}

// Running the baseline again keeps the business data and the migration checksums.
async fn upgrade(db: &Database) {
    db.migrate().await.unwrap();
    content::seed_forums(db).await.unwrap();
    content::seed_fixture(db).await.unwrap();
    let before = scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await;
    let text: String =
        gono_forum_web::db::query_scalar("SELECT subject FROM gonof_forum_thread WHERE tid=103")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let checksums: Vec<Vec<u8>> =
        gono_forum_web::db::query_scalar("SELECT checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        gono_forum_web::db::migrations::LATEST
    );
    let after_checksums: Vec<Vec<u8>> =
        gono_forum_web::db::query_scalar("SELECT checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(checksums, after_checksums);
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_forum_post").await,
        before
    );
    assert_eq!(
        gono_forum_web::db::query_scalar::<String>(
            "SELECT subject FROM gonof_forum_thread WHERE tid=103"
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        text
    );
    for table in [
        "gonof_home_favorite",
        "gonof_forum_post_reaction",
        "gonof_home_notification",
    ] {
        assert_eq!(
            scalar(db, &format!("SELECT COUNT(*) FROM {table}")).await,
            0
        );
    }
}

async fn topic(db: &Database, title: &str) -> Published {
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    ContentWriter::new(db.clone(), WriteConfig::default())
        .publish(
            105,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: title.into(),
                body: format!("Body of {title}"),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
}
async fn reply(db: &Database, topic: Published, root: Option<i64>) -> Published {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .reply(
            105,
            NewReply {
                tid: topic.tid,
                root_pid: root,
                reply_to_pid: root,
                body: "Reply that gets interactions".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
}
async fn state(db: &Database, uid: i64, pid: i64) -> (i64, bool) {
    let values = interaction::reactions(db, Some(uid), &[pid]).await.unwrap();
    values
        .get(&pid)
        .map(|value| (value.count, value.active))
        .unwrap_or((0, false))
}

async fn contract(db: &Database) {
    upgrade(db).await;
    for uid in 101..=125 {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(?,?,?,'test-password',?,?,?)")
            .bind(uid).bind(format!("interaction-{uid}")).bind(format!("interaction-{uid}@example.test"))
            .bind(i64::from(uid != 104 && uid != 105)).bind(if uid == 105 { 1i64 } else { 0i64 })
            .bind(identity::now() - 200_000).execute(&db.pool).await.unwrap();
    }
    let item = topic(db, "Permission-sensitive bookmark title").await;
    let root = reply(db, item, None).await;
    let child = reply(db, item, Some(root.pid)).await;
    let service = Interaction::new(db.clone());
    favorites_contract(db, &service, item).await;
    reactions_contract(db, &service, item, root, child).await;
    permissions_contract(db, &service, item).await;
    failed_request_rolls_back(db, &service, item).await;
    constraints_contract(db, item).await;
    // Every request record must store the request time; rows that miss it have dateline 0 and would be deleted by the
    // first cleanup.
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE dateline<=0"
        )
        .await,
        0,
        "the request row records the request time"
    );
    assert!(scalar(db, "SELECT COUNT(*) FROM gonof_forum_post_request").await > 0);
}

async fn favorites_contract(db: &Database, service: &Interaction, item: Published) {
    // Request keys follow the same rule as follows and blocks: only 64 lowercase hex characters.
    let upper = identity::random_token().to_ascii_uppercase();
    assert!(matches!(
        service
            .set_favorite(actor(101), item.tid, true, &upper)
            .await,
        Err(ContentWriteError::Validation(_))
    ));
    assert!(matches!(
        service
            .set_reaction(actor(101), item.pid, true, &upper)
            .await,
        Err(ContentWriteError::Validation(_))
    ));
    let first = identity::random_token();
    service
        .set_favorite(actor(101), item.tid, true, &first)
        .await
        .unwrap();
    service
        .set_favorite(actor(101), item.tid, true, &first)
        .await
        .unwrap();
    assert!(
        interaction::favorite_state(db, 101, item.tid)
            .await
            .unwrap()
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=101 AND tid={}",
                item.tid
            )
        )
        .await,
        1
    );
    assert!(matches!(
        service
            .set_favorite(actor(101), item.tid, false, &first)
            .await,
        Err(ContentWriteError::Conflict(_))
    ));
    let cancel = identity::random_token();
    service
        .set_favorite(actor(101), item.tid, false, &cancel)
        .await
        .unwrap();
    service
        .set_favorite(actor(101), item.tid, true, &first)
        .await
        .unwrap();
    assert!(
        !interaction::favorite_state(db, 101, item.tid)
            .await
            .unwrap(),
        "an old bookmark request must not override a later removal"
    );
    let second = identity::random_token();
    service
        .set_favorite(actor(101), item.tid, true, &second)
        .await
        .unwrap();
    service
        .set_favorite(actor(101), item.tid, false, &cancel)
        .await
        .unwrap();
    assert!(
        interaction::favorite_state(db, 101, item.tid)
            .await
            .unwrap(),
        "an old removal request must not override a later bookmark"
    );
    assert!(matches!(
        service
            .set_reaction(actor(101), item.pid, true, &second)
            .await,
        Err(ContentWriteError::Conflict(_))
    ));
    let all = interaction::favorites(db, 101, 1, &Default::default())
        .await
        .unwrap();
    assert_eq!(all.total, 1);
    assert_eq!(all.rows[0].tid, item.tid);
    assert_eq!(all.rows[0].title, "Permission-sensitive bookmark title");
    assert!(all.rows[0].available);
    assert_eq!(
        interaction::favorites(db, 102, 1, &Default::default())
            .await
            .unwrap()
            .total,
        0
    );
    assert!(
        interaction::favorites(db, 101, 0, &Default::default())
            .await
            .is_err()
    );
    assert!(
        interaction::favorites(db, 101, 2, &Default::default())
            .await
            .is_err()
    );

    for change in [
        format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            item.pid
        ),
        format!(
            "UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid={}",
            item.tid
        ),
    ] {
        execute(db, &change).await;
        // Hiding the first post rewrites the thread row's displayorder; after raw SQL, sync by the regular rules.
        gono_forum_web::test_support::resync_thread(db, item.tid)
            .await
            .unwrap();
        let hidden = interaction::favorites(db, 101, 1, &Default::default())
            .await
            .unwrap();
        assert_eq!(hidden.total, 1);
        assert!(!hidden.rows[0].available);
        assert!(!hidden.rows[0].title.contains("sensitive"));
        assert!(
            service
                .set_favorite(actor(102), item.tid, true, &identity::random_token())
                .await
                .is_err()
        );
        service
            .set_favorite(actor(101), item.tid, false, &identity::random_token())
            .await
            .unwrap();
        assert_eq!(
            interaction::favorites(db, 101, 1, &Default::default())
                .await
                .unwrap()
                .total,
            0
        );
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
                item.pid
            ),
        )
        .await;
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_thread SET deleted_at=0 WHERE tid={}",
                item.tid
            ),
        )
        .await;
        gono_forum_web::test_support::resync_thread(db, item.tid)
            .await
            .unwrap();
        service
            .set_favorite(actor(101), item.tid, true, &identity::random_token())
            .await
            .unwrap();
        assert!(
            interaction::favorites(db, 101, 1, &Default::default())
                .await
                .unwrap()
                .rows[0]
                .available
        );
    }
    let key = identity::random_token();
    let calls = (0..8).map(|_| service.set_favorite(actor(102), item.tid, true, &key));
    for result in tokio::time::timeout(Duration::from_secs(15), join_all(calls))
        .await
        .expect("concurrent bookmarks must not deadlock")
    {
        result.unwrap();
    }
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=102 AND tid={}",
                item.tid
            )
        )
        .await,
        1
    );

    // Sorting with a fixed identical second must end with tid, so pages neither repeat nor miss items.
    for n in 0..21 {
        let entry = topic(db, &format!("Paged bookmark {n}")).await;
        service
            .set_favorite(actor(103), entry.tid, true, &identity::random_token())
            .await
            .unwrap();
    }
    execute(
        db,
        "UPDATE gonof_home_favorite SET dateline=100 WHERE uid=103",
    )
    .await;
    let first = interaction::favorites(db, 103, 1, &Default::default())
        .await
        .unwrap();
    let last = interaction::favorites(db, 103, 2, &Default::default())
        .await
        .unwrap();
    assert_eq!(first.total, 21);
    assert_eq!(first.rows.len(), 20);
    assert_eq!(last.rows.len(), 1);
    let ids: Vec<_> = first
        .rows
        .iter()
        .chain(last.rows.iter())
        .map(|row| row.tid)
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] > pair[1]));
    assert!(
        interaction::favorites(db, 103, 3, &Default::default())
            .await
            .is_err()
    );
}

async fn reactions_contract(
    db: &Database,
    service: &Interaction,
    item: Published,
    root: Published,
    child: Published,
) {
    for pid in [item.pid, root.pid, child.pid] {
        assert!(matches!(
            service
                .set_reaction(actor(105), pid, true, &identity::random_token())
                .await,
            Err(ContentWriteError::Forbidden(_))
        ));
        service
            .set_reaction(actor(101), pid, true, &identity::random_token())
            .await
            .unwrap();
        assert_eq!(state(db, 101, pid).await, (1, true));
    }
    let add = identity::random_token();
    service
        .set_reaction(actor(102), root.pid, true, &add)
        .await
        .unwrap();
    service
        .set_reaction(actor(102), root.pid, true, &add)
        .await
        .unwrap();
    assert_eq!(state(db, 102, root.pid).await, (2, true));
    assert!(matches!(
        service
            .set_reaction(actor(102), child.pid, true, &add)
            .await,
        Err(ContentWriteError::Conflict(_))
    ));
    let cancel = identity::random_token();
    service
        .set_reaction(actor(102), root.pid, false, &cancel)
        .await
        .unwrap();
    service
        .set_reaction(actor(102), root.pid, true, &add)
        .await
        .unwrap();
    assert_eq!(state(db, 102, root.pid).await, (1, false));
    service
        .set_reaction(actor(102), root.pid, true, &identity::random_token())
        .await
        .unwrap();
    service
        .set_reaction(actor(102), root.pid, false, &cancel)
        .await
        .unwrap();
    assert_eq!(state(db, 102, root.pid).await, (2, true));

    let key = identity::random_token();
    for result in tokio::time::timeout(
        Duration::from_secs(15),
        join_all((0..8).map(|_| service.set_reaction(actor(103), child.pid, true, &key))),
    )
    .await
    .expect("concurrent likes with the same key must not deadlock")
    {
        result.unwrap();
    }
    assert_eq!(state(db, 103, child.pid).await, (2, true));
    let keys: Vec<_> = (110..=125).map(|_| identity::random_token()).collect();
    for result in tokio::time::timeout(
        Duration::from_secs(20),
        join_all(
            (110..=125)
                .zip(keys.iter())
                .map(|(uid, key)| service.set_reaction(actor(uid), child.pid, true, key)),
        ),
    )
    .await
    .expect("concurrent likes from different accounts must not deadlock")
    {
        result.unwrap();
    }
    assert_eq!(state(db, 110, child.pid).await, (18, true));
    let cancel_keys: Vec<_> = (110..=125).map(|_| identity::random_token()).collect();
    for result in join_all(
        (110..=125)
            .zip(cancel_keys.iter())
            .map(|(uid, key)| service.set_reaction(actor(uid), child.pid, false, key)),
    )
    .await
    {
        result.unwrap();
    }
    assert_eq!(state(db, 110, child.pid).await, (2, false));

    // Withdrawing a parent post does not mask child replies; only moderation-hiding the parent post stops new likes on
    // child replies.
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at=1 WHERE pid={}",
            root.pid
        ),
    )
    .await;
    service
        .set_reaction(actor(106), child.pid, true, &identity::random_token())
        .await
        .unwrap();
    assert!(
        service
            .set_reaction(actor(106), root.pid, true, &identity::random_token())
            .await
            .is_err()
    );
    service
        .set_reaction(actor(101), root.pid, false, &identity::random_token())
        .await
        .unwrap();
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            root.pid
        ),
    )
    .await;
    assert!(
        service
            .set_reaction(actor(107), child.pid, true, &identity::random_token())
            .await
            .is_err()
    );
    service
        .set_reaction(actor(103), child.pid, false, &identity::random_token())
        .await
        .unwrap();
    assert!(
        !interaction::target(db, child.pid)
            .await
            .unwrap()
            .unwrap()
            .visible,
        "a hidden parent reply must block public reads of its nested replies"
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0,deleted_at=0 WHERE pid={}",
            root.pid
        ),
    )
    .await;
    assert_eq!(state(db, 106, child.pid).await, (2, true));

    // Different commands from the same account must also be serialized, and the final result is decided by the target
    // state committed last.
    let mixed_keys: Vec<_> = (0..16).map(|_| identity::random_token()).collect();
    for result in
        tokio::time::timeout(
            Duration::from_secs(20),
            join_all(mixed_keys.iter().enumerate().map(|(index, key)| {
                service.set_reaction(actor(109), root.pid, index % 2 == 0, key)
            })),
        )
        .await
        .expect("concurrent likes and unlikes with different keys must not deadlock")
    {
        result.unwrap();
    }
    let actual = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={}",
            root.pid
        ),
    )
    .await;
    let own = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={} AND uid=109",
            root.pid
        ),
    )
    .await;
    assert!((0..=1).contains(&own));
    assert_eq!(state(db, 109, root.pid).await, (actual, own == 1));
    let favorite_keys: Vec<_> = (0..16).map(|_| identity::random_token()).collect();
    for result in
        tokio::time::timeout(
            Duration::from_secs(20),
            join_all(favorite_keys.iter().enumerate().map(|(index, key)| {
                service.set_favorite(actor(109), item.tid, index % 2 == 0, key)
            })),
        )
        .await
        .expect("concurrent bookmarks and removals with different keys must not deadlock")
    {
        result.unwrap();
    }
    let actual = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=109 AND tid={}",
            item.tid
        ),
    )
    .await;
    assert!((0..=1).contains(&actual));
    assert_eq!(
        interaction::favorite_state(db, 109, item.tid)
            .await
            .unwrap(),
        actual == 1
    );
}

async fn failed_request_rolls_back(db: &Database, service: &Interaction, item: Published) {
    let key = "f".repeat(64);
    gono_forum_web::test_support::fail_when(
        db,
        "gonof_test_interaction_request",
        "BEFORE INSERT",
        "gonof_forum_post_request",
        &format!("NEW.request_key='{key}'"),
        "injected request failure",
    )
    .await
    .unwrap();
    let before = scalar(
        db,
        "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid=108",
    )
    .await;
    assert!(
        service
            .set_favorite(actor(108), item.tid, true, &key)
            .await
            .is_err()
    );
    assert!(
        !interaction::favorite_state(db, 108, item.tid)
            .await
            .unwrap(),
        "the bookmark rolls back when the request row fails"
    );
    let original_count = state(db, 108, item.pid).await.0;
    assert!(
        service
            .set_reaction(actor(108), item.pid, true, &key)
            .await
            .is_err()
    );
    assert_eq!(state(db, 108, item.pid).await, (original_count, false));
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid=108"
        )
        .await,
        before
    );
    service
        .set_favorite(actor(108), item.tid, true, &identity::random_token())
        .await
        .unwrap();
    service
        .set_reaction(actor(108), item.pid, true, &identity::random_token())
        .await
        .unwrap();
    assert!(
        service
            .set_favorite(actor(108), item.tid, false, &key)
            .await
            .is_err()
    );
    assert!(
        interaction::favorite_state(db, 108, item.tid)
            .await
            .unwrap(),
        "the removal rolls back when the request row fails"
    );
    assert!(
        service
            .set_reaction(actor(108), item.pid, false, &key)
            .await
            .is_err()
    );
    assert_eq!(state(db, 108, item.pid).await, (original_count + 1, true));
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid=108"
        )
        .await,
        before + 2
    );
    {
        gono_forum_web::test_support::drop_trigger(db, "gonof_test_interaction_request")
            .await
            .unwrap();
    }
    service
        .set_favorite(actor(108), item.tid, false, &key)
        .await
        .unwrap();
    assert!(
        !interaction::favorite_state(db, 108, item.tid)
            .await
            .unwrap()
    );
}

async fn permissions_contract(db: &Database, service: &Interaction, item: Published) {
    for uid in [104, 999999] {
        assert!(
            service
                .set_favorite(actor(uid), item.tid, true, &identity::random_token())
                .await
                .is_err()
        );
        assert!(
            service
                .set_reaction(actor(uid), item.pid, true, &identity::random_token())
                .await
                .is_err()
        );
    }
    let old = identity::random_token();
    service
        .set_favorite(actor(101), item.tid, true, &old)
        .await
        .unwrap();
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=101",
    )
    .await;
    assert!(
        service
            .set_favorite(actor(101), item.tid, true, &old)
            .await
            .is_err(),
        "an old request does not bypass the current freeze"
    );
    assert!(
        service
            .set_reaction(actor(101), item.pid, false, &identity::random_token())
            .await
            .is_err()
    );
    execute(
        db,
        "UPDATE gonof_common_member SET `freeze`=0,credentials_version=2 WHERE uid=101",
    )
    .await;
    assert!(
        service
            .set_favorite(actor(101), item.tid, false, &identity::random_token())
            .await
            .is_err()
    );
    execute(
        db,
        "UPDATE gonof_common_member SET credentials_version=1,emailstatus=0 WHERE uid=101",
    )
    .await;
    assert!(
        service
            .set_favorite(actor(101), item.tid, false, &identity::random_token())
            .await
            .is_err()
    );
    assert_eq!(
        interaction::favorites(db, 101, 1, &Default::default())
            .await
            .unwrap()
            .total,
        1,
        "an unverified account can read its own collections"
    );
    execute(
        db,
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid=101",
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET closed=1 WHERE tid={}",
            item.tid
        ),
    )
    .await;
    service
        .set_favorite(actor(105), item.tid, true, &identity::random_token())
        .await
        .unwrap();
    service
        .set_reaction(actor(107), item.pid, true, &identity::random_token())
        .await
        .unwrap();
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET visibility='members' WHERE tid={}",
            item.tid
        ),
    )
    .await;
    service
        .set_favorite(actor(108), item.tid, true, &identity::random_token())
        .await
        .unwrap();
    service
        .set_reaction(actor(108), item.pid, true, &identity::random_token())
        .await
        .unwrap();
    assert!(
        interaction::favorites(db, 101, 1, &Default::default())
            .await
            .unwrap()
            .rows[0]
            .available
    );
    service
        .set_favorite(actor(108), item.tid, false, &identity::random_token())
        .await
        .unwrap();
    service
        .set_reaction(actor(108), item.pid, false, &identity::random_token())
        .await
        .unwrap();
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET visibility='public' WHERE tid={}",
            item.tid
        ),
    )
    .await;
    for restriction in [
        "visibility='private'",
        "readperm=1",
        "displayorder=-3",
        "publication_state='draft'",
    ] {
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_thread SET {restriction} WHERE tid={}",
                item.tid
            ),
        )
        .await;
        // Read paths judge by the sign of displayorder: after raw SQL changed the reserved state, recompute by the
        // regular rules.
        gono_forum_web::test_support::refresh_listing(db)
            .await
            .unwrap();
        assert!(
            service
                .set_favorite(actor(108), item.tid, true, &identity::random_token())
                .await
                .is_err()
        );
        assert!(
            service
                .set_reaction(actor(108), item.pid, true, &identity::random_token())
                .await
                .is_err()
        );
        assert!(
            !interaction::favorites(db, 101, 1, &Default::default())
                .await
                .unwrap()
                .rows[0]
                .available
        );
        execute(db, &format!("UPDATE gonof_forum_thread SET visibility='public',readperm=0,displayorder=0,publication_state='published' WHERE tid={}", item.tid)).await;
        gono_forum_web::test_support::refresh_listing(db)
            .await
            .unwrap();
    }
    assert!(
        service
            .set_favorite(actor(101), item.tid, true, "bad-key")
            .await
            .is_err()
    );
    assert!(
        service
            .set_reaction(actor(101), -1, true, &identity::random_token())
            .await
            .is_err()
    );
}

async fn constraints_contract(db: &Database, item: Published) {
    reject(
        db,
        &format!(
            "INSERT INTO gonof_home_favorite(uid,tid,dateline) VALUES(101,{},0)",
            item.tid
        ),
    )
    .await;
    reject(
        db,
        &format!(
            "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES({},101,0)",
            item.pid
        ),
    )
    .await;
    for sql in [
        format!(
            "INSERT INTO gonof_home_favorite(uid,tid,dateline) VALUES(999999,{},0)",
            item.tid
        ),
        "INSERT INTO gonof_home_favorite(uid,tid,dateline) VALUES(110,999999,0)".into(),
        format!(
            "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES({},999999,0)",
            item.pid
        ),
        "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(999999,110,0)".into(),
    ] {
        reject(db, &sql).await;
    }
    // Negative times are not rejected by the database: the entry points only write `identity::now()`, and anything
    // written bypassing them is detected by the messaging check script (`tests/consistency/messaging.sql`).
    for sql in [
        format!(
            "INSERT INTO gonof_home_favorite(uid,tid,dateline) VALUES(110,{},-1)",
            item.tid
        ),
        format!(
            "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES({},110,-1)",
            item.pid
        ),
    ] {
        gono_forum_web::test_support::assert_messaging_rejected_or_flagged(
            db,
            &sql,
            "attachment references, likes or favorites with a negative time",
        )
        .await;
    }
    let mut tx = db.begin_write().await.unwrap();
    gono_forum_web::db::query("INSERT INTO gonof_home_favorite(uid,tid,dateline) VALUES(110,?,0)")
        .bind(item.tid)
        .execute(db.observe(&mut *tx))
        .await
        .unwrap();
    // The later write in the same transaction violates a foreign key (the post does not exist).
    assert!(
        gono_forum_web::db::query(
            "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(999999,110,0)"
        )
        .execute(db.observe(&mut *tx))
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(
        !interaction::favorite_state(db, 110, item.tid)
            .await
            .unwrap(),
        "a constraint failure rolls back rows added earlier in the same transaction"
    );
}

#[tokio::test]
async fn mysql_interactions_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    contract(&services.db).await;
    services.cleanup().await.unwrap();
}

/// Favoriting and posting only take a shared lock on the forum row, so they no longer queue behind
/// each other; the exclusive lock forum management needs still holds them back, and the protected invariant (the
/// forum's status and visibility do not change before commit) is unchanged.
#[tokio::test]
async fn mysql_interactions_and_posts_share_the_forum_lock() {
    // Only verifies MySQL's shared lock (interactions and posts in the same forum do not wait for each other): SQLite's
    // write transactions queue as a whole.
    gono_forum_web::mysql_only!();
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    use std::sync::Arc;
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    let db = &services.db;
    upgrade(db).await;
    for uid in [105_i64, 106] {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(?,?,?,'test-password',1,?,?)")
            .bind(uid).bind(format!("forum-lock-{uid}")).bind(format!("forum-lock-{uid}@example.test"))
            .bind(i64::from(uid == 105)).bind(identity::now() - 200_000).execute(&db.pool).await.unwrap();
    }
    let item = topic(db, "Forum shared lock").await;
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    let service = Interaction::new(db.clone());

    // Another transaction holds a shared lock on the forum row (for example another favorite or post not yet
    // committed).
    let mut other = db.begin_write().await.unwrap();
    gono_forum_web::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? LOCK IN SHARE MODE")
        .bind(fid)
        .fetch_one(&mut *other)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        service.set_favorite(actor(106), item.tid, true, &identity::random_token()),
    )
    .await
    .expect("a bookmark must not wait for other interactions in the same forum")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), reply(db, item, None))
        .await
        .expect("posting must not wait for other interactions in the same forum");
    other.commit().await.unwrap();

    // While forum management holds the exclusive lock, favoriting must wait for it to commit.
    let mut admin = db.begin_write().await.unwrap();
    gono_forum_web::db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE")
        .bind(fid)
        .fetch_one(&mut *admin)
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let favoriting = {
        let service = service.clone();
        let profile = profile.clone();
        tokio::spawn(async move {
            trace::scope(
                profile,
                Box::pin(service.set_favorite(
                    actor(106),
                    item.tid,
                    false,
                    &identity::random_token(),
                )),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !profile.snapshot().iter().any(|record| {
            record.source == QuerySource::Application
                && record
                    .sql
                    .starts_with("SELECT fid FROM gonof_forum_forum WHERE fid=?")
                && record.sql.ends_with("LOCK IN SHARE MODE")
                && record.elapsed.is_zero()
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the bookmark must wait on the shared lock of the forum row");
    assert!(!favoriting.is_finished());
    admin.commit().await.unwrap();
    favoriting.await.unwrap().unwrap();
    assert!(
        !interaction::favorite_state(db, 106, item.tid)
            .await
            .unwrap()
    );
    services.cleanup().await.unwrap();
}
