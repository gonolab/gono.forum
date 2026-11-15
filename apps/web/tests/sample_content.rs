//! The optional sample content of the setup wizard (docs/install.md, "Setup wizard"): what it writes, that the
//! copies and counters agree with the rows, that it refuses a site that already has content, and that it gives way to
//! a taken name.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use gono_forum_web::{
    content::{self, sample},
    db,
    i18n::Lang,
    identity::Identity,
    test_support::{TestFixture, assert_identity_consistent, content_drift, detached_global},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Sample-owner-password-1";

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

fn identity(f: &TestFixture) -> Identity {
    Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        detached_global(),
    )
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

/// What any language's sample writes: the promised numbers of members, forums, threads and replies, synthetic
/// authors after the administrator, posts in order, listing copies and counters that agree with the rows, and the
/// content and identity rules.
async fn assert_sample_written(f: &TestFixture, owner: i64, now: i64) {
    let members = i64::try_from(sample::MEMBERS).unwrap();
    let threads = i64::try_from(sample::THREADS).unwrap();
    let replies = i64::try_from(sample::REPLIES).unwrap();
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_common_member").await,
        members + 1
    );
    assert_eq!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_common_member WHERE password IS NULL AND email IS NULL AND mobile IS NULL AND adminid = 0 AND emailstatus = 0 AND regdate > 0 AND bio <> ''"
        )
        .await,
        members,
        "sample members are synthetic authors nobody can sign in as"
    );
    assert_eq!(
        count(
            f,
            "SELECT MIN(uid) FROM gonof_common_member WHERE adminid = 0"
        )
        .await,
        owner + 1,
        "sample members come after the administrator"
    );
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_forum_forum").await,
        i64::try_from(sample::FORUMS).unwrap()
    );
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        threads
    );
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_forum_post").await,
        threads + replies
    );
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_forum_post_tableid").await,
        threads + replies
    );
    assert_eq!(
        count(f, "SELECT COUNT(*) FROM gonof_forum_post WHERE first = 1").await,
        threads
    );
    assert_eq!(
        count(
            f,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid = {owner}")
        )
        .await,
        0,
        "the administrator wrote nothing"
    );
    // Every first post comes before its thread's replies, both in number and in time.
    assert_eq!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_forum_post p JOIN gonof_forum_post first ON first.first_tid = p.tid WHERE p.first = 0 AND (p.pid <= first.pid OR p.dateline <= first.dateline)"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            f,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE dateline > {now}")
        )
        .await,
        0,
        "nothing is dated after the import"
    );
    // Listing copies and counters.
    assert_eq!(
        sum(f, "SELECT threads FROM gonof_forum_thread_count").await,
        threads
    );
    assert_eq!(
        sum(f, "SELECT valid_replies FROM gonof_forum_thread").await,
        replies
    );
    assert_eq!(
        sum(f, "SELECT threads FROM gonof_common_member_count").await,
        threads
    );
    assert_eq!(
        sum(f, "SELECT replies FROM gonof_common_member_count").await,
        replies
    );
    assert_eq!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_forum_thread WHERE last_reply_pid IS NULL OR lastpost < dateline OR author = ''"
        )
        .await,
        0
    );
    assert!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_forum_thread WHERE displayorder > 0"
        )
        .await
            >= 1
    );
    assert!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_forum_thread WHERE digest > 0"
        )
        .await
            >= 1
    );
    assert!(
        count(
            f,
            "SELECT COUNT(*) FROM gonof_forum_post WHERE root_pid IS NOT NULL"
        )
        .await
            > 0,
        "some replies answer other replies"
    );
    // The SQL consistency checks (they run on MySQL only).
    let mut conn = f.db.pool.acquire().await.unwrap();
    let drift = content_drift(&mut conn).await.unwrap();
    drop(conn);
    assert!(
        gono_forum_web::test_support::sqlite_tests() || !drift.is_empty(),
        "the content check ran"
    );
    assert!(
        drift.iter().all(|(_, amount)| *amount == 0),
        "sample content keeps every content rule: {drift:?}"
    );
    assert_identity_consistent(&f.db).await;
}

#[tokio::test]
async fn sample_content_fills_an_empty_site_consistently() {
    let f = TestFixture::migrated().await.unwrap();
    let owner = identity(&f)
        .setup_admin("owner", None, PASSWORD)
        .await
        .unwrap();
    assert!(content::sample_fits(&f.db).await.unwrap());
    let now = gono_forum_web::identity::now();
    content::import_sample(&f.db, now, Lang::EN).await.unwrap();

    assert_sample_written(&f, owner, now).await;
    let members = i64::try_from(sample::MEMBERS).unwrap();
    let threads = i64::try_from(sample::THREADS).unwrap();

    // Pages show it.
    let app = gono_forum_web::router::build_with_global(
        f.config.clone(),
        &f.root,
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    );
    let (status, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Welcome! A note about the sample content"),
        "{body}"
    );
    let (status, body) = get(&app, "/help").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("How to ask a good question here"), "{body}");
    let (status, body) = get(&app, "/thread-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("sample content"), "{body}");

    // A second import is refused and changes nothing.
    assert!(!content::sample_fits(&f.db).await.unwrap());
    let error = content::import_sample(&f.db, now, Lang::EN)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sample content only goes into a site without"),
        "{error}"
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_forum_thread").await,
        threads
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_member").await,
        members + 1
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn sample_content_refuses_a_site_with_content_and_works_around_taken_names() {
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    assert!(
        !content::sample_fits(&f.db).await.unwrap(),
        "forums alone are enough to refuse"
    );
    assert!(
        content::import_sample(&f.db, 1_800_000_000, Lang::EN)
            .await
            .is_err()
    );
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM gonof_common_member").await,
        0
    );
    db::query("DELETE FROM gonof_forum_forum")
        .execute(f.db.executor())
        .await
        .unwrap();

    // The administrator took the name of the first sample member.
    let owner = identity(&f)
        .setup_admin("Nora", None, PASSWORD)
        .await
        .unwrap();
    content::import_sample(&f.db, 1_800_000_000, Lang::EN)
        .await
        .unwrap();
    let names: Vec<String> = db::query_scalar(
        "SELECT username FROM gonof_common_member WHERE username LIKE 'nora%' ORDER BY uid",
    )
    .fetch_all(f.db.executor())
    .await
    .unwrap();
    assert_eq!(names, ["nora", "nora2"]);
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid = {owner}")
        )
        .await,
        0,
        "the sample posts of nora go to nora2, not to the administrator"
    );
    assert!(
        count(
            &f,
            "SELECT COUNT(*) FROM gonof_forum_post p JOIN gonof_common_member m ON m.uid = p.authorid WHERE m.username = 'nora2'"
        )
        .await
            > 0
    );
    f.cleanup().await.unwrap();
}

/// Every other language with a sample writes the same structure with its own text, and the pages show it.
#[tokio::test]
async fn translated_sample_content_fills_an_empty_site_consistently() {
    for lang in sample::languages().filter(|lang| *lang != Lang::EN) {
        let f = TestFixture::migrated().await.unwrap();
        let owner = identity(&f)
            .setup_admin("owner", None, PASSWORD)
            .await
            .unwrap();
        let now = gono_forum_web::identity::now();
        content::import_sample(&f.db, now, lang).await.unwrap();
        assert_sample_written(&f, owner, now).await;

        // The text is the translation's, not the base's (the base sample is plain ASCII).
        let translated = |text: &str| !text.is_ascii();
        let subjects: Vec<String> =
            db::query_scalar("SELECT subject FROM gonof_forum_thread ORDER BY tid")
                .fetch_all(f.db.executor())
                .await
                .unwrap();
        assert!(
            subjects.iter().all(|subject| translated(subject)),
            "{lang}: {subjects:?}"
        );
        let forums: Vec<String> =
            db::query_scalar("SELECT name FROM gonof_forum_forum ORDER BY fid")
                .fetch_all(f.db.executor())
                .await
                .unwrap();
        assert!(
            forums.iter().all(|name| translated(name)),
            "{lang}: {forums:?}"
        );
        let pinned: String = db::query_scalar(
            "SELECT subject FROM gonof_forum_thread WHERE displayorder > 0 ORDER BY tid LIMIT 1",
        )
        .fetch_one(f.db.executor())
        .await
        .unwrap();
        let author: String = db::query_scalar(
            "SELECT m.username FROM gonof_forum_thread t JOIN gonof_common_member m ON m.uid = t.authorid WHERE t.tid = 1",
        )
        .fetch_one(f.db.executor())
        .await
        .unwrap();

        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        let (status, body) = get(&app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&pinned), "{lang}: {body}");
        let (status, body) = get(&app, "/help").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&forums[1]), "{lang}: {body}");
        let (status, body) = get(&app, "/thread-1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&subjects[0]) && body.contains(&author),
            "{lang}: {body}"
        );
        f.cleanup().await.unwrap();
    }
}
