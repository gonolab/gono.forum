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

/// Runs `gono-forum-cli` on the fixture's database, as an operator would.
fn cli(f: &TestFixture, args: &[&str]) -> std::process::Output {
    let config = f.root.join("cli.toml");
    std::fs::write(&config, "[redis]\nenabled = false\n").unwrap();
    std::process::Command::new(env!("CARGO_BIN_EXE_gono-forum-cli"))
        .args(args)
        .env("GONOF_HOME", &f.root)
        .env("GONOF_CONFIG", &config)
        .env("GONOF_DATABASE_URL", &f.config.database.url)
        .env("GONOF_AUTH_COOKIE_KEY", "11".repeat(32))
        .env("GONOF_TWO_FACTOR_KEY", "22".repeat(32))
        .env_remove("GONOF_AUTH_COOKIE_PREVIOUS_KEYS")
        .env_remove("GONOF_TWO_FACTOR_PREVIOUS_KEYS")
        .output()
        .unwrap()
}

/// `--members-out` is created before anything is imported: a path that cannot be written stops the command while the
/// site is still empty, so the import can be run again, and an import that fails removes the file again.
#[tokio::test]
async fn the_member_list_is_created_before_the_import_and_removed_when_it_fails() {
    let f = TestFixture::migrated().await.unwrap();
    let scratch = Scratch::new();
    write_dataset(&scratch.0);
    let dir = scratch.0.to_str().unwrap();
    let missing = f.root.join("missing/members.json");
    let output = cli(
        &f,
        &[
            "import-content",
            dir,
            &format!("--members-out={}", missing.display()),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("cannot create the output file"), "{stderr}");
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_forum").await,
        0,
        "nothing is imported when the member list cannot be created"
    );

    let members = f.root.join("members.json");
    let output = cli(
        &f,
        &[
            "import-content",
            dir,
            &format!("--members-out={}", members.display()),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let list: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&members).unwrap()).unwrap();
    assert_eq!(list["roles"].as_array().unwrap().len(), 4);

    // The site now has boards, so a second import fails and leaves no member list behind.
    let again = f.root.join("again.json");
    let output = cli(
        &f,
        &[
            "import-content",
            dir,
            &format!("--members-out={}", again.display()),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("only goes into a site without forums"),
        "{stderr}"
    );
    assert!(!again.exists(), "a failed import removes the member list");
    f.cleanup().await.unwrap();
}

/// A small dataset: one board, the given members, and one thread per member with one reply by the next member.
fn write_small_dataset(dir: &Path, board: &str, members: &[String]) {
    std::fs::write(
        dir.join("manifest.json"),
        "{\"format\": \"gono-forum-content\", \"version\": 1, \"language\": \"en\"}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("boards.json"),
        format!(
            r#"[{{"slug": "{board}", "name": "Board {board}", "description": "About {board}"}}]"#
        ),
    )
    .unwrap();
    let users: String = members
        .iter()
        .map(|name| format!("{{\"username\":\"{name}\"}}\n"))
        .collect();
    std::fs::write(dir.join("users.jsonl"), users).unwrap();
    let threads: String = members
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let other = &members[(index + 1) % members.len()];
            format!(
                "{{\"key\":\"{board}-{index}\",\"board\":\"{board}\",\"author\":\"{name}\",\"title\":\"Thread {index}\",\"body\":\"Body {index}.\",\"replies\":[{{\"id\":1,\"author\":\"{other}\",\"body\":\"Reply {index}.\"}}]}}\n"
            )
        })
        .collect();
    std::fs::write(dir.join("threads/threads-001.jsonl"), threads).unwrap();
}

fn checked(dir: &Path) -> dataset::Dataset {
    let checked = dataset::check(dir);
    assert!(
        checked.report.errors.is_empty(),
        "{}",
        checked.report.render(20)
    );
    checked.dataset.unwrap()
}

/// Two imports into the same empty site go one at a time: the second finds the first one's boards and writes
/// nothing, instead of both committing their boards and members and one of them then failing on thread 1.
#[tokio::test]
async fn concurrent_imports_leave_only_the_one_that_went_first() {
    let f = TestFixture::migrated().await.unwrap();
    let (first, second) = (Scratch::new(), Scratch::new());
    let names =
        |prefix: &str| -> Vec<String> { (0..300).map(|n| format!("{prefix}{n}")).collect() };
    write_small_dataset(&first.0, "alpha", &names("alpha-"));
    write_small_dataset(&second.0, "beta", &names("beta-"));
    let (first, second) = (checked(&first.0), checked(&second.0));
    let now = gono_forum_web::identity::now();
    let (a, b) = tokio::join!(
        content::import_dataset(&f.db, &first, now, |_, _| {}),
        content::import_dataset(&f.db, &second, now, |_, _| {}),
    );
    let (winner, error) = match (a, b) {
        (Ok(_), Err(error)) => ("alpha", error),
        (Err(error), Ok(_)) => ("beta", error),
        (a, b) => panic!(
            "exactly one import goes in: {:?} {:?}",
            a.map(|m| m.len()),
            b.map(|m| m.len())
        ),
    };
    assert!(
        error
            .to_string()
            .contains("only goes into a site without forums"),
        "{error:#}"
    );
    let slugs: Vec<String> = db::query_scalar("SELECT slug FROM gonof_forum_forum")
        .fetch_all(f.db.executor())
        .await
        .unwrap();
    assert_eq!(slugs, [winner], "the refused import left no board behind");
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_member").await,
        300,
        "the refused import left no member behind"
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        300
    );
    assert_consistent(&f).await;
    f.cleanup().await.unwrap();
}

/// A valid 50-character name that the site already uses gets a number within the 50-character limit, instead of
/// stopping the import.
#[tokio::test]
async fn a_taken_name_at_the_length_limit_is_shortened_for_its_number() {
    let f = TestFixture::migrated().await.unwrap();
    let long = "n".repeat(50);
    Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        detached_global(),
    )
    .setup_admin(&long, None, PASSWORD)
    .await
    .unwrap();
    let scratch = Scratch::new();
    write_small_dataset(&scratch.0, "help", &[long.clone(), "felix".to_owned()]);
    let members = content::import_dataset(
        &f.db,
        &checked(&scratch.0),
        gono_forum_web::identity::now(),
        |_, _| {},
    )
    .await
    .unwrap();
    let mut names: Vec<&str> = members.iter().map(|m| m.username.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["felix", &format!("{}2", "n".repeat(49))]);
    assert_consistent(&f).await;
    f.cleanup().await.unwrap();
}

/// Imported posts earn no growth points for being imported, but the likes and highlights they get afterwards count
/// as usual (D6); the import's own highlight never does.
#[tokio::test]
async fn imported_posts_earn_growth_only_for_what_they_receive_afterwards() {
    use gono_forum_web::{
        content::{
            edits::{ChangeRequest, ContentAction, ContentEditor},
            writes::WriteConfig,
        },
        growth,
        identity::Actor,
        interaction::Interaction,
    };
    let f = TestFixture::migrated().await.unwrap();
    let owner = Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        detached_global(),
    )
    .setup_admin("Owner", None, PASSWORD)
    .await
    .unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(9001,'reader','synthetic-hash','reader@example.test',1,1)")
        .execute(f.db.executor())
        .await
        .unwrap();
    let scratch = Scratch::new();
    write_dataset(&scratch.0);
    let now = gono_forum_web::identity::now();
    content::import_dataset(&f.db, &checked(&scratch.0), now, |_, _| {})
        .await
        .unwrap();
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_growth_post").await,
        count(&f, "SELECT COUNT(*) FROM gonof_forum_post").await,
        "every imported post has a growth record"
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_growth_ledger").await,
        0,
        "the import itself earns nothing"
    );
    let points = gono_forum_web::runtime::read(&f.db).await.unwrap().growth;
    let uid = |name: &'static str| {
        let f = &f;
        async move {
            count(
                f,
                &format!("SELECT uid FROM gonof_common_member WHERE username = '{name}'"),
            )
            .await
        }
    };
    let actor = |uid: i64| Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    };

    // A real like on an imported reply.
    let reply = count(
        &f,
        "SELECT pid FROM gonof_forum_post WHERE message = 'Yes, it keeps your changes.'",
    )
    .await;
    Interaction::new(f.db.clone())
        .set_reaction(
            actor(9001),
            reply,
            true,
            &gono_forum_web::identity::random_token(),
        )
        .await
        .unwrap();
    assert_eq!(
        growth::growth(&f.db, uid("felix").await).await.unwrap(),
        points.like
    );

    // A highlight a moderator gives counts; the highlight the dataset brought does not, even when given again.
    let editor = ContentEditor::new(f.db.clone(), WriteConfig::default(), 86_400);
    let change = |pid: i64, action: ContentAction| {
        let editor = &editor;
        async move {
            let post = editor.editable_at(actor(owner), pid, now).await.unwrap();
            editor
                .change_at(
                    actor(owner),
                    ChangeRequest {
                        pid,
                        expected_post_version: post.version,
                        expected_thread_version: post.thread_version,
                        request_key: gono_forum_web::identity::random_token(),
                        reason: "growth after import".into(),
                        action,
                    },
                    now,
                )
                .await
                .unwrap();
        }
    };
    let opening = |subject: &'static str| {
        let f = &f;
        async move {
            count(
                f,
                &format!("SELECT p.pid FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid WHERE p.first = 1 AND t.subject = '{subject}'"),
            )
            .await
        }
    };
    change(opening("Question number 3").await, ContentAction::Feature).await;
    let nora = uid("nora").await;
    assert_eq!(growth::growth(&f.db, nora).await.unwrap(), points.digest);
    let imported_feature = opening("Git: undo the last commit?").await;
    change(imported_feature, ContentAction::Unfeature).await;
    change(imported_feature, ContentAction::Feature).await;
    assert_eq!(growth::growth(&f.db, nora).await.unwrap(), points.digest);

    let reconciled = growth::reconcile(&f.db).await.unwrap();
    assert!(reconciled.is_consistent(), "{reconciled:?}");
    assert_consistent(&f).await;
    f.cleanup().await.unwrap();
}
