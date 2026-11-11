//! When the old FID has been read but the thread lock not yet taken, a move must invalidate the original forum's
//! authorization.
use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures_util::FutureExt;
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    i18n::Lang,
    identity::{self, Actor},
    moderation::{self, ReportTarget},
    test_support::TestFixture,
};

const NOW: i64 = 1_810_000_000;
const AUTHOR: i64 = 101;
const MODERATOR: i64 = 103;
const REPORTER: i64 = 104;
const ADMIN: i64 = 105;
const ORIGINAL_BODY: &str = "Original body of the move race test";
const EDITED_BODY: &str = "Body after the administrator confirmed";
const POST_DISCOVERY: &str = "SELECT tid,fid,root_pid FROM gonof_forum_post WHERE pid=?";
const REPORT_DISCOVERY: &str = "SELECT p.pid,p.tid,t.fid FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=? AND p.first=0";
const EDIT_THREAD_LOCK: &str = "SELECT fid,subject,version,deleted_at,closed,displayorder,digest,readperm,visibility,publication_state FROM gonof_forum_thread WHERE tid=? FOR UPDATE";
const REPORT_THREAD_LOCK: &str = "SELECT fid FROM gonof_forum_thread WHERE tid=? FOR UPDATE";

#[derive(Clone, Copy, Debug)]
enum Operation {
    Edit,
    Hide,
    ResolveReport,
}
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn wait_for_thread_lock(profile: &RequestProfile, operation: Operation) {
    let (discovery, lock) = match operation {
        Operation::Edit | Operation::Hide => (POST_DISCOVERY, EDIT_THREAD_LOCK),
        Operation::ResolveReport => (REPORT_DISCOVERY, REPORT_THREAD_LOCK),
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let records = profile.snapshot();
            if let Some(pending) = records
                .iter()
                .find(|record| record.source == QuerySource::Application && record.sql == lock)
            {
                let discovered = records
                    .iter()
                    .find(|record| {
                        record.source == QuerySource::Application && record.sql == discovery
                    })
                    .expect("the old FID of the post must be discovered first");
                assert_eq!(discovered.rows_returned, 1);
                assert!(discovered.error.is_none());
                assert!(discovered.sequence < pending.sequence);
                assert_eq!(pending.rows_returned, 0);
                assert!(
                    pending.elapsed.is_zero(),
                    "the thread lock is still held by the move transaction"
                );
                return;
            }
            // Only lowers polling overhead; the move is allowed to commit only after the query-start event is observed.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the request authorized by the old forum must be seen waiting for the thread lock");
}

struct Subject {
    pid: i64,
    tid: i64,
    report_id: i64,
}
async fn perform(
    db: &Database,
    operation: Operation,
    uid: i64,
    subject: &Subject,
    post_version: i64,
    thread_version: i64,
) -> Result<(), ContentWriteError> {
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86_400);
    match operation {
        Operation::Edit => editor
            .edit_at(
                actor(uid),
                EditRequest {
                    visibility: None,
                    pid: subject.pid,
                    expected_post_version: post_version,
                    expected_thread_version: thread_version,
                    title: None,
                    body: Some(EDITED_BODY.into()),
                    request_key: identity::random_token(),
                    reason: "Corrected after confirming the current forum permission".into(),
                },
                NOW + 20,
            )
            .await
            .map(|_| ()),
        Operation::Hide => editor
            .change_at(
                actor(uid),
                ChangeRequest {
                    pid: subject.pid,
                    expected_post_version: post_version,
                    expected_thread_version: thread_version,
                    request_key: identity::random_token(),
                    reason: "Hidden after confirming the current forum permission".into(),
                    action: ContentAction::Hide,
                },
                NOW + 20,
            )
            .await
            .map(|_| ()),
        Operation::ResolveReport => {
            moderation::resolve_report(
                db,
                actor(uid),
                subject.report_id,
                "resolved",
                "Report handled after confirming the current forum permission",
                NOW + 20,
            )
            .await
        }
    }
}

async fn contract(_fixture: &TestFixture, database: &Database, operation: Operation) {
    content::seed_forums(database).await.unwrap();
    for (uid, role) in [(AUTHOR, 0), (MODERATOR, 3), (REPORTER, 0), (ADMIN, 1)] {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,adminid,regdate) VALUES(?,?,?,'fixture-hash',1,?,?)")
            .bind(uid).bind(format!("movement-{uid}")).bind(format!("movement-{uid}@example.test"))
            .bind(role).bind(NOW - 200_000).execute(database.executor()).await.unwrap();
    }
    let source_fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(database.executor())
        .await
        .unwrap();
    let target_fid: i64 =
        db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE fid<>? ORDER BY fid LIMIT 1")
            .bind(source_fid)
            .fetch_one(database.executor())
            .await
            .unwrap();
    db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(?,?)")
        .bind(MODERATOR)
        .bind(source_fid)
        .execute(database.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(database.clone(), WriteConfig::default());
    let thread = writer
        .publish_at(
            ADMIN,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid: source_fid,
                title: "Thread being moved".into(),
                body: "First post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap();
    let reply = writer
        .reply_at(
            AUTHOR,
            NewReply {
                tid: thread.tid,
                root_pid: None,
                reply_to_pid: None,
                body: ORIGINAL_BODY.into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 1,
        )
        .await
        .unwrap();
    let report_id = moderation::submit_report(
        database,
        actor(REPORTER),
        &ReportTarget {
            target_type: "reply".into(),
            target_id: reply.pid,
        },
        "Pending report",
        &identity::random_token(),
        NOW + 2,
        10,
    )
    .await
    .unwrap();
    let subject = Subject {
        pid: reply.pid,
        tid: thread.tid,
        report_id,
    };
    let editor = ContentEditor::new(database.clone(), WriteConfig::default(), 86_400);
    let before = editor
        .editable_at(actor(MODERATOR), reply.pid, NOW + 3)
        .await
        .unwrap();
    assert!(before.can_manage);
    assert_eq!(before.fid, source_fid);
    moderation::report_detail(database, actor(MODERATOR), report_id, true)
        .await
        .unwrap();
    let request_count: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_forum_post_request")
        .fetch_one(database.executor())
        .await
        .unwrap();

    // The ordinary move entry point also locks the forum first, which would serialize the race at an earlier point.
    // Here a separate transaction holds the thread lock and runs the same FID/version UPDATE and database cascade,
    // specifically to cover the deeper line of defense.
    // It does not commit before the old request actually issues the thread FOR UPDATE, so lock order does not depend on
    // sleep.
    let mut mover = database.begin_write().await.unwrap();
    db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
        .bind(subject.tid)
        .fetch_one(database.observe(&mut *mover))
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let old_request = trace::scope(
        profile.clone(),
        Box::pin(perform(
            database,
            operation,
            MODERATOR,
            &subject,
            before.version,
            before.thread_version,
        )),
    );
    let move_and_release = async {
        wait_for_thread_lock(&profile, operation).await;
        db::query(
            "UPDATE gonof_forum_thread SET fid=?,version=version+1 WHERE tid=? AND version=?",
        )
        .bind(target_fid)
        .bind(subject.tid)
        .bind(before.thread_version)
        .execute(database.observe(&mut *mover))
        .await
        .unwrap();
        mover.commit().await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(old_request, move_and_release)
    })
    .await
    .expect("after the move commits the old request must stop waiting and be refused");
    // Edits and moderation say so in the editor's words, report handling in the review queue's.
    let moved = match operation {
        Operation::Edit | Operation::Hide => Lang::EN.editor_error_thread_moved(),
        Operation::ResolveReport => Lang::EN.governance_manage_error_report_moved(),
    };
    assert!(
        matches!(result, Err(ContentWriteError::Conflict(message)) if message.contains(moved)),
        "the old FID must be refused before any version or business write: {result:?}"
    );
    let records = profile.snapshot();
    assert!(
        records
            .iter()
            .filter(|r| r.source == QuerySource::Application)
            .all(|r| r.sql.starts_with("SELECT ")),
        "a move conflict only reads: no body, moderation state, audit or idempotency write"
    );

    let current = editor
        .editable_at(actor(ADMIN), subject.pid, NOW + 4)
        .await
        .unwrap();
    assert_eq!(current.fid, target_fid);
    assert_eq!(current.body, ORIGINAL_BODY);
    assert_eq!(current.version, before.version);
    assert_eq!(current.invisible, 0);
    assert_eq!(current.deleted_at, 0);
    assert_eq!(current.thread_version, before.thread_version + 1);
    assert_eq!(
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_forum_post WHERE tid=? AND fid<>?")
            .bind(subject.tid)
            .bind(target_fid)
            .fetch_one(database.executor())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_forum_post_revision WHERE pid=?")
            .bind(subject.pid)
            .fetch_one(database.executor())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_forum_post_request")
            .fetch_one(database.executor())
            .await
            .unwrap(),
        request_count
    );
    assert_eq!(
        moderation::report_detail(database, actor(ADMIN), report_id, true)
            .await
            .unwrap()
            .status,
        "pending"
    );
    assert!(
        matches!(
            perform(
                database,
                operation,
                MODERATOR,
                &subject,
                current.version,
                current.thread_version
            )
            .await,
            Err(ContentWriteError::NotFound)
        ),
        "even with the new versions the source forum's moderator permission does not carry over"
    );

    perform(
        database,
        operation,
        ADMIN,
        &subject,
        current.version,
        current.thread_version,
    )
    .await
    .unwrap();
    let after = editor
        .editable_at(actor(ADMIN), subject.pid, NOW + 30)
        .await
        .unwrap();
    match operation {
        Operation::Edit => assert_eq!(after.body, EDITED_BODY),
        Operation::Hide => {
            assert_eq!(after.body, ORIGINAL_BODY);
            assert_eq!(after.invisible, -1);
        }
        Operation::ResolveReport => {
            assert_eq!(after.body, ORIGINAL_BODY);
            let report = moderation::report_detail(database, actor(ADMIN), report_id, true)
                .await
                .unwrap();
            assert_eq!(report.status, "resolved");
            assert_eq!(report.handler_uid, Some(ADMIN));
        }
    }
}

async fn run(operation: Operation) {
    let fixture = TestFixture::migrated().await.unwrap();
    // The move barrier and the request each take one connection, with extra connections for queries and cleanup; does
    // not depend on the local pool size.
    let mut config = fixture.config.database.clone();
    config.max_connections = 4;
    let database = Database::connect(&config).await.unwrap();
    let result = AssertUnwindSafe(Box::pin(contract(&fixture, &database, operation)))
        .catch_unwind()
        .await;
    database.pool.close().await;
    fixture.cleanup().await.unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
#[tokio::test]
async fn moved_thread_rejects_editor_waiting_with_old_forum_scope() {
    // Only verifies MySQL's row lock wait orchestration.
    gono_forum_web::mysql_only!();
    run(Operation::Edit).await;
}
#[tokio::test]
async fn moved_thread_rejects_hide_waiting_with_old_forum_scope() {
    // Only verifies MySQL's row lock wait orchestration.
    gono_forum_web::mysql_only!();
    run(Operation::Hide).await;
}
#[tokio::test]
async fn moved_thread_rejects_report_resolution_waiting_with_old_forum_scope() {
    // Only verifies MySQL's row lock wait orchestration.
    gono_forum_web::mysql_only!();
    run(Operation::ResolveReport).await;
}
