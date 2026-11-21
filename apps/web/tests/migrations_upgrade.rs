//! Migrations that backfill data, run against a database built at the version before them (docs/queries.md, "Copy
//! columns"): the backfilled values must equal what the write paths compute, and the consistency checks must pass.
use gono_forum_web::{
    content,
    db::{self, Database},
    test_support::{self, TestFixture},
};

async fn execute(db: &Database, sql: &str) {
    db::query(sql)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn last_reply_uids(db: &Database) -> Vec<(i64, Option<i64>)> {
    db::query("SELECT tid,last_reply_uid FROM gonof_forum_thread ORDER BY tid")
        .fetch_all(db.executor())
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get("tid").unwrap(),
                row.try_get("last_reply_uid").unwrap(),
            )
        })
        .collect()
}

/// Migration 4 adds `gonof_forum_thread.last_reply_uid` and fills it from the author of `last_reply_pid`, leaving
/// NULL for threads without a live reply and for replies by deleted accounts or without an author.
#[tokio::test]
async fn the_last_replier_uid_is_backfilled_from_the_last_reply() {
    let services = TestFixture::new().await.unwrap();
    let db = services.db.clone();
    db.migrate_until(3).await.unwrap();
    // One board with the columns of version 3 (`content::seed_forums` writes the later color and glyph columns).
    execute(
        &db,
        "INSERT INTO gonof_forum_forum (slug, name, description, guidance, displayorder) VALUES ('daily', 'Daily', '', '', 0)",
    )
    .await;
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(db.executor())
        .await
        .unwrap();
    // Alice and Carol are active; Bob's account was deleted after he replied.
    execute(
        &db,
        "INSERT INTO gonof_common_member (uid, username, account_state) VALUES (1,'alice','active'),(2,'bob','deleted'),(3,'carol','active')",
    )
    .await;
    // Thread 1 ends with Alice's reply, thread 2 with Bob's, thread 3 has no reply, thread 4 ends with a reply that has
    // no author.
    let threads = [
        (
            1,
            "'alice'",
            &[(11, 1, "1"), (12, 2, "3"), (13, 3, "1")][..],
        ),
        (2, "'carol'", &[(21, 1, "3"), (22, 2, "2")][..]),
        (3, "'alice'", &[(31, 1, "1")][..]),
        (4, "'carol'", &[(41, 1, "3"), (42, 2, "NULL")][..]),
    ];
    for (tid, author, posts) in threads {
        let authorid = posts[0].2;
        execute(
            &db,
            &format!(
                "INSERT INTO gonof_forum_thread (tid, fid, family_fid, authorid, subject, author, dateline, lastpost, replies, root_replies, valid_replies, maxposition)
                 VALUES ({tid}, {fid}, {fid}, {authorid}, 'Thread {tid}', {author}, 1000, 1000, {replies}, {replies}, {replies}, {positions})",
                replies = posts.len() - 1,
                positions = posts.len(),
            ),
        )
        .await;
        for (pid, position, uid) in posts {
            let first = *position == 1;
            let (first_tid, root_position) = if first {
                (tid.to_string(), "NULL".to_owned())
            } else {
                ("NULL".to_owned(), (position - 1).to_string())
            };
            execute(
                &db,
                &format!("INSERT INTO gonof_forum_post_tableid (pid) VALUES ({pid})"),
            )
            .await;
            execute(
                &db,
                &format!(
                    "INSERT INTO gonof_forum_post (pid, fid, tid, authorid, first, first_tid, position, message, dateline, root_position)
                     VALUES ({pid}, {fid}, {tid}, {uid}, {first}, {first_tid}, {position}, 'Post {pid}', {date}, {root_position})",
                    first = i64::from(first),
                    date = 1000 + position,
                ),
            )
            .await;
        }
        if let Some((pid, position, uid)) = posts.iter().skip(1).last() {
            let name = match *uid {
                "1" => "alice",
                "3" => "carol",
                // A deleted account or no author: the stored marker for "Deleted user".
                _ => content::copies::DELETED_AUTHOR,
            };
            execute(
                &db,
                &format!(
                    "UPDATE gonof_forum_thread SET last_reply_pid={pid}, last_reply_author='{name}', last_reply_summary='Post {pid}', lastpost={} WHERE tid={tid}",
                    1000 + position
                ),
            )
            .await;
        }
    }
    db.migrate().await.unwrap();
    let expected = vec![(1, Some(1)), (2, None), (3, None), (4, None)];
    assert_eq!(last_reply_uids(&db).await, expected, "the backfill");
    // The write paths compute the same values.
    for tid in 1..=4 {
        test_support::refresh_replies(&db, tid).await.unwrap();
    }
    assert_eq!(last_reply_uids(&db).await, expected, "after recomputing");
    test_support::refresh_listing(&db).await.unwrap();
    let drift = test_support::content_drift(db.executor()).await.unwrap();
    let bad: Vec<_> = drift.iter().filter(|(_, amount)| *amount != 0).collect();
    assert!(bad.is_empty(), "{bad:?}");
    services.cleanup().await.unwrap();
}
