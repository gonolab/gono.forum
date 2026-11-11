use futures_util::future::join_all;
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::Database,
    identity::{self, Actor},
    interaction::{self, Interaction},
};

const NOW: i64 = 1_800_080_000;

async fn execute(db: &Database, statement: &str) {
    gono_forum_web::db::query(statement)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}
/// After raw SQL changes a forum or thread, synchronizes the copies by the official rules (the forum copies on the
/// thread row, and `displayorder` for a hidden first post and reserved states).
async fn resync(db: &Database, table: &str, fid: i64, tid: i64) {
    if table == "gonof_forum_forum" {
        gono_forum_web::test_support::sync_forum_copies(db, fid)
            .await
            .unwrap();
    } else {
        gono_forum_web::test_support::resync_thread(db, tid)
            .await
            .unwrap();
    }
}
async fn scalar(db: &Database, statement: &str) -> i64 {
    gono_forum_web::db::query_scalar(statement)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn ranked(db: &Database, tid: i64) -> Vec<(i64, i64)> {
    let Some(mut thread) = content::thread(db, tid as u64).await.unwrap() else {
        return vec![];
    };
    content::load_replies(
        db,
        &mut thread,
        1,
        true,
        None,
        1,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    interaction::highlights(
        db,
        &thread.posts,
        gono_forum_web::test_support::SEEDED_HIGHLIGHTS,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|item| (item.pid, item.count))
    .collect()
}
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
async fn topic(db: &Database, fid: i64) -> Published {
    writer(db)
        .publish_at(
            1,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Top replies aggregation test".into(),
                body: "The first post can never be a top reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}
async fn reply(db: &Database, tid: i64, root: Option<i64>) -> Published {
    writer(db)
        .reply_at(
            1,
            NewReply {
                tid,
                root_pid: root,
                reply_to_pid: root,
                body: "A reply that can be ranked".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}
async fn react(service: &Interaction, uid: i64, pid: i64, desired: bool) {
    service
        .set_reaction(actor(uid), pid, desired, &identity::random_token())
        .await
        .unwrap();
}
async fn likes(service: &Interaction, pid: i64, count: i64) {
    for uid in 2..=count + 1 {
        react(service, uid, pid, true).await;
    }
}

async fn contract(db: &Database) {
    db.migrate().await.unwrap();
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=15 {
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate) VALUES(?,?,?,'contract-password',1,?)")
            .bind(uid).bind(format!("highlight-{uid}")).bind(format!("highlight-{uid}@example.test")).bind(NOW-172800)
            .execute(db.executor()).await.unwrap();
    }
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    let topic = topic(db, fid).await;
    let service = Interaction::new(db.clone());
    for tid in [topic.tid, 0, -1, 999_999_999] {
        assert!(ranked(db, tid).await.is_empty());
    }
    likes(&service, topic.pid, 10).await;
    assert!(
        ranked(db, topic.tid).await.is_empty(),
        "the first post never ranks among top replies, even with likes"
    );
    let b = reply(db, topic.tid, None).await;
    let a = reply(db, topic.tid, None).await;
    let c = reply(db, topic.tid, None).await;
    let d = reply(db, topic.tid, None).await;
    let child = reply(db, topic.tid, Some(a.pid)).await;
    assert!(
        ranked(db, topic.tid).await.is_empty(),
        "replies without likes stay off the ranking"
    );
    let other = writer(db)
        .publish_at(
            1,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Another thread".into(),
                body: "Isolated thread".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap();
    let other_reply = reply(db, other.tid, None).await;
    likes(&service, other_reply.pid, 12).await;
    assert!(
        ranked(db, topic.tid).await.is_empty(),
        "better-liked replies of another thread must not mix in"
    );
    assert_eq!(ranked(db, other.tid).await, [(other_reply.pid, 12)]);

    for (pid, count) in [
        (a.pid, 3),
        (b.pid, 3),
        (c.pid, 2),
        (d.pid, 1),
        (child.pid, 4),
    ] {
        likes(&service, pid, count).await;
    }
    assert_eq!(
        ranked(db, topic.tid).await,
        [(child.pid, 4), (b.pid, 3), (a.pid, 3)],
        "nested replies on the current page are ranked, and the fourth is not shown"
    );
    let expected = [(child.pid, 4), (b.pid, 3), (a.pid, 3)];
    assert_eq!(
        ranked(db, topic.tid).await,
        expected,
        "ties go by floor order, never by PID order"
    );
    let mut displayed = content::thread(db, topic.tid as u64)
        .await
        .unwrap()
        .unwrap();
    content::load_replies(
        db,
        &mut displayed,
        1,
        true,
        None,
        1,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    for post in &mut displayed.posts {
        if post.pid as i64 == child.pid {
            post.blocked = true;
        }
        if post.pid as i64 == b.pid {
            post.erased = true;
        }
    }
    let visible = interaction::highlights(
        db,
        &displayed.posts,
        gono_forum_web::test_support::SEEDED_HIGHLIGHTS,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.pid, row.count))
    .collect::<Vec<_>>();
    assert_eq!(
        visible,
        [(a.pid, 3), (c.pid, 2), (d.pid, 1)],
        "blocked and erased content is dropped before the three top replies are picked"
    );
    visibility(db, topic, fid, [a, b, c, d, child], &expected).await;
    cancellation_and_concurrency(db, &service, topic.tid, [a, b, c, d, child]).await;
    position_tiebreak(db, &service, fid).await;
}

async fn position_tiebreak(db: &Database, service: &Interaction, fid: i64) {
    let target = topic(db, fid).await;
    let last = scalar(db, "SELECT MAX(pid) FROM gonof_forum_post_tableid").await;
    // Allocate valid immutable identities with PID order intentionally unlike floor order.
    // No later test allocates new posts, so manually allocated IDs cannot affect sequence reuse.
    let first = last + 2;
    let second = last + 1;
    let mut tx = db.begin_write().await.unwrap();
    for (pid, position) in [(first, 2_i64), (second, 3_i64)] {
        gono_forum_web::db::query("INSERT INTO gonof_forum_post_tableid(pid) VALUES(?)")
            .bind(pid)
            .execute(db.observe(&mut *tx))
            .await
            .unwrap();
        gono_forum_web::db::query("INSERT INTO gonof_forum_post(pid,fid,tid,authorid,first,position,root_position,message,dateline) VALUES(?,?,?,1,0,?,?,'Tie ordering fixture',?)")
            .bind(pid).bind(fid).bind(target.tid).bind(position).bind(position-1).bind(NOW).execute(db.observe(&mut *tx)).await.unwrap();
    }
    gono_forum_web::db::query(
        "UPDATE gonof_forum_thread SET replies=2,root_replies=2,maxposition=3 WHERE tid=?",
    )
    .bind(target.tid)
    .execute(db.observe(&mut *tx))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    likes(service, first, 1).await;
    likes(service, second, 1).await;
    assert_eq!(
        ranked(db, target.tid).await,
        [(first, 1), (second, 1)],
        "equal likes go by floor, a smaller PID must not win"
    );
}

async fn visibility(
    db: &Database,
    topic: Published,
    fid: i64,
    posts: [Published; 5],
    expected: &[(i64, i64)],
) {
    let [a, b, c, d, child] = posts;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            a.pid
        ),
    )
    .await;
    assert_eq!(
        ranked(db, topic.tid).await,
        [(b.pid, 3), (c.pid, 2), (d.pid, 1)],
        "a parent hidden by moderation blocks its nested replies and the next candidates move up"
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            a.pid
        ),
    )
    .await;
    assert_eq!(ranked(db, topic.tid).await, expected);
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at={NOW} WHERE pid={}",
            a.pid
        ),
    )
    .await;
    assert_eq!(
        ranked(db, topic.tid).await,
        [(child.pid, 4), (b.pid, 3), (c.pid, 2)],
        "a withdrawn parent leaves only itself out, its public nested replies still rank"
    );
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={}",
            a.pid
        ),
    )
    .await;
    for (assignment, restore) in [
        ("invisible=1", "invisible=0"),
        ("invisible=-1", "invisible=0"),
        ("deleted_at=1", "deleted_at=0"),
    ] {
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_post SET {assignment} WHERE pid={}",
                child.pid
            ),
        )
        .await;
        assert_eq!(
            ranked(db, topic.tid).await,
            [(b.pid, 3), (a.pid, 3), (c.pid, 2)],
            "a nested reply that is not visible now leaves the ranking: {assignment}"
        );
        execute(
            db,
            &format!(
                "UPDATE gonof_forum_post SET {restore} WHERE pid={}",
                child.pid
            ),
        )
        .await;
    }
    for (table, column, id, assignment, restore) in [
        ("gonof_forum_forum", "fid", fid, "status=0", "status=1"),
        (
            "gonof_forum_forum",
            "fid",
            fid,
            "visibility='private'",
            "visibility='public'",
        ),
        (
            "gonof_forum_forum",
            "fid",
            fid,
            "visibility='members'",
            "visibility='public'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "readperm=1",
            "readperm=0",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "displayorder=-3",
            "displayorder=0",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "visibility='private'",
            "visibility='public'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "visibility='members'",
            "visibility='public'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "publication_state='draft'",
            "publication_state='published'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            topic.tid,
            "publication_state='scheduled'",
            "publication_state='published'",
        ),
        (
            "gonof_forum_post",
            "pid",
            topic.pid,
            "invisible=-1",
            "invisible=0",
        ),
    ] {
        execute(
            db,
            &format!("UPDATE {table} SET {assignment} WHERE {column}={id}"),
        )
        .await;
        resync(db, table, fid, topic.tid).await;
        assert!(
            ranked(db, topic.tid).await.is_empty(),
            "no top reply may remain after access is tightened: {table} {assignment}"
        );
        execute(
            db,
            &format!("UPDATE {table} SET {restore} WHERE {column}={id}"),
        )
        .await;
        resync(db, table, fid, topic.tid).await;
        assert_eq!(ranked(db, topic.tid).await, expected);
    }
    // Existing semantics keep public replies readable when the first post is withdrawn or closed.
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET closed=1,status=1,deleted_at={NOW} WHERE tid={}",
            topic.tid
        ),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at={NOW} WHERE pid={}",
            topic.pid
        ),
    )
    .await;
    assert_eq!(ranked(db, topic.tid).await, expected);
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET closed=0,status=0,deleted_at=0 WHERE tid={}",
            topic.tid
        ),
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={}",
            topic.pid
        ),
    )
    .await;
}

async fn cancellation_and_concurrency(
    db: &Database,
    service: &Interaction,
    tid: i64,
    posts: [Published; 5],
) {
    let [a, b, c, d, child] = posts;
    react(service, 5, child.pid, false).await;
    assert_eq!(
        ranked(db, tid).await,
        [(b.pid, 3), (a.pid, 3), (child.pid, 3)],
        "after one like is removed the ranking follows the new tie order at once"
    );
    for uid in 2..=4 {
        react(service, uid, child.pid, false).await;
    }
    assert_eq!(
        ranked(db, tid).await,
        [(b.pid, 3), (a.pid, 3), (c.pid, 2)],
        "after the last like is removed the reply leaves and the former fourth moves up"
    );
    join_all((2..=9).map(|uid| react(service, uid, c.pid, true))).await;
    assert_eq!(
        ranked(db, tid).await,
        [(c.pid, 8), (b.pid, 3), (a.pid, 3)],
        "concurrent likes from different members count each unique pair exactly"
    );
    join_all((0..8).map(|_| react(service, 2, d.pid, true))).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={}",
                d.pid
            )
        )
        .await,
        1,
        "concurrent repeated likes from one member must not inflate the count"
    );
    assert!(
        gono_forum_web::db::query(
            "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(?,2,?)"
        )
        .bind(d.pid)
        .bind(NOW)
        .execute(db.executor())
        .await
        .is_err(),
        "the database still keeps one like per member and floor"
    );
    for uid in 2..=9 {
        react(service, uid, c.pid, false).await;
    }
    assert_eq!(ranked(db, tid).await, [(b.pid, 3), (a.pid, 3), (d.pid, 1)]);
    let add_key = identity::random_token();
    let remove_key = identity::random_token();
    let (added, removed) = tokio::join!(
        service.set_reaction(actor(10), c.pid, true, &add_key),
        service.set_reaction(actor(10), c.pid, false, &remove_key),
    );
    added.unwrap();
    removed.unwrap();
    let actual = scalar(
        db,
        &format!(
            "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={}",
            c.pid
        ),
    )
    .await;
    assert!((0..=1).contains(&actual));
    let expected_third = if actual == 1 { (c.pid, 1) } else { (d.pid, 1) };
    assert_eq!(
        ranked(db, tid).await,
        [(b.pid, 3), (a.pid, 3), expected_third],
        "after concurrent likes and unlikes the ranking matches the final set of likes"
    );
}

#[tokio::test]
async fn mysql_highlights_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    contract(&services.db).await;
    services.cleanup().await.unwrap();
}
