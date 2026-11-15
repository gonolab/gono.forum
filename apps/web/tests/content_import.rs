//! Importing a content dataset (docs/content-import.md): forums with a child forum, members (one name taken by the
//! administrator), threads written in several batches, nested conversations, and the copies and counters agreeing
//! with the rows afterwards. Also imports the committed example, which is the setup wizard's sample content.
use std::path::{Path, PathBuf};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use gono_forum_web::{
    content::{self, dataset},
    db,
    identity::Identity,
    test_support::{TestFixture, assert_identity_consistent, content_drift, detached_global},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Content-owner-password-1";
/// More than two batches of the import (200 threads each).
const THREADS: usize = 450;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gonof_content_{}_{}",
            std::process::id(),
            &gono_forum_web::identity::random_token()[..16]
        ));
        std::fs::create_dir_all(path.join("threads")).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_dataset(dir: &Path) {
    std::fs::write(
        dir.join("manifest.json"),
        "{\"format\": \"gono-forum-content\", \"version\": 1, \"language\": \"en\"}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("boards.json"),
        r#"[
  {"slug": "community", "name": "Community", "description": "Everything about us"},
  {"slug": "intros", "name": "Introductions", "description": "Say hello", "parent": "community"},
  {"slug": "help", "name": "Help", "description": "Questions and answers", "guidance": "Say what you tried."}
]
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("users.jsonl"),
        "{\"username\":\"nora\",\"bio\":\"Backend developer.\"}\n{\"username\":\"felix\"}\n{\"username\":\"Priya\"}\n{\"username\":\"lurker\"}\n",
    )
    .unwrap();
    let mut lines = String::new();
    lines.push_str(r#"{"key":"help-chain","board":"help","author":"nora","title":"Git: undo the last commit?","body":"Is `git reset --soft HEAD~1` right?","featured":true,"replies":[{"id":1,"author":"felix","body":"Yes, it keeps your changes."},{"id":2,"author":"nora","reply_to":1,"body":"Thanks! Do I need to force-push?"},{"id":3,"author":"felix","reply_to":2,"body":"Only if you pushed it."},{"id":4,"author":"priya","body":"Or make the branch first."}]}"#);
    lines.push('\n');
    lines.push_str(r#"{"key":"intros-hello","board":"intros","author":"priya","title":"Hello from Priya","body":"I design products.","pinned":true,"replies":[]}"#);
    lines.push('\n');
    let authors = ["nora", "felix", "priya"];
    for number in 3..=THREADS {
        let author = authors[number % 3];
        let other = authors[(number + 1) % 3];
        let board = if number % 4 == 0 { "intros" } else { "help" };
        lines.push_str(&format!(
            r#"{{"key":"t-{number}","board":"{board}","author":"{author}","title":"Question number {number}","body":"Body of question {number}.","replies":[{{"id":1,"author":"{other}","body":"Answer to {number}."}},{{"id":2,"author":"{author}","reply_to":1,"body":"Thanks for answering {number}."}}]}}"#
        ));
        lines.push('\n');
    }
    std::fs::write(dir.join("threads/threads-001.jsonl"), lines).unwrap();
}

async fn count(f: &TestFixture, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(f.db.executor())
        .await
        .unwrap()
}

/// Adds up one integer column in Rust: `SUM` is `DECIMAL` on MySQL and `INTEGER` on SQLite.
async fn sum(f: &TestFixture, sql: &str) -> i64 {
    let values: Vec<i64> = db::query_scalar(sql)
        .fetch_all(f.db.executor())
        .await
        .unwrap();
    values.into_iter().sum()
}

async fn assert_consistent(f: &TestFixture) {
    let mut conn = f.db.pool.acquire().await.unwrap();
    let drift = content_drift(&mut conn).await.unwrap();
    drop(conn);
    assert!(
        gono_forum_web::test_support::sqlite_tests() || !drift.is_empty(),
        "the content check ran"
    );
    assert!(
        drift.iter().all(|(_, amount)| *amount == 0),
        "imported content keeps every content rule: {drift:?}"
    );
    assert_identity_consistent(&f.db).await;
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn app(f: &TestFixture) -> Router {
    gono_forum_web::router::build_with_global(
        f.config.clone(),
        &f.root,
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    )
}

#[tokio::test]
async fn a_dataset_is_imported_in_batches_with_consistent_copies() {
    let f = TestFixture::migrated().await.unwrap();
    // The administrator took the name of a dataset member.
    let owner = Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        detached_global(),
    )
    .setup_admin("Nora", None, PASSWORD)
    .await
    .unwrap();
    let scratch = Scratch::new();
    write_dataset(&scratch.0);
    let checked = dataset::check(&scratch.0);
    assert!(
        checked.report.errors.is_empty(),
        "{}",
        checked.report.render(20)
    );
    let data = checked.dataset.unwrap();
    let now = gono_forum_web::identity::now();
    let mut progress = Vec::new();
    let members = content::import_dataset(&f.db, &data, now, |done, total| {
        progress.push((done, total))
    })
    .await
    .unwrap();
    assert_eq!(
        progress,
        [(200, THREADS), (400, THREADS), (THREADS, THREADS)]
    );

    // Members: after the administrator, in the order they joined; the taken name got a number.
    assert_eq!(members.len(), 4);
    let mut names: Vec<&str> = members.iter().map(|m| m.username.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["felix", "lurker", "nora2", "priya"]);
    assert!(members.iter().all(|member| member.uid > owner));
    assert!(
        members
            .windows(2)
            .all(|pair| pair[0].regdate <= pair[1].regdate)
    );
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid = {owner}")
        )
        .await,
        0
    );
    // Forums: the child forum points at its parent, and its threads carry the parent as their family.
    let community: i64 = count(
        &f,
        "SELECT fid FROM gonof_forum_forum WHERE slug = 'community'",
    )
    .await;
    let intros: i64 = count(
        &f,
        "SELECT fid FROM gonof_forum_forum WHERE slug = 'intros'",
    )
    .await;
    assert_eq!(
        count(
            &f,
            "SELECT parent_fid FROM gonof_forum_forum WHERE slug = 'intros'"
        )
        .await,
        community
    );
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM gonof_forum_thread WHERE fid = {intros} AND family_fid <> {community}")
        )
        .await,
        0
    );
    // Threads and posts.
    let threads = i64::try_from(THREADS).unwrap();
    let replies = 4 + 2 * (threads - 2);
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        threads
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_post WHERE first = 0").await,
        replies
    );
    assert_eq!(
        sum(&f, "SELECT threads FROM gonof_forum_thread_count").await,
        threads
    );
    assert_eq!(
        sum(&f, "SELECT valid_replies FROM gonof_forum_thread").await,
        replies
    );
    assert_eq!(
        sum(&f, "SELECT replies FROM gonof_common_member_count").await,
        replies
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_thread WHERE subject = 'Hello from Priya' AND displayorder = 1").await,
        1
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_thread WHERE subject = 'Git: undo the last commit?' AND digest = 1").await,
        1
    );
    // The conversation three replies deep stays under its first reply.
    let chain: i64 = count(
        &f,
        "SELECT tid FROM gonof_forum_thread WHERE subject = 'Git: undo the last commit?'",
    )
    .await;
    let rows: Vec<(i64, Option<i64>, Option<i64>)> = db::query(&format!(
        "SELECT pid, root_pid, reply_to_pid FROM gonof_forum_post WHERE tid = {chain} AND first = 0 ORDER BY position"
    ))
    .fetch_all(f.db.executor())
    .await
    .unwrap()
    .iter()
    .map(|row| (row.try_get("pid").unwrap(), row.try_get("root_pid").unwrap(), row.try_get("reply_to_pid").unwrap()))
    .collect();
    let pid = |index: usize| rows[index].0;
    assert_eq!(
        rows.iter().map(|row| (row.1, row.2)).collect::<Vec<_>>(),
        [
            (None, None),
            (Some(pid(0)), Some(pid(0))),
            (Some(pid(0)), Some(pid(1))),
            (None, None)
        ]
    );
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE first = 0 AND dateline > {now}")
        )
        .await,
        0
    );
    assert_consistent(&f).await;

    // Pages: the parent forum lists its child forum's threads.
    let app = app(&f);
    let (status, body) = get(&app, "/community").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Hello from Priya"), "{body}");
    let (status, body) = get(&app, &format!("/thread-{chain}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Only if you pushed it."), "{body}");

    // A second import is refused before writing anything.
    let error = content::import_dataset(&f.db, &data, now, |_, _| {})
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only goes into a site without forums"),
        "{error}"
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_member").await,
        5
    );
    f.cleanup().await.unwrap();
}

/// Each committed example (`examples/content/<tag>/`) imports like the wizard's sample in that language.
#[tokio::test]
async fn the_committed_examples_import_like_the_wizard_samples() {
    for lang in content::sample::languages() {
        let f = TestFixture::migrated().await.unwrap();
        let example = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/content")
            .join(lang.tag());
        let checked = dataset::check(&example);
        assert!(
            checked.report.errors.is_empty(),
            "{lang}: {}",
            checked.report.render(20)
        );
        content::import_dataset(
            &f.db,
            checked.dataset.as_ref().unwrap(),
            gono_forum_web::identity::now(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM gonof_forum_thread").await,
            i64::try_from(content::sample::THREADS).unwrap()
        );
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM gonof_forum_post WHERE first = 0").await,
            i64::try_from(content::sample::REPLIES).unwrap()
        );
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM gonof_common_member").await,
            i64::try_from(content::sample::MEMBERS).unwrap()
        );
        assert_consistent(&f).await;
        f.cleanup().await.unwrap();
    }
}
