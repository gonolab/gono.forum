//! Board structure and its edges: merge jobs that overlap or meet a new child board, promoting and creating
//! top-level boards on a multi-domain site (domain links, versions and the top-level limit), subsite paths against
//! board page addresses, the thread move form, and the board icon picker.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::{
    content::writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    db,
    identity::{self, Actor},
    organization::{self, ForumInput, MergeReport},
    test_support::raise_posting_quotas,
};
use site::Site;

async fn admin(site: &Site) -> Actor {
    let row = db::query(
        "SELECT uid,credentials_version FROM gonof_common_member WHERE username='tester-1'",
    )
    .fetch_one(site.db().executor())
    .await
    .unwrap();
    Actor {
        uid: row.try_get("uid").unwrap(),
        credentials_version: row.try_get("credentials_version").unwrap(),
        api_key: None,
    }
}

async fn version(site: &Site, fid: i64) -> i64 {
    site.scalar(&format!(
        "SELECT version FROM gonof_forum_forum WHERE fid={fid}"
    ))
    .await
}

async fn thread(site: &Site, author: i64, fid: i64, title: &str) -> i64 {
    raise_posting_quotas(site.db()).await.unwrap();
    ContentWriter::new(site.db().clone(), WriteConfig::default())
        .publish_at(
            author,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: title.into(),
                body: format!("Body of {title}"),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            identity::now() - 60,
        )
        .await
        .unwrap()
        .tid
}

/// The board's stored fields as a save form, so a test changes only what it means to.
async fn input(site: &Site, fid: i64) -> ForumInput {
    let row = db::query("SELECT * FROM gonof_forum_forum WHERE fid=?")
        .bind(fid)
        .fetch_one(site.db().executor())
        .await
        .unwrap();
    ForumInput {
        fid,
        parent_fid: row
            .try_get::<Option<i64>, _>("parent_fid")
            .unwrap()
            .unwrap_or(0),
        name: row.try_get("name").unwrap(),
        slug: row.try_get("slug").unwrap(),
        description: row.try_get("description").unwrap(),
        guidance: row.try_get("guidance").unwrap(),
        displayorder: row.try_get("displayorder").unwrap(),
        status: row.try_get("status").unwrap(),
        visibility: db::text(&row, "visibility").unwrap(),
        version: row.try_get("version").unwrap(),
        ..Default::default()
    }
}

/// A merge into board B and a merge of B into C must not overlap: while one job still fills B, merging B away is
/// refused; a B-to-C job queued anyway (before that rule, or by an older process) waits until the threads have all
/// arrived, so none of them is left behind in the disabled B.
#[tokio::test]
async fn a_board_still_being_filled_is_merged_away_only_after_its_job() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let (a, b, c) = (
        site.fid("releases").await,
        site.fid("daily").await,
        site.fid("hosting").await,
    );
    let from_a = thread(&site, admin.uid, a, "Started in A").await;
    let from_b = thread(&site, admin.uid, b, "Started in B").await;
    let first = organization::request_merge(db, admin, a, b, version(&site, a).await, "A into B")
        .await
        .unwrap();
    assert!(matches!(
        organization::request_merge(db, admin, b, c, version(&site, b).await, "B into C").await,
        Err(ContentWriteError::Conflict(message)) if message.contains("still moving threads into this forum")
    ));

    let now = identity::now();
    let payload =
        serde_json::json!({"source": b, "target": c, "uid": admin.uid, "reason": "B into C"});
    let older = db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'forum',?,?,?,?)")
        .bind(format!("{}:{b}:older", organization::MERGE_EVENT))
        .bind(organization::MERGE_EVENT)
        .bind(b)
        .bind(payload.to_string())
        .bind(now)
        .bind(now)
        .execute(db.executor())
        .await
        .unwrap()
        .last_insert_id();
    // Another process holds the A-to-B job, so this round claims the B-to-C one.
    site.execute(&format!(
        "UPDATE gonof_common_outbox SET status='processing',lease_token='elsewhere',lease_until={} WHERE eventid={first}",
        now + 300
    ))
    .await;
    assert_eq!(
        organization::process_merges(db, &site.services.global, now)
            .await
            .unwrap(),
        MergeReport::default()
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT fid FROM gonof_forum_thread WHERE tid={from_b}"
        ))
        .await,
        b,
        "nothing moves while B is still being filled"
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT status FROM gonof_forum_forum WHERE fid={b}"
        ))
        .await,
        1
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE eventid={older} AND status='pending' AND attempts=0 AND available_at>{now}"
        ))
        .await,
        1,
        "the waiting job goes back to the queue without spending an attempt"
    );

    // The other process lets its lease run out: A-to-B finishes, then B-to-C when its wait is over.
    site.execute(&format!(
        "UPDATE gonof_common_outbox SET lease_until=1 WHERE eventid={first}"
    ))
    .await;
    let report = organization::process_merges(db, &site.services.global, now)
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (1, 1));
    let report = organization::process_merges(db, &site.services.global, now + 60)
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (2, 1));
    for tid in [from_a, from_b] {
        assert_eq!(
            site.scalar(&format!(
                "SELECT fid FROM gonof_forum_thread WHERE tid={tid}"
            ))
            .await,
            c,
            "thread {tid} ends in C"
        );
    }
    assert_eq!(
        site.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_forum WHERE fid IN ({a},{b}) AND status=0"
        ))
        .await,
        2
    );
    site.services.cleanup_ref().await.unwrap();
}

/// A board being merged away cannot take a child board: saving one under it is refused, and a child that got there
/// some other way stops the job instead of being disabled with its parent.
#[tokio::test]
async fn a_board_being_merged_away_takes_no_child_boards() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let (source, target, child) = (
        site.fid("showcase").await,
        site.fid("daily").await,
        site.fid("ai").await,
    );
    thread(&site, admin.uid, source, "To be merged").await;
    let eventid = organization::request_merge(
        db,
        admin,
        source,
        target,
        version(&site, source).await,
        "Merge showcase",
    )
    .await
    .unwrap();
    let moved = ForumInput {
        parent_fid: source,
        ..input(&site, child).await
    };
    assert!(matches!(
        organization::save_forum(db, admin, &moved).await,
        Err(ContentWriteError::Validation(message)) if message.contains("is being merged into another one")
    ));

    site.execute(&format!(
        "UPDATE gonof_forum_forum SET parent_fid={source} WHERE fid={child}"
    ))
    .await;
    let report = organization::process_merges(db, &site.services.global, identity::now())
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (1, 0));
    assert_eq!(
        site.scalar(&format!(
            "SELECT status FROM gonof_forum_forum WHERE fid={source}"
        ))
        .await,
        1,
        "the source board stays enabled with its child"
    );
    assert_eq!(
        site.text(&format!(
            "SELECT status FROM gonof_common_outbox WHERE eventid={eventid}"
        ))
        .await,
        "failed"
    );
    assert!(
        site.text(&format!(
            "SELECT last_error FROM gonof_common_outbox WHERE eventid={eventid}"
        ))
        .await
        .starts_with("the source forum has child forums")
    );
    site.changed();
    let home = site.get("/ai", None).await;
    assert_eq!(home.status, StatusCode::OK);
    site.services.cleanup_ref().await.unwrap();
}
