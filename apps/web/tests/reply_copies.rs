//! A thread's live-reply copies (`valid_replies` and the last reply, docs/queries.md, "Copy columns") follow
//! moderation one post at a time: hiding, unhiding, withdrawing and restoring move the count by what changed and look
//! for the last reply again only when it can have moved, without counting the thread.
use std::sync::Arc;

use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
    test_support::TestFixture,
};

const NOW: i64 = 1_800_050_000;
const ADMIN: Actor = Actor {
    uid: 1,
    credentials_version: 1,
    api_key: None,
};

async fn execute(db: &Database, sql: &str) {
    db::query(sql)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

/// Boards, an administrator (uid 1) and members 2 to 6; returns a public board.
async fn site(db: &Database) -> i64 {
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=6 {
        execute(db, &format!(
            "INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES ({uid},'copy-{uid}','copy-{uid}@example.test','test-password',1,{},{})",
            NOW - 172_800,
            i64::from(uid == 1)
        ))
        .await;
    }
    scalar(
        db,
        "SELECT MIN(fid) FROM gonof_forum_forum WHERE visibility='public' AND status=1",
    )
    .await
}

async fn thread(db: &Database, fid: i64, author: i64) -> i64 {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .publish_at(
            author,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Copied thread".into(),
                body: "Opening post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
        .tid
}

async fn reply(db: &Database, author: i64, tid: i64, root: Option<i64>, at: i64) -> i64 {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .reply_at(
            author,
            NewReply {
                tid,
                root_pid: root,
                reply_to_pid: root,
                body: format!("Reply by {author} at {at}"),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            at,
        )
        .await
        .unwrap()
        .pid
}

async fn moderate(db: &Database, pid: i64, action: ContentAction, at: i64) -> bool {
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86_400);
    let Ok(post) = editor.editable_at(ADMIN, pid, at).await else {
        return false;
    };
    editor
        .change_at(
            ADMIN,
            ChangeRequest {
                pid,
                action,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                request_key: identity::random_token(),
                reason: "Copy check".into(),
            },
            at,
        )
        .await
        .is_ok()
}

/// What a recount from scratch gives, with the parent's hidden state read from the parent row (as the consistency
/// check does): `(live replies, last live reply by floor number)`.
async fn recount(db: &Database, tid: i64) -> (i64, Option<i64>) {
    let live = "FROM gonof_forum_post p LEFT JOIN gonof_forum_post parent ON parent.pid = p.root_pid
        WHERE p.tid = ? AND p.first = 0 AND p.invisible = 0 AND p.deleted_at = 0 AND COALESCE(parent.invisible, 0) = 0";
    let count = db::query_scalar(&format!("SELECT COUNT(*) {live}"))
        .bind(tid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    let last = db::query_scalar(&format!(
        "SELECT p.pid {live} ORDER BY p.position DESC LIMIT 1"
    ))
    .bind(tid)
    .fetch_optional(db.executor())
    .await
    .unwrap();
    (count, last)
}

async fn copies(db: &Database, tid: i64) -> (i64, Option<i64>) {
    let row = db::query("SELECT valid_replies,last_reply_pid FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    (
        row.try_get("valid_replies").unwrap(),
        row.try_get("last_reply_pid").unwrap(),
    )
}

/// Random hiding, unhiding, withdrawing and restoring of floors and nested replies (and now and then the opening
/// post): after every step the copies equal a recount, and the content consistency check finds nothing.
#[tokio::test]
async fn moderation_keeps_the_live_reply_copies_equal_to_a_recount() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db).await;
    let tid = thread(db, fid, 2).await;
    let first: i64 = scalar(
        db,
        &format!("SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"),
    )
    .await;
    let mut posts = Vec::new();
    let mut at = NOW;
    for floor in 0..8 {
        at += 1;
        let root = reply(db, 2 + floor % 5, tid, None, at).await;
        posts.push(root);
        for child in 0..(floor % 4) {
            at += 1;
            posts.push(reply(db, 2 + child % 5, tid, Some(root), at).await);
        }
    }
    assert_eq!(copies(db, tid).await, recount(db, tid).await);
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut applied = 0;
    for step in 0..80 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let pid = if seed.is_multiple_of(13) {
            first
        } else {
            posts[(seed % posts.len() as u64) as usize]
        };
        let action = [
            ContentAction::Hide,
            ContentAction::Unhide,
            ContentAction::Withdraw,
            ContentAction::Restore,
        ][((seed >> 8) % 4) as usize];
        at += 1;
        if moderate(db, pid, action, at).await {
            applied += 1;
        }
        assert_eq!(
            copies(db, tid).await,
            recount(db, tid).await,
            "step {step}: {action:?} on {pid}"
        );
    }
    assert!(applied > 30, "only {applied} actions applied");
    let mut conn = db.pool.acquire().await.unwrap();
    let drift: Vec<_> = gono_forum_web::test_support::content_drift(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, count)| *count != 0)
        .collect();
    assert!(drift.is_empty(), "{drift:?}");
    drop(conn);
    fixture.cleanup().await.unwrap();
}

/// Hiding a floor with nested replies, and the last reply, counts nothing: the nested replies are taken from the
/// rewrite's affected rows and the last reply is read from the end of the primary key.
#[tokio::test]
async fn hiding_floors_counts_nothing() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db).await;
    let tid = thread(db, fid, 2).await;
    let root = reply(db, 3, tid, None, NOW + 1).await;
    reply(db, 4, tid, Some(root), NOW + 2).await;
    let last = reply(db, 5, tid, None, NOW + 3).await;
    for (pid, action) in [
        (root, ContentAction::Hide),
        (last, ContentAction::Hide),
        (root, ContentAction::Unhide),
    ] {
        let profile = Arc::new(RequestProfile::new());
        assert!(trace::scope(profile.clone(), moderate(db, pid, action, NOW + 10)).await);
        let statements: Vec<String> = profile
            .snapshot()
            .into_iter()
            .filter(|record| record.source == QuerySource::Application)
            .map(|record| record.sql)
            .filter(|sql| sql.contains("gonof_forum_post") && sql.contains("COUNT("))
            .collect();
        assert!(
            statements.is_empty(),
            "{action:?} on {pid}: {statements:#?}"
        );
        assert_eq!(copies(db, tid).await, recount(db, tid).await);
    }
    fixture.cleanup().await.unwrap();
}

/// The rows the MySQL server has read so far (`Handler_read_*`). Moderation's write transaction closes its connection
/// afterwards, so the session counters cannot be read; the server's include other connections' reads, so callers take
/// the smallest of several rounds.
async fn rows_read(db: &Database) -> i64 {
    db::query_scalar(
        "SELECT CAST(SUM(VARIABLE_VALUE) AS SIGNED) FROM performance_schema.global_status WHERE VARIABLE_NAME LIKE 'Handler_read%'",
    )
    .fetch_one(db.executor())
    .await
    .unwrap()
}

/// Hiding and unhiding a reply in a thread of 3,000 replies reads about as many rows as in a thread of five: the
/// copies no longer count the thread or sort it to find the last reply.
#[tokio::test]
async fn mysql_hiding_a_reply_reads_the_same_rows_however_long_the_thread() {
    gono_forum_web::mysql_only!();
    const REPLIES: i64 = 3000;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db).await;
    let short = thread(db, fid, 2).await;
    let mut short_replies = Vec::new();
    for n in 1..=5 {
        short_replies.push(reply(db, 3, short, None, NOW + n).await);
    }
    let long = thread(db, fid, 2).await;
    let base = scalar(db, "SELECT MAX(pid) FROM gonof_forum_post_tableid").await;
    let mut n = 1;
    while n <= REPLIES {
        let last = (n + 499).min(REPLIES);
        let ids = (n..=last)
            .map(|n| format!("({})", base + n))
            .collect::<Vec<_>>()
            .join(",");
        execute(
            db,
            &format!("INSERT INTO gonof_forum_post_tableid (pid) VALUES {ids}"),
        )
        .await;
        let rows = (n..=last)
            .map(|n| {
                format!(
                    "({},{fid},{long},3,0,{},'Long reply {n}',{},{n})",
                    base + n,
                    n + 1,
                    NOW + n
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        execute(db, &format!(
            "INSERT INTO gonof_forum_post (pid,fid,tid,authorid,first,position,message,dateline,root_position) VALUES {rows}"
        ))
        .await;
        n = last + 1;
    }
    execute(db, &format!(
        "UPDATE gonof_forum_thread SET maxposition={},replies={REPLIES},root_replies={REPLIES} WHERE tid={long}",
        REPLIES + 1
    ))
    .await;
    gono_forum_web::test_support::refresh_replies(db, long)
        .await
        .unwrap();
    let middle = base + REPLIES / 2;
    let mut quiet = i64::MAX;
    let mut busy = i64::MAX;
    let mut at = NOW + 9000;
    for _ in 0..3 {
        for (pid, smallest) in [(short_replies[2], &mut quiet), (middle, &mut busy)] {
            for action in [ContentAction::Hide, ContentAction::Unhide] {
                at += 1;
                let before = rows_read(db).await;
                assert!(moderate(db, pid, action, at).await);
                *smallest = (*smallest).min(rows_read(db).await - before);
            }
        }
    }
    for tid in [short, long] {
        assert_eq!(copies(db, tid).await, recount(db, tid).await);
    }
    eprintln!(
        "fewest rows read moderating a reply: {quiet} in a thread of 5 replies, {busy} in one of {REPLIES}"
    );
    assert!(
        busy < quiet + 300,
        "rows read: {quiet} in a thread of 5 replies, {busy} in one of {REPLIES}"
    );
    fixture.cleanup().await.unwrap();
}
