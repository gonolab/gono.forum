//! Like, downvote and favorite counters are kept incrementally (docs/queries.md, "Counter columns"): a mark reads the
//! member's own row by primary key and moves its counter only when the state changes, growth points read the like
//! counter, and the cost of a like does not grow with the post's number of likes.
use std::sync::Arc;

use futures_util::future::join_all;
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
    interaction::Interaction,
    test_support::TestFixture,
};

const NOW: i64 = 1_800_000_000;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// Boards and members `1..=members` (1 is the administrator).
async fn site(db: &Database, members: i64) -> i64 {
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    let mut uid = 1;
    while uid <= members {
        let last = (uid + 499).min(members);
        let rows = (uid..=last)
            .map(|n| {
                format!(
                    "({n},'mark-{n}','mark-{n}@example.test','test-password',1,{},{})",
                    NOW - 172_800,
                    i64::from(n == 1)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        execute(db, &format!("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES {rows}")).await;
        uid = last + 1;
    }
    scalar(
        db,
        "SELECT MIN(fid) FROM gonof_forum_forum WHERE visibility='public' AND status=1",
    )
    .await
}

async fn topic(db: &Database, fid: i64, author: i64) -> Published {
    ContentWriter::new(db.clone(), WriteConfig::default())
        .publish_at(
            author,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Counted thread".into(),
                body: "A thread to like".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}

async fn like(db: &Database, uid: i64, pid: i64, desired: bool) {
    Interaction::new(db.clone())
        .set_reaction(actor(uid), pid, desired, &identity::random_token())
        .await
        .unwrap();
}

/// `(reactions, downvotes)` on the post and the number of its like rows.
async fn post_counts(db: &Database, pid: i64) -> (i64, i64, i64) {
    (
        scalar(
            db,
            &format!("SELECT reactions FROM gonof_forum_post WHERE pid={pid}"),
        )
        .await,
        scalar(
            db,
            &format!("SELECT downvotes FROM gonof_forum_post WHERE pid={pid}"),
        )
        .await,
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={pid}"),
        )
        .await,
    )
}

/// What the post's growth row credits, and what it should: base points plus like points per like by someone other
/// than the author.
async fn growth(db: &Database, pid: i64) -> (i64, i64) {
    let credited = scalar(
        db,
        &format!("SELECT credited FROM gonof_common_growth_post WHERE pid={pid}"),
    )
    .await;
    let expected = scalar(
        db,
        &format!(
            "SELECT g.points_base+g.points_like*(SELECT COUNT(*) FROM gonof_forum_post_reaction r WHERE r.pid=g.pid AND r.uid<>g.uid) FROM gonof_common_growth_post g WHERE g.pid={pid}"
        ),
    )
    .await;
    (credited, expected)
}

/// Marks change counters only when the member's state changes, by reading that member's own row: no statement of a
/// like counts the post's likes, and growth points come from the like counter less the author's own like.
#[tokio::test]
async fn marks_move_counters_only_when_their_state_changes() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db, 6).await;
    let thread = topic(db, fid, 2).await;
    let pid = thread.pid;
    let tid = thread.tid;
    let service = Interaction::new(db.clone());

    like(db, 3, pid, true).await;
    like(db, 3, pid, true).await;
    like(db, 4, pid, true).await;
    assert_eq!(post_counts(db, pid).await, (2, 0, 2));
    like(db, 4, pid, false).await;
    like(db, 4, pid, false).await;
    like(db, 5, pid, false).await;
    assert_eq!(post_counts(db, pid).await, (1, 0, 1));
    let (credited, expected) = growth(db, pid).await;
    assert_eq!(credited, expected);

    // A downvote takes back the member's like, and a like takes back the downvote, each moving its own counter.
    service
        .set_downvote(actor(3), pid, true, &identity::random_token())
        .await
        .unwrap();
    service
        .set_downvote(actor(5), pid, true, &identity::random_token())
        .await
        .unwrap();
    assert_eq!(post_counts(db, pid).await, (0, 2, 0));
    like(db, 5, pid, true).await;
    assert_eq!(post_counts(db, pid).await, (1, 1, 1));
    service
        .set_downvote(actor(3), pid, false, &identity::random_token())
        .await
        .unwrap();
    service
        .set_downvote(actor(3), pid, false, &identity::random_token())
        .await
        .unwrap();
    assert_eq!(post_counts(db, pid).await, (1, 0, 1));

    let favorites = || async {
        (
            scalar(
                db,
                &format!("SELECT favorites FROM gonof_forum_thread WHERE tid={tid}"),
            )
            .await,
            scalar(
                db,
                &format!("SELECT COUNT(*) FROM gonof_home_favorite WHERE tid={tid}"),
            )
            .await,
        )
    };
    for (uid, desired) in [(3, true), (3, true), (4, true), (3, false), (6, false)] {
        service
            .set_favorite(actor(uid), tid, desired, &identity::random_token())
            .await
            .unwrap();
    }
    assert_eq!(favorites().await, (1, 1));

    // The author's own like (imported, or from before liking one's own post was refused) is counted on the post but
    // earns no growth points.
    execute(
        db,
        &format!("INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES({pid},2,{NOW})"),
    )
    .await;
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    like(db, 6, pid, true).await;
    assert_eq!(post_counts(db, pid).await, (3, 0, 3));
    let (credited, expected) = growth(db, pid).await;
    assert_eq!(credited, expected, "the author's own like earns nothing");

    // A like reads the member's own row by primary key; nothing counts the post's likes.
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), like(db, 4, pid, true)).await;
    let statements: Vec<String> = profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .map(|record| record.sql)
        .collect();
    assert!(
        statements
            .iter()
            .any(|sql| sql.contains("FROM gonof_forum_post_reaction WHERE pid=? AND uid=?")),
        "{statements:#?}"
    );
    for sql in &statements {
        assert!(
            !sql.contains("COUNT("),
            "a like counts nothing: {sql}\n{statements:#?}"
        );
    }
    assert_eq!(post_counts(db, pid).await, (4, 0, 4));
    fixture.cleanup().await.unwrap();
}

/// The rows a MySQL session has read so far (`Handler_read_*`).
async fn rows_read(db: &Database) -> i64 {
    db::query_scalar(
        "SELECT CAST(SUM(VARIABLE_VALUE) AS SIGNED) FROM performance_schema.session_status WHERE VARIABLE_NAME LIKE 'Handler_read%'",
    )
    .fetch_one(db.executor())
    .await
    .unwrap()
}

/// A like on a post with 1500 likes reads about as many rows as a like on a post with none: neither the counter nor
/// the growth points recount the post's likes.
#[tokio::test]
async fn mysql_a_like_reads_the_same_rows_however_many_likes_the_post_has() {
    gono_forum_web::mysql_only!();
    const LIKES: i64 = 1500;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db, LIKES + 20).await;
    let quiet = topic(db, fid, 2).await.pid;
    let busy = topic(db, fid, 2).await.pid;
    let mut uid = 21;
    while uid <= LIKES + 20 {
        let last = (uid + 499).min(LIKES + 20);
        let rows = (uid..=last)
            .map(|n| format!("({busy},{n},{NOW})"))
            .collect::<Vec<_>>()
            .join(",");
        execute(
            db,
            &format!("INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES {rows}"),
        )
        .await;
        uid = last + 1;
    }
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    like(db, 3, busy, true).await;
    like(db, 3, quiet, true).await;
    // One connection, so the session counters are the like's own.
    let mut config = fixture.config.database.clone();
    config.max_connections = 1;
    let single = Database::connect(&config).await.unwrap();
    let mut read = Vec::new();
    for pid in [quiet, busy, quiet, busy] {
        let before = rows_read(&single).await;
        like(&single, 4 + read.len() as i64, pid, true).await;
        read.push(rows_read(&single).await - before);
    }
    single.pool.close().await;
    assert_eq!(post_counts(db, busy).await.0, LIKES + 3);
    let (credited, expected) = growth(db, busy).await;
    assert_eq!(credited, expected);
    eprintln!("rows read by a like on a post with 0, {LIKES}, 0 and {LIKES} likes: {read:?}");
    let (quiet_rows, busy_rows) = (read[2], read[3]);
    assert!(
        busy_rows < quiet_rows + 100,
        "rows read by a like: {quiet_rows} with no likes, {busy_rows} with {LIKES} likes (all runs: {read:?})"
    );
    fixture.cleanup().await.unwrap();
}

/// On MySQL the primary-key read of a missing like row takes a gap lock, so likes on posts of different threads that
/// fall into the same gap can deadlock. The like that loses is retried and counts once.
#[tokio::test]
async fn mysql_a_like_that_loses_a_gap_lock_deadlock_is_retried() {
    gono_forum_web::mysql_only!();
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db, 4).await;
    let first = topic(db, fid, 2).await.pid;
    let second = topic(db, fid, 2).await.pid;
    db::raw_sql("CREATE TABLE s09_ballast (id INT PRIMARY KEY)")
        .execute(&db.pool)
        .await
        .unwrap();
    let deadlocks = || async {
        scalar(
            db,
            "SELECT COUNT FROM information_schema.INNODB_METRICS WHERE NAME='lock_deadlocks'",
        )
        .await
    };
    let deadlocks_before = deadlocks().await;
    // Another transaction that has written more (so InnoDB picks the like as the victim) holds the gap with a read of
    // a missing like on the other thread's post.
    let mut holder = db.begin_write().await.unwrap();
    let ballast = (1..=200)
        .map(|n| format!("({n})"))
        .collect::<Vec<_>>()
        .join(",");
    db::query(&format!("INSERT INTO s09_ballast(id) VALUES {ballast}"))
        .execute(db.observe(&mut *holder))
        .await
        .unwrap();
    db::query("SELECT uid FROM gonof_forum_post_reaction WHERE pid=? AND uid=? FOR UPDATE")
        .bind(second)
        .bind(1)
        .fetch_optional(db.observe(&mut *holder))
        .await
        .unwrap();
    let liking = {
        let db = db.clone();
        tokio::spawn(async move { like(&db, 3, first, true).await })
    };
    // Wait until the like's insert waits for the gap: an insert that is not blocked finishes at once, so one still
    // listed as running is waiting.
    let waiting = "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE DB=DATABASE() AND INFO LIKE 'INSERT INTO gonof_forum_post_reaction%'";
    let started = std::time::Instant::now();
    while scalar(db, waiting).await == 0 {
        assert!(
            !liking.is_finished(),
            "the like finished without waiting for the gap"
        );
        assert!(
            started.elapsed().as_secs() < 10,
            "the like never waited for the gap"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // Inserting into the gap the like holds closes the cycle: InnoDB rolls back the lighter like.
    let inserted =
        db::query("INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(?,?,?)")
            .bind(second)
            .bind(1)
            .bind(NOW)
            .execute(db.observe(&mut *holder))
            .await;
    assert!(
        inserted.is_ok(),
        "the like, not the heavier transaction, is the victim: {inserted:?}"
    );
    holder.rollback().await.unwrap();
    liking.await.unwrap();
    assert!(deadlocks().await > deadlocks_before, "a deadlock happened");
    assert_eq!(post_counts(db, first).await, (1, 0, 1));
    assert_eq!(post_counts(db, second).await, (0, 0, 0));
    let (credited, expected) = growth(db, first).await;
    assert_eq!(credited, expected);
    db::raw_sql("DROP TABLE s09_ballast")
        .execute(&db.pool)
        .await
        .unwrap();
    fixture.cleanup().await.unwrap();
}

/// A like moves the author's growth points, and the foreign keys of the author's ledger entry and account share-lock
/// the author's member row. The like takes that lock with the liker's, in uid order and before the board path, as
/// posting and moderation lock member rows: while another transaction holds the author's row, the like waits there
/// holding no board or thread lock, so that transaction can go on to lock the thread without a deadlock. Both orders
/// are covered, an author below and above the liker.
#[tokio::test]
async fn mysql_a_like_waits_for_the_authors_member_row_before_the_board() {
    gono_forum_web::mysql_only!();
    const AUTHOR_LOCK: &str = "SELECT uid FROM gonof_common_member WHERE uid=? LOCK IN SHARE MODE";
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db, 4).await;
    for (author, liker) in [(2, 3), (4, 3)] {
        let post = topic(db, fid, author).await;
        // As moderation does: the author's member row first, then the thread.
        let mut holder = db.begin_write().await.unwrap();
        db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(author)
            .fetch_one(db.observe(&mut *holder))
            .await
            .unwrap();
        let profile = Arc::new(RequestProfile::new());
        let liking = {
            let (db, profile) = (db.clone(), profile.clone());
            tokio::spawn(trace::scope(profile, async move {
                Interaction::new(db)
                    .set_reaction(actor(liker), post.pid, true, &identity::random_token())
                    .await
            }))
        };
        // A statement sent and not answered yet is waiting for a lock.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !profile.snapshot().iter().any(|record| {
                record.source == QuerySource::Application
                    && record.sql == AUTHOR_LOCK
                    && record.rows_returned == 0
                    && record.elapsed.is_zero()
            }) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the like by {liker} never waited for author {author}"));
        let thread = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
                .bind(post.tid)
                .fetch_one(db.observe(&mut *holder)),
        )
        .await;
        assert!(
            matches!(thread, Ok(Ok(_))),
            "the waiting like holds no thread lock: {thread:?}"
        );
        holder.rollback().await.unwrap();
        liking.await.unwrap().unwrap();
        assert_eq!(post_counts(db, post.pid).await, (1, 0, 1));
        let (credited, expected) = growth(db, post.pid).await;
        assert_eq!(credited, expected);
    }
    fixture.cleanup().await.unwrap();
}

/// Members liking the first posts of several threads at once, each in a different order: every like that succeeds
/// counts exactly once, and the counters and growth points end up matching the like rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_likes_across_threads_end_with_exact_counts() {
    const THREADS: usize = 5;
    const MEMBERS: i64 = 8;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(db, MEMBERS + 2).await;
    let mut posts = Vec::new();
    for _ in 0..THREADS {
        posts.push(topic(db, fid, 2).await.pid);
    }
    let results = join_all((3..MEMBERS + 3).map(|uid| {
        let db = db.clone();
        let mut order = posts.clone();
        order.rotate_left(uid as usize % THREADS);
        tokio::spawn(async move {
            let service = Interaction::new(db);
            let mut liked = Vec::new();
            for pid in order {
                match service
                    .set_reaction(actor(uid), pid, true, &identity::random_token())
                    .await
                {
                    Ok(()) => liked.push(pid),
                    Err(error) => panic!("like by {uid} on {pid}: {error}"),
                }
            }
            liked
        })
    }))
    .await;
    let liked: Vec<i64> = results
        .into_iter()
        .flat_map(|result| result.unwrap())
        .collect();
    assert_eq!(liked.len(), THREADS * MEMBERS as usize);
    for pid in posts {
        assert_eq!(
            post_counts(db, pid).await,
            (MEMBERS, 0, MEMBERS),
            "post {pid}"
        );
        let (credited, expected) = growth(db, pid).await;
        assert_eq!(credited, expected, "post {pid}");
    }
    fixture.cleanup().await.unwrap();
}
