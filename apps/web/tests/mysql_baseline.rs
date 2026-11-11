use std::sync::Arc;

use gono_forum_web::{
    content,
    debug::trace::{self, QueryRecord, QuerySource, RequestProfile},
    test_support::TestFixture,
    urls::Sort,
};

fn application_records(profile: &RequestProfile, expected: usize) -> Vec<QueryRecord> {
    let records = profile.snapshot();
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            index + 1,
            "application and driver records must keep consecutive sequence numbers"
        );
    }
    let records: Vec<_> = records
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .collect();
    assert_eq!(
        records.len(),
        expected,
        "the application query count must exclude driver setup and include application SETs"
    );
    records
}

#[tokio::test]
async fn mysql_baseline_has_position_keys_and_bounded_read_paths() {
    // Only verifies the MySQL baseline's indexes and execution plans.
    gono_forum_web::mysql_only!();
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    content::seed_forums(db).await.unwrap();
    content::seed_fixture(db).await.unwrap();
    db.migrate().await.unwrap();
    let columns: Vec<String> = gono_forum_web::db::query_scalar(
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='gonof_forum_post' AND INDEX_NAME='PRIMARY' ORDER BY SEQ_IN_INDEX",
    ).fetch_all(db.executor()).await.unwrap();
    assert_eq!(columns, ["tid", "position"]);

    let daily: i64 =
        gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(db.executor())
            .await
            .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let rows = trace::scope(
        profile.clone(),
        content::list_scope_for_viewer(
            db,
            &content::ListScope::of_forum(daily, None),
            Sort::Latest,
            None,
            None,
            1,
            20,
            content::ListViewer {
                forums: &[daily],
                ..content::ListViewer::default()
            },
        ),
    )
    .await
    .unwrap();
    assert!(!rows.threads.is_empty());
    assert!(
        rows.threads
            .iter()
            .all(|thread| thread.posts.is_empty() && thread.body.iter().all(String::is_empty))
    );
    // Two single-table queries for the thread count and the current page, plus one `IN` query for the thumbnails by
    // this page's thread IDs (`gonof_forum_thread_image`).
    let (thumbnails, list_queries): (Vec<_>, Vec<_>) = application_records(&profile, 3)
        .into_iter()
        .partition(|record| record.sql.contains("gonof_forum_thread_image"));
    assert_eq!(thumbnails.len(), 1, "thumbnails add exactly one query");
    assert!(thumbnails[0].sql.contains("ti.tid IN ("));
    for query in &list_queries {
        let sql = query.sql.to_ascii_lowercase();
        assert!(
            sql.contains("gonof_forum_thread"),
            "the list runs only the thread count and current page queries"
        );
        assert!(
            !sql.contains("gonof_forum_post"),
            "the list must not load post bodies"
        );
        if sql.contains(" join ") {
            panic!("each list query reads one table and joins no other: {sql}");
        }
    }
    assert!(list_queries.iter().any(
        |record| record.sql.contains("FORCE INDEX") && record.sql.contains("displayorder DESC")
    ));

    let mut thread = content::thread(db, 103).await.unwrap().unwrap();
    assert!(
        thread.posts.is_empty(),
        "the thread metadata query must not load the replies"
    );
    assert_eq!((thread.reply_count, thread.root_replies), (78, 54));
    let profile = Arc::new(RequestProfile::new());
    trace::scope(
        profile.clone(),
        content::load_replies(
            db,
            &mut thread,
            2,
            true,
            None,
            1,
            gono_forum_web::test_support::SEEDED_PAGING,
        ),
    )
    .await
    .unwrap();
    assert_eq!(thread.posts.len(), 28);
    assert_eq!(thread.posts.first().unwrap().position, 52);
    assert_eq!(thread.posts.last().unwrap().position, 79);
    let records = application_records(&profile, 1);
    assert_eq!(records[0].rows_returned, 28);
    assert!(!records[0].sql.to_ascii_uppercase().contains("COUNT("));
    assert!(records[0].sql.contains("FORCE INDEX (PRIMARY)"));

    content::load_replies(
        db,
        &mut thread,
        2,
        false,
        None,
        1,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    assert_eq!(thread.posts.len(), 4);
    assert!(
        thread
            .posts
            .iter()
            .all(|post| post.root_pid.is_none() && post.root_position >= 51)
    );
    content::load_replies(
        db,
        &mut thread,
        1,
        false,
        Some(103002),
        2,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    let children: Vec<_> = thread
        .posts
        .iter()
        .filter(|post| post.root_pid == Some(103002))
        .collect();
    assert_eq!(children.len(), 4);
    assert_eq!(children[0].sub_position, 21);

    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=-2 WHERE pid=103002")
        .execute(db.executor())
        .await
        .unwrap();
    let mut hidden = content::thread(db, 103).await.unwrap().unwrap();
    assert_eq!((hidden.reply_count, hidden.root_replies), (78, 54));
    content::load_replies(
        db,
        &mut hidden,
        1,
        false,
        Some(103002),
        2,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    assert!(
        hidden
            .posts
            .iter()
            .filter(|post| post.pid == 103002 || post.root_pid == Some(103002))
            .all(|post| post.hidden && post.body.is_empty())
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn effective_reply_summary_tracks_governance_without_releasing_positions() {
    use gono_forum_web::{
        content::{
            edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
            writes::{ContentWriter, NewReply, NewThread, WriteConfig},
        },
        db::Database,
        identity::{self, Actor},
    };
    const NOW: i64 = 1_800_100_000;
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::db::query(
        "INSERT INTO gonof_common_member(uid,username,adminid,password) VALUES(900001,'summary-admin',1,'summary-fixture-password-hash')",
    )
    .execute(db.executor())
    .await
    .unwrap();
    let fid: i64 =
        gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let topic = writer
        .publish_at(
            900001,
            NewThread {
                visibility: Default::default(),
                fid,
                title: "Summary upkeep".into(),
                body: "First post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
                activity: Default::default(),
            },
            NOW,
        )
        .await
        .unwrap();
    let root = writer
        .reply_at(
            900001,
            NewReply {
                tid: topic.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Visible top-level reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 1,
        )
        .await
        .unwrap();
    let child = writer
        .reply_at(
            900001,
            NewReply {
                tid: topic.tid,
                root_pid: Some(root.pid),
                reply_to_pid: Some(root.pid),
                body: "Visible nested reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 2,
        )
        .await
        .unwrap();
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86400);
    let actor = Actor {
        uid: 900001,
        credentials_version: 1,
        api_key: None,
    };
    async fn state(db: &Database, tid: i64) -> (i64, i64, Option<i64>, String, String) {
        let row=gono_forum_web::db::query("SELECT replies,valid_replies,last_reply_pid,last_reply_author,last_reply_summary FROM gonof_forum_thread WHERE tid=?").bind(tid).fetch_one(db.executor()).await.unwrap();
        (
            row.try_get("replies").unwrap(),
            row.try_get("valid_replies").unwrap(),
            row.try_get("last_reply_pid").unwrap(),
            row.try_get("last_reply_author").unwrap(),
            row.try_get("last_reply_summary").unwrap(),
        )
    }
    assert_eq!(
        state(db, topic.tid).await,
        (
            2,
            2,
            Some(child.pid),
            "summary-admin".into(),
            "Visible nested reply".into()
        )
    );
    for (pid, action, expected_count, expected_pid, expected_summary) in [
        (root.pid, ContentAction::Hide, 0, None, ""),
        (
            root.pid,
            ContentAction::Unhide,
            2,
            Some(child.pid),
            "Visible nested reply",
        ),
        (
            child.pid,
            ContentAction::Withdraw,
            1,
            Some(root.pid),
            "Visible top-level reply",
        ),
        (
            child.pid,
            ContentAction::Restore,
            2,
            Some(child.pid),
            "Visible nested reply",
        ),
    ] {
        let post = editor.editable_at(actor, pid, NOW + 10).await.unwrap();
        editor
            .change_at(
                actor,
                ChangeRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: "Summary contract".into(),
                    action,
                },
                NOW + 10,
            )
            .await
            .unwrap();
        let result = state(db, topic.tid).await;
        assert_eq!(
            (result.0, result.1, result.2, result.4),
            (2, expected_count, expected_pid, expected_summary.into())
        );
    }
    let post = editor
        .editable_at(actor, child.pid, NOW + 20)
        .await
        .unwrap();
    editor
        .edit_at(
            actor,
            EditRequest {
                visibility: None,
                pid: child.pid,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                title: None,
                body: Some("Corrected summary".into()),
                request_key: identity::random_token(),
                reason: "Correct the summary".into(),
            },
            NOW + 20,
        )
        .await
        .unwrap();
    gono_forum_web::db::query(
        "UPDATE gonof_common_member SET username='renamed-summary-admin' WHERE uid=900001",
    )
    .execute(db.executor())
    .await
    .unwrap();
    // The rename entry point rewrites the last replier through `content::copies::member_changed`.
    gono_forum_web::test_support::sync_member_copies(db, 900001)
        .await
        .unwrap();
    assert_eq!(
        state(db, topic.tid).await,
        (
            2,
            2,
            Some(child.pid),
            "renamed-summary-admin".into(),
            "Corrected summary".into()
        )
    );
    services.cleanup().await.unwrap();
}
