#[path = "support/debug.rs"]
mod debug_contract;
mod support;

use axum::http::StatusCode;
use gono_forum_web::{content, db::Database, project_root, router, urls::Sort};
use std::time::Duration;

/// Change every thread's title (built in code and written back, the same statement on both backends) and return
/// the original titles.
async fn retitle(db: &Database, title: impl Fn(&str) -> String) -> Vec<(i64, String)> {
    let rows =
        gono_forum_web::db::query("SELECT tid, subject FROM gonof_forum_thread ORDER BY tid")
            .fetch_all(db.executor())
            .await
            .unwrap();
    let mut original = Vec::new();
    for row in rows {
        let tid: i64 = row.try_get("tid").unwrap();
        let subject = gono_forum_web::db::text(&row, "subject").unwrap();
        gono_forum_web::db::query("UPDATE gonof_forum_thread SET subject = ? WHERE tid = ?")
            .bind(title(&subject))
            .bind(tid)
            .execute(db.executor())
            .await
            .unwrap();
        original.push((tid, subject));
    }
    original
}

async fn execute(db: &Database, statement: &str) {
    gono_forum_web::db::query(statement)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn reject(db: &Database, statement: &str) {
    assert!(
        gono_forum_web::db::query(statement)
            .execute(&db.pool)
            .await
            .is_err(),
        "a write that breaks a constraint must be rejected: {statement}"
    );
}

/// Such values and shapes are not rejected by the database but guaranteed by how the writing code builds them
/// (`PostShape`, `forum_fields`, source scans). Verified in a rolled-back transaction: the database either rejects it
/// (foreign key, unique key), or the check script's "{item}" detects it.
async fn rejected_or_flagged(db: &Database, statement: &str, item: &str) {
    let mut tx = db.pool.begin().await.unwrap();
    // The check script only runs on MySQL: on SQLite only the half rejected by the database is verified.
    if gono_forum_web::db::query(statement)
        .execute(&mut *tx)
        .await
        .is_ok()
        && !gono_forum_web::test_support::sqlite_tests()
    {
        let drift = gono_forum_web::test_support::content_drift(&mut tx)
            .await
            .unwrap();
        assert!(
            drift
                .iter()
                .any(|(name, amount)| name == item && *amount > 0),
            "{statement} must be found by the check script item \"{item}\": {drift:?}"
        );
    }
    tx.rollback().await.unwrap();
}

/// After raw SQL changed a thread or post, sync the copies and invalidate caches as the edit and moderation entry
/// points do (the copies are maintained only by code).
async fn thread_changed(db: &Database, tid: i64) {
    gono_forum_web::test_support::resync_thread(db, tid)
        .await
        .unwrap();
}

/// After raw SQL changed a forum, rewrite the forum copies on threads as saving a forum in the admin area does.
async fn forum_changed(db: &Database, slug: &str) {
    let fid: i64 =
        gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug=?")
            .bind(slug)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    gono_forum_web::test_support::sync_forum_copies(db, fid)
        .await
        .unwrap();
}

async fn count(db: &Database, table: &str) -> i64 {
    gono_forum_web::db::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn ids(db: &Database, slug: &str, sort: Sort, search: Option<&str>) -> Vec<u64> {
    content::list_threads(db, slug, sort, search, 1, 100)
        .await
        .unwrap()
        .threads
        .into_iter()
        .map(|thread| thread.id)
        .collect()
}

// The three entry points share this behavior contract; the server database must be a separate empty database.
async fn database_contract(db: Database) {
    let config = support::config_for(&db);
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    // The English sample site the assertions were written for (docs/translating.md, "For developers: adding
    // interface text").
    gono_forum_web::test_support::restore_sample_site(&db)
        .await
        .unwrap();
    assert_migration_failure_releases_lock(&db).await;
    for table in [
        "gonof_common_member",
        "gonof_forum_forum",
        "gonof_forum_thread",
        "gonof_forum_post_tableid",
        "gonof_forum_post",
    ] {
        assert_eq!(
            count(&db, table).await,
            0,
            "needs a separate empty test database: {table}"
        );
    }

    content::seed_forums(&db).await.unwrap();
    content::seed_forums(&db).await.unwrap();
    let forums = content::forums(&db).await.unwrap();
    assert_eq!(
        forums
            .iter()
            .map(|forum| forum.slug.as_str())
            .collect::<Vec<_>>(),
        ["daily", "showcase", "ai", "hosting", "releases", "meta"]
    );
    execute(&db, "UPDATE gonof_forum_forum SET name = 'Daily from the database', displayorder = 99 WHERE slug = 'daily'").await;
    content::seed_forums(&db).await.unwrap();
    let forums = content::forums(&db).await.unwrap();
    assert_eq!(forums.last().unwrap().slug, "daily");
    assert_eq!(forums.last().unwrap().name, "Daily from the database");
    // After the slug was changed in the admin area and the server restarted, no forum may be created again from the
    // original slug in the config.
    execute(
        &db,
        "UPDATE gonof_forum_forum SET slug = 'daily-life' WHERE slug = 'daily'",
    )
    .await;
    content::seed_forums(&db).await.unwrap();
    assert_eq!(count(&db, "gonof_forum_forum").await, 6);
    assert_eq!(
        count(&db, "gonof_forum_forum WHERE slug = 'daily'").await,
        0
    );
    execute(
        &db,
        "UPDATE gonof_forum_forum SET slug = 'daily' WHERE slug = 'daily-life'",
    )
    .await;

    // The deployment config no longer has a site URL: the page's canonical takes the forum URL, and test requests have
    // no Host, so fill it in first.
    gono_forum_web::test_support::set_site_url(&db, gono_forum_web::test_support::TEST_SITE_URL)
        .await
        .unwrap();
    let empty_app = router::build(config.clone(), &project_root(), db.clone());
    let (status, _, empty) = support::fetch(&empty_app, "/", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.contains("No threads match."));
    assert!(empty.contains("Daily from the database"));
    assert_eq!(
        support::fetch(&empty_app, "/daily", false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        support::fetch(&empty_app, "/page-2", false).await.0,
        StatusCode::NOT_FOUND
    );
    execute(
        &db,
        "UPDATE gonof_forum_forum SET name = 'Daily', displayorder = 0 WHERE slug = 'daily'",
    )
    .await;

    let mut invalid = content::fixtures();
    invalid
        .last_mut()
        .unwrap()
        .posts
        .last_mut()
        .unwrap()
        .root_pid = Some(103002);
    assert!(content::import_fixture(&db, &invalid).await.is_err());
    // The fixture writes primary keys explicitly and the connection has NO_AUTO_VALUE_ON_ZERO on, so id 0 would be
    // stored as is; the database has no "ID is positive" check, so only the import code's own check stops it, and
    // the error text proves that code, not the database, stopped it.
    let mut zero_thread = content::fixtures();
    zero_thread[0].id = 0;
    let error = content::import_fixture(&db, &zero_thread)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("development fixture thread ids must be positive"),
        "{error}"
    );
    let mut zero_post = content::fixtures();
    zero_post[0].posts[0].pid = 0;
    let error = content::import_fixture(&db, &zero_post)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("development fixture post ids must be positive"),
        "{error}"
    );
    for table in [
        "gonof_common_member",
        "gonof_forum_thread",
        "gonof_forum_post",
        "gonof_forum_post_tableid",
    ] {
        assert_eq!(
            count(&db, table).await,
            0,
            "a failed fixture import must roll back completely: {table}"
        );
    }
    content::seed_fixture(&db).await.unwrap();
    assert!(content::seed_fixture(&db).await.is_err());
    assert_eq!(count(&db, "gonof_forum_thread").await, 24);
    assert_eq!(count(&db, "gonof_forum_post").await, 240);
    assert_eq!(count(&db, "gonof_forum_post_tableid").await, 240);
    db.migrate().await.unwrap();
    assert_eq!(count(&db, "gonof_forum_thread").await, 24);

    assert_reading(&db).await;
    assert_http(&db).await;
    debug_contract::assert_debug_contract(&db).await;
    assert_large_unicode_text(&db).await;
    assert_sort_ties(&db).await;
    assert_search_and_visibility(&db).await;
    assert_transactions(&db).await;
    assert_constraints(&db).await;
    let disconnected_app = router::build(config, &project_root(), db.clone());
    db.pool.close().await;
    let (status, _, body) = support::fetch(&disconnected_app, "/", false).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    for private in [
        "SELECT ",
        "gonof_forum_",
        "sqlite:",
        "mysql://",
        "postgres://",
        "href=\"/daily\"",
    ] {
        assert!(
            !body.contains(private),
            "a database failure response must not leak internals or fall back to the configured navigation: {private}"
        );
    }
    debug_contract::assert_disconnected_debug_page(&db).await;
}

async fn assert_large_unicode_text(db: &Database) {
    let before = gono_forum_web::test_support::thread_with_all_pages(db, 103)
        .await
        .unwrap()
        .unwrap();
    let original = before
        .posts
        .iter()
        .find(|post| post.pid == 103003)
        .unwrap()
        .body
        .clone();
    // "\u{91d1}\u{5e01}" is the Chinese for "gold coins": three-byte characters next to a four-byte emoji, so the
    // text is over 65,535 bytes while far shorter in characters.
    let unit = "\u{91d1}\u{5e01}🍓";
    let message = format!("{}<script>alert('long text')</script>", unit.repeat(7000));
    assert!(message.len() > 65_535);
    let update = "UPDATE gonof_forum_post SET message = ? WHERE pid = ?";
    gono_forum_web::db::query(update)
        .bind(&message)
        .bind(103003_i64)
        .execute(&db.pool)
        .await
        .unwrap();
    thread_changed(db, 103).await;
    let after = gono_forum_web::test_support::thread_with_all_pages(db, 103)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after
            .posts
            .iter()
            .find(|post| post.pid == 103003)
            .unwrap()
            .body,
        message
    );
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    let (status, _, body) = support::fetch(&app, "/thread-103", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&unit.repeat(7000)));
    assert!(
        !body.contains("alert('long text')"),
        "body sanitizing must remove the script and its content"
    );
    gono_forum_web::db::query(update)
        .bind(original)
        .bind(103003_i64)
        .execute(&db.pool)
        .await
        .unwrap();
    thread_changed(db, 103).await;
}

async fn assert_migration_failure_releases_lock(db: &Database) {
    let original: Vec<u8> =
        gono_forum_web::db::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let update = "UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1";
    gono_forum_web::db::query(update)
        .bind(vec![0_u8; original.len()])
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(db.migrate().await.is_err());
    gono_forum_web::db::query(update)
        .bind(original)
        .execute(&db.pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), db.migrate())
        .await
        .expect("a failed migration must release the migration lock")
        .unwrap();
}

async fn assert_reading(db: &Database) {
    assert_eq!(
        ids(db, "", Sort::Latest, None).await,
        (101..=124).collect::<Vec<_>>()
    );
    assert_eq!(
        ids(db, "", Sort::Created, None).await,
        [102, 101]
            .into_iter()
            .chain((103..=124).rev())
            .collect::<Vec<_>>()
    );
    assert_eq!(ids(db, "", Sort::Pinned, None).await, [101, 102]);
    assert_eq!(ids(db, "", Sort::Featured, None).await, [103, 107, 116]);
    assert_eq!(
        ids(db, "ai", Sort::Latest, None).await,
        [104, 107, 113, 118, 122]
    );
    assert!(ids(db, "missing", Sort::Latest, None).await.is_empty());
    let first = content::list_threads(db, "", Sort::Latest, None, 1, 20)
        .await
        .unwrap();
    let second = content::list_threads(db, "", Sort::Latest, None, 2, 20)
        .await
        .unwrap();
    assert_eq!(first.total, 24);
    assert_eq!(second.total, 24);
    assert_eq!(
        first
            .threads
            .iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>(),
        (101..=120).collect::<Vec<_>>()
    );
    assert_eq!(
        second
            .threads
            .iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>(),
        [121, 122, 123, 124]
    );
    assert!(
        content::list_threads(db, "", Sort::Latest, None, 3, 20)
            .await
            .unwrap()
            .threads
            .is_empty()
    );
    let thread = gono_forum_web::test_support::thread_with_all_pages(db, 103)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        thread.title,
        "I built a lightweight RSS reader: notes on how development went"
    );
    // The fixture import stores usernames normalized (NFKC, lowercase): "Southwind" in the fixture is "southwind".
    assert_eq!(thread.author, "southwind");
    assert_eq!(thread.body.len(), 1);
    assert!(
        thread.body[0].contains("\n\n"),
        "the first post keeps its whole Markdown paragraphs"
    );
    assert_eq!(thread.posts.len(), 78);
    assert_eq!(thread.reply_count, 78);
    assert_eq!(
        thread
            .posts
            .iter()
            .map(|post| post.position)
            .collect::<Vec<_>>(),
        (2..=79).collect::<Vec<_>>()
    );
    assert_eq!(
        thread
            .posts
            .iter()
            .filter(|post| post.root_pid.is_none())
            .count(),
        54
    );
    assert!(
        gono_forum_web::test_support::thread_with_all_pages(db, 999)
            .await
            .unwrap()
            .is_none()
    );
    let overview =
        content::sidebar_at(db, 1788864000, gono_forum_web::test_support::SEEDED_SIDEBAR)
            .await
            .unwrap();
    assert_eq!(overview.total, 24);
    assert_eq!(overview.replies, 216);
    assert_eq!(overview.contributors, 6);
    // Hot by reply count over the last 7 days: 103 leads with 77 replies, the rest all have 5, by tid descending.
    assert_eq!(
        overview
            .hot
            .iter()
            .map(|t| t.id)
            .take(3)
            .collect::<Vec<_>>(),
        [103, 124, 123]
    );
    let week_later =
        content::sidebar_at(db, 1789468800, gono_forum_web::test_support::SEEDED_SIDEBAR)
            .await
            .unwrap();
    assert_eq!(week_later.contributors, 0);
    assert!(
        week_later.hot.is_empty(),
        "no new replies within 7 days means not hot"
    );
}

async fn assert_http(db: &Database) {
    let app = router::build(support::config_for(db), &project_root(), db.clone());
    for htmx in [false, true] {
        for path in [
            "/",
            "/?sort=created",
            "/?sort=pinned",
            "/?sort=featured",
            "/ai",
            "/page-2",
            "/thread-103",
        ] {
            let (status, headers, body) = support::fetch(&app, path, htmx).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(body.starts_with("<!doctype html>"));
            assert!(body.contains("Sample Forum"));
            assert_eq!(headers["vary"], "HX-Request");
        }
    }
    let (_, _, tree) = support::fetch(&app, "/thread-103", false).await;
    assert!(tree.contains("id=\"floor-75\""));
    assert!(!tree.contains("id=\"floor-76\""));
    let (_, _, sub_page) =
        support::fetch(&app, "/thread-103?sub_root=103002&sub_page=2", false).await;
    assert!(sub_page.contains("id=\"floor-76\""));
    let (_, _, flat) = support::fetch(&app, "/thread-103-2?view=flat", false).await;
    assert!(flat.contains("id=\"floor-52\""));
    assert!(flat.contains("In reply #2"));
    assert!(flat.contains("/thread-103?sub_root=103002&#38;sub_page=2#floor-76"));

    // Search only matches thread titles; set a common title word for the pagination contract, independent of reply
    // bodies.
    let subjects = retitle(db, |subject| format!("Paged search check {subject}")).await;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", "Paged search check")
        .finish();
    let (status, _, search) =
        support::fetch(&app, &format!("/search?{query}&sort=created"), false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search.matches("class=\"row-title\"").count(), 20);
    assert!(search.contains("sort=created&#38;page=2"));
    let (status, _, second) =
        support::fetch(&app, &format!("/search?{query}&sort=created&page=2"), false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second.matches("class=\"row-title\"").count(), 4);
    assert_eq!(
        support::fetch(&app, &format!("/search?{query}&page=3"), false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for (tid, subject) in subjects {
        gono_forum_web::db::query("UPDATE gonof_forum_thread SET subject = ? WHERE tid = ?")
            .bind(subject)
            .bind(tid)
            .execute(db.executor())
            .await
            .unwrap();
    }

    execute(
        db,
        "UPDATE gonof_forum_thread SET subject = 'Title changed in the database' WHERE tid = 101",
    )
    .await;
    // Raw SQL no longer increments any version; the title edit entry point increments the site-wide version, so the
    // same code invalidates caches here.
    thread_changed(db, 101).await;
    assert!(
        support::fetch(&app, "/", false)
            .await
            .2
            .contains("Title changed in the database")
    );
    assert!(
        support::fetch(&app, "/meta", false)
            .await
            .2
            .contains("Title changed in the database")
    );
    assert!(
        support::fetch(&app, "/thread-101", false)
            .await
            .2
            .contains("Title changed in the database")
    );
}

async fn assert_sort_ties(db: &Database) {
    execute(
        db,
        "UPDATE gonof_forum_thread SET lastpost = 1788832800, dateline = 1788825600",
    )
    .await;
    let latest = ids(db, "", Sort::Latest, None).await;
    assert_eq!(&latest[..2], &[102, 101]);
    assert_eq!(&latest[2..], &(103..=124).rev().collect::<Vec<_>>());
    assert_eq!(ids(db, "", Sort::Created, None).await, latest);
    assert_eq!(ids(db, "", Sort::Pinned, None).await, [102, 101]);
    assert_eq!(ids(db, "", Sort::Featured, None).await, [116, 107, 103]);
    let first = content::list_threads(db, "", Sort::Latest, None, 1, 20)
        .await
        .unwrap();
    let second = content::list_threads(db, "", Sort::Latest, None, 2, 20)
        .await
        .unwrap();
    let paged: Vec<_> = first
        .threads
        .iter()
        .chain(&second.threads)
        .map(|thread| thread.id)
        .collect();
    assert_eq!(paged, latest);
}

async fn assert_search_and_visibility(db: &Database) {
    execute(db, "UPDATE gonof_forum_post SET message = 'Only the body has needleleaf %_ ! MiXcAsEz' WHERE tid = 101 AND first = 1").await;
    execute(
        db,
        "UPDATE gonof_forum_post SET message = 'Only a reply has ginkgo' WHERE pid = 103002",
    )
    .await;
    assert!(
        ids(db, "", Sort::Latest, Some("needleleaf"))
            .await
            .is_empty(),
        "post bodies are not searched"
    );
    execute(
        db,
        "UPDATE gonof_forum_thread SET subject='Only the title has needleleaf %_ ! MiXcAsEz Ärger ΣΟΦΙΑ' WHERE tid=101",
    )
    .await;
    // Non-ASCII case must match both ways too: the old code only lowercased ASCII, so "ÄRGER" became "Ärger".
    for query in [
        "needleleaf",
        "%",
        "_",
        "%_",
        "!",
        "mixcasez",
        "MIXCASEZ",
        "ärger",
        "ÄRGER",
        "σοφια",
        "ΣΟΦΙΑ",
    ] {
        // The fixture title of thread 124 ends a sentence with "!" too.
        let expected: &[u64] = if query == "!" { &[101, 124] } else { &[101] };
        assert_eq!(
            ids(db, "", Sort::Latest, Some(query)).await,
            expected,
            "{query}"
        );
    }
    assert!(
        ids(db, "", Sort::Latest, Some("ginkgo")).await.is_empty(),
        "reply bodies are not searched"
    );
    assert!(
        ids(db, "ai", Sort::Latest, Some("needleleaf"))
            .await
            .is_empty()
    );
    assert!(
        ids(db, "", Sort::Latest, Some("' OR 1=1 --"))
            .await
            .is_empty()
    );

    for (column, value, restore) in [
        ("displayorder", "-1", "0"),
        ("displayorder", "-2", "0"),
        ("visibility", "'private'", "'public'"),
        ("visibility", "'members'", "'public'"),
        ("publication_state", "'draft'", "'published'"),
        ("publication_state", "'scheduled'", "'published'"),
        ("readperm", "1", "0"),
    ] {
        execute(
            db,
            &format!("UPDATE gonof_forum_thread SET {column} = {value} WHERE tid = 103"),
        )
        .await;
        // Raw SQL changed the readability columns: recompute the unlisted pin level and counts by the listing rules,
        // the same effect as going through the moderation and edit entry points.
        // Writing a negative pin level directly already means "not listed" and is not recomputed (recomputing would
        // set it back to 0 based on the first post).
        if column != "displayorder" {
            gono_forum_web::test_support::refresh_listing(db)
                .await
                .unwrap();
        }
        assert!(
            gono_forum_web::test_support::thread_with_all_pages(db, 103)
                .await
                .unwrap()
                .is_none(),
            "{column}={value}"
        );
        assert!(!ids(db, "", Sort::Latest, None).await.contains(&103));
        assert!(ids(db, "", Sort::Latest, Some("ginkgo")).await.is_empty());
        execute(
            db,
            &format!("UPDATE gonof_forum_thread SET {column} = {restore} WHERE tid = 103"),
        )
        .await;
        gono_forum_web::test_support::refresh_listing(db)
            .await
            .unwrap();
    }
    execute(
        db,
        "UPDATE gonof_forum_thread SET deleted_at = 1 WHERE tid = 103",
    )
    .await;
    let withdrawn = gono_forum_web::test_support::thread_with_all_pages(db, 103)
        .await
        .unwrap()
        .unwrap();
    assert!(withdrawn.withdrawn);
    assert_eq!(withdrawn.title, "Thread withdrawn");
    assert!(withdrawn.body.iter().all(String::is_empty));
    assert_eq!(withdrawn.posts.len(), 78);
    assert!(ids(db, "", Sort::Latest, None).await.contains(&103));
    assert!(
        ids(db, "", Sort::Latest, Some("ginkgo")).await.is_empty(),
        "reply bodies are not searched"
    );
    execute(
        db,
        "UPDATE gonof_forum_thread SET deleted_at = 0 WHERE tid = 103",
    )
    .await;
    execute(
        db,
        "UPDATE gonof_forum_thread SET closed = 1 WHERE tid = 103",
    )
    .await;
    assert!(
        gono_forum_web::test_support::thread_with_all_pages(db, 103)
            .await
            .unwrap()
            .is_some()
    );
    execute(
        db,
        "UPDATE gonof_forum_post SET invisible = -2 WHERE tid = 103 AND first = 1",
    )
    .await;
    thread_changed(db, 103).await;
    assert!(
        gono_forum_web::test_support::thread_with_all_pages(db, 103)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!ids(db, "", Sort::Latest, None).await.contains(&103));
    execute(
        db,
        "UPDATE gonof_forum_post SET invisible = 0 WHERE tid = 103 AND first = 1",
    )
    .await;
    thread_changed(db, 103).await;
    execute(
        db,
        "UPDATE gonof_forum_post SET invisible = -2 WHERE pid = 103002",
    )
    .await;
    // Nested replies carry the parent post's hidden copy (`root_invisible`): recompute by the regular rules after raw
    // SQL changed the top-level post.
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    let thread = gono_forum_web::test_support::thread_with_all_pages(db, 103)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        thread.posts.len(),
        78,
        "hiding keeps the permanent floor numbers and page slots"
    );
    let masked: Vec<_> = thread
        .posts
        .iter()
        .filter(|post| post.pid == 103002 || post.root_pid == Some(103002))
        .collect();
    assert_eq!(masked.len(), 25);
    assert!(
        masked
            .iter()
            .all(|post| post.hidden && post.body.is_empty())
    );
    assert_eq!(thread.reply_count, 78);
    assert_eq!(
        content::sidebar(db, gono_forum_web::test_support::SEEDED_SIDEBAR)
            .await
            .unwrap()
            .replies,
        191
    );
    assert!(ids(db, "", Sort::Latest, Some("ginkgo")).await.is_empty());
    execute(
        db,
        "UPDATE gonof_forum_post SET invisible = 0 WHERE pid = 103002",
    )
    .await;
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    for (column, value, restore) in [
        ("status", "0", "1"),
        ("visibility", "'private'", "'public'"),
    ] {
        execute(
            db,
            &format!("UPDATE gonof_forum_forum SET {column} = {value} WHERE slug = 'showcase'"),
        )
        .await;
        forum_changed(db, "showcase").await;
        assert!(
            gono_forum_web::test_support::thread_with_all_pages(db, 103)
                .await
                .unwrap()
                .is_none()
        );
        assert!(ids(db, "showcase", Sort::Latest, None).await.is_empty());
        let sidebar =
            content::sidebar_at(db, 1788864000, gono_forum_web::test_support::SEEDED_SIDEBAR)
                .await
                .unwrap();
        assert_eq!(sidebar.total, 19);
        assert!(!sidebar.hot.is_empty());
        // The thread row has no forum URL: compare by forum id.
        let showcase: i64 = gono_forum_web::db::query_scalar(
            "SELECT fid FROM gonof_forum_forum WHERE slug='showcase'",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        assert!(
            sidebar
                .hot
                .iter()
                .chain(&sidebar.latest)
                .all(|thread| thread.fid != showcase)
        );
        assert!(
            !content::forums(db)
                .await
                .unwrap()
                .iter()
                .any(|forum| forum.slug == "showcase")
        );
        let app = router::build(support::config_for(db), &project_root(), db.clone());
        for (path, expected) in [
            ("/?sort=wrong", StatusCode::BAD_REQUEST),
            ("/missing", StatusCode::NOT_FOUND),
        ] {
            let (status, _, body) = support::fetch(&app, path, false).await;
            assert_eq!(status, expected);
            assert!(
                !body.contains("/showcase"),
                "error pages must respect forum visibility too: {path}"
            );
        }
        execute(
            db,
            &format!("UPDATE gonof_forum_forum SET {column} = {restore} WHERE slug = 'showcase'"),
        )
        .await;
        forum_changed(db, "showcase").await;
    }
}

async fn assert_constraints(db: &Database) {
    // "linden" is a fixture author as the import stores it (normalized, lowercase).
    for statement in [
        "INSERT INTO gonof_common_member (username) VALUES ('linden')",
        "UPDATE gonof_forum_forum SET slug = 'daily' WHERE slug = 'ai'",
        "UPDATE gonof_forum_thread SET fid = 999999 WHERE tid = 101",
        "UPDATE gonof_forum_thread SET authorid = 999999 WHERE tid = 101",
        "UPDATE gonof_forum_post SET authorid = 999999 WHERE pid = 101002",
        // Explicitly writing a 0 or negative primary key is not stopped by the database: code does not write
        // auto-increment columns explicitly, see `db::source_guards`; fixture import checks that ids are
        // positive itself, see the `import_fixture` assertion above.
        "INSERT INTO gonof_forum_post (pid, fid, tid, position, root_position, message) SELECT NULL, fid, tid, 8, 99, 'Null PID' FROM gonof_forum_thread WHERE tid = 101",
    ] {
        reject(db, statement).await;
    }
    // Leading/trailing whitespace and empty usernames are not rejected by the database: code checks before writing
    // (`identity::rules`), and existing data is detected by the identity check script
    // (`tests/consistency/identity.sql`).
    for statement in [
        "INSERT INTO gonof_common_member (username) VALUES (' Leading space')",
        "INSERT INTO gonof_common_member (username) VALUES ('')",
    ] {
        gono_forum_web::test_support::assert_identity_rejected_or_flagged(
            db,
            statement,
            "members with invalid values",
        )
        .await;
    }
    const THREAD: &str = "threads with invalid values";
    const POST: &str = "posts with invalid values or shape";
    const TWO_LEVEL: &str = "nested replies that break the two-level reply structure";
    const FORUM: &str = "forums with invalid values or nesting";
    for (statement, item) in [
        (
            "UPDATE gonof_forum_forum SET visibility = 'invalid' WHERE slug = 'ai'",
            FORUM,
        ),
        (
            "UPDATE gonof_forum_forum SET slug = 'UPPERCASE' WHERE slug = 'ai'",
            FORUM,
        ),
        (
            "UPDATE gonof_forum_thread SET visibility = 'invalid' WHERE tid = 101",
            THREAD,
        ),
        (
            "UPDATE gonof_forum_thread SET publication_state = 'invalid' WHERE tid = 101",
            THREAD,
        ),
        (
            "UPDATE gonof_forum_thread SET maxposition = 0 WHERE tid = 101",
            THREAD,
        ),
        (
            "UPDATE gonof_forum_thread SET subject = '' WHERE tid = 101",
            THREAD,
        ),
        (
            "UPDATE gonof_forum_post SET position = 0 WHERE pid = 101002",
            POST,
        ),
        (
            "UPDATE gonof_forum_post SET root_pid = 101007, reply_to_pid = 101007 WHERE pid = 101005",
            TWO_LEVEL,
        ),
        (
            "UPDATE gonof_forum_post SET root_pid = 101005, reply_to_pid = 101005 WHERE pid = 101005",
            POST,
        ),
    ] {
        rejected_or_flagged(db, statement, item).await;
    }
    let first_pid: i64 = gono_forum_web::db::query_scalar(
        "SELECT pid FROM gonof_forum_post WHERE tid = 101 AND first = 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let first_pid = first_pid.to_string();
    // The third item being None: the database rejects it (primary key, foreign key); Some: guaranteed by code and
    // detected by the check script.
    for (index, (first, position, root, reply, item)) in [
        (1, 8, "NULL", "NULL", Some(POST)), // The first post can only be at position=1.
        (0, 1, "NULL", "NULL", None),
        (0, 0, "NULL", "NULL", Some(POST)),
        (0, 2, "NULL", "NULL", None),                // Unique position.
        (1, 1, "NULL", "NULL", None),                // One first post per thread.
        (0, 8, "103002", "103002", None),            // Parent in another thread.
        (0, 8, "101005", "101005", Some(TWO_LEVEL)), // A third level.
        (
            0,
            8,
            first_pid.as_str(),
            first_pid.as_str(),
            Some(TWO_LEVEL),
        ), // The first post cannot be the parent of a reply.
        (0, 8, "101002", "101003", Some(TWO_LEVEL)), // A response across parent posts.
        (0, 8, "101002", "NULL", Some(POST)),
        (0, 8, "NULL", "101002", Some(POST)),
        (0, 8, "999999", "999999", None),
    ]
    .into_iter()
    .enumerate()
    {
        let pid = 900000 + index;
        execute(
            db,
            &format!("INSERT INTO gonof_forum_post_tableid (pid) VALUES ({pid})"),
        )
        .await;
        let statement = format!(
            "INSERT INTO gonof_forum_post (pid, fid, tid, first, position, root_pid, reply_to_pid, root_position, sub_position, message) SELECT {pid}, fid, tid, {first}, {position}, {root}, {reply}, CASE WHEN {first}=0 AND {root} IS NULL THEN 99 ELSE NULL END, CASE WHEN {first}=0 AND {root} IS NOT NULL THEN 99 ELSE NULL END, 'Constraint test' FROM gonof_forum_thread WHERE tid = 101"
        );
        match item {
            Some(item) => rejected_or_flagged(db, &statement, item).await,
            None => reject(db, &statement).await,
        }
    }
    execute(
        db,
        "INSERT INTO gonof_forum_post_tableid (pid) VALUES (900100)",
    )
    .await;
    rejected_or_flagged(db, "INSERT INTO gonof_forum_post (pid, fid, tid, position, root_pid, reply_to_pid, sub_position, message) SELECT 900100, fid, tid, 8, 900100, 900100, 99, 'Self reference' FROM gonof_forum_thread WHERE tid = 101", POST).await;
    rejected_or_flagged(db, "INSERT INTO gonof_forum_post (pid, fid, tid, position, root_pid, reply_to_pid, sub_position, message) SELECT 900100, fid, tid, 8, 101002, 900100, 99, 'Replies to itself' FROM gonof_forum_thread WHERE tid = 101", POST).await;
    reject(db, "INSERT INTO gonof_forum_post (pid, fid, tid, position, root_position, message) SELECT 999999, fid, tid, 8, 99, 'No allocated PID' FROM gonof_forum_thread WHERE tid = 101").await;
    reject(db, "INSERT INTO gonof_forum_post (pid, fid, tid, position, root_position, message) VALUES (900100, 999999, 101, 8, 99, 'Wrong forum')").await;
    assert_eq!(count(db, "gonof_forum_post").await, 240);
}

async fn assert_transactions(db: &Database) {
    let title = gono_forum_web::test_support::thread_with_all_pages(db, 101)
        .await
        .unwrap()
        .unwrap()
        .title;
    let mut transaction = db.begin_write().await.unwrap();
    gono_forum_web::db::query(
        "UPDATE gonof_forum_thread SET subject = 'Must roll back' WHERE tid = 101",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(
        gono_forum_web::test_support::thread_with_all_pages(db, 101)
            .await
            .unwrap()
            .unwrap()
            .title,
        title
    );
    // After importing explicit IDs, MySQL's auto-increment allocator must still give the next normal write a new id.
    let mut transaction = db.begin_write().await.unwrap();
    for (table, key, insert) in [
        (
            "gonof_common_member",
            "uid",
            "INSERT INTO gonof_common_member (username) VALUES ('Next id test')",
        ),
        (
            "gonof_forum_thread",
            "tid",
            "INSERT INTO gonof_forum_thread (fid, subject, family_fid) SELECT fid, 'Next id test', fid FROM gonof_forum_forum WHERE slug = 'daily'",
        ),
        // Insert NULL to take the auto-increment value, the same statement on both backends.
        ("gonof_forum_post_tableid", "pid", {
            "INSERT INTO gonof_forum_post_tableid (pid) VALUES (NULL)"
        }),
    ] {
        let maximum = format!("SELECT MAX({key}) FROM {table}");
        let before: i64 = gono_forum_web::db::query_scalar(&maximum)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        gono_forum_web::db::query(insert)
            .execute(&mut *transaction)
            .await
            .unwrap();
        let after: i64 = gono_forum_web::db::query_scalar(&maximum)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert!(
            after > before,
            "after explicit ids are imported the {table}.{key} allocator must move on"
        );
    }
    transaction.rollback().await.unwrap();
    assert_eq!(count(db, "gonof_common_member").await, 6);
    assert_eq!(count(db, "gonof_forum_thread").await, 24);
}

#[tokio::test]
async fn mysql_database_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    database_contract(services.db.clone()).await;
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn mysql_enforces_foreign_keys_on_every_pool_connection() {
    // The connection parameters of SQLite's two pools are checked in tests/sqlite_schema.rs.
    gono_forum_web::mysql_only!();
    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let mut connections = Vec::new();
    for _ in 0..4 {
        let mut connection = services.db.pool.acquire().await.unwrap();
        let enabled: i64 = gono_forum_web::db::query_scalar("SELECT @@foreign_key_checks")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(enabled, 1);
        let mode: String = gono_forum_web::db::query_scalar("SELECT @@sql_mode")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert!(mode.contains("STRICT_TRANS_TABLES"));
        assert!(mode.contains("NO_AUTO_VALUE_ON_ZERO"));
        connections.push(connection);
    }
    drop(connections);
    services.cleanup().await.unwrap();
}

/// Without calling cleanup, Drop must delete the database (on SQLite its files), the Redis namespace and the local
/// directory before returning: a detached thread would be killed when the process exits, which is exactly why each
/// run used to leak a few `gonof_test_*`. The fixture also holds an uncommitted transaction, whose metadata lock on
/// MySQL must not make DROP DATABASE hang until it times out.
#[tokio::test]
async fn dropped_fixture_removes_its_database_before_returning() {
    use gono_forum_web::cache::{Cache, CacheKey, registry};

    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let name = services.db.pool.database_name().unwrap();
    assert!(
        name.starts_with(&format!("gonof_test_{}_", std::process::id())),
        "the database name carries this process id, so each run counts and cleans only its own leftovers: {name}"
    );
    let root = services.root.clone();
    let redis = services.config.redis.clone();
    let sqlite_file = services
        .config
        .database
        .url
        .strip_prefix("sqlite://")
        .map(std::path::PathBuf::from);
    let key = CacheKey::new(&registry::FLOORS, 1, &"fixture-drop").unwrap();
    services
        .cache
        .set_json(
            &key,
            &1_i64,
            gono_forum_web::cache::Lifetimes {
                member: 30,
                page: 60,
                list: 60,
                statistics: 600,
                forum_statistics: 600,
            },
        )
        .await
        .unwrap();
    let mut open = services.db.pool.begin().await.unwrap();
    gono_forum_web::db::query(
        "INSERT INTO gonof_common_settings(skey,svalue) VALUES('fixture.drop','pending')",
    )
    .execute(&mut *open)
    .await
    .unwrap();

    let started = std::time::Instant::now();
    drop(services);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "Drop must not wait for the metadata lock to time out: {:?}",
        started.elapsed()
    );
    if let Some(file) = sqlite_file {
        for suffix in ["", "-wal", "-shm"] {
            let path = format!("{}{suffix}", file.display());
            assert!(
                !std::path::Path::new(&path).exists(),
                "the database file must be gone when Drop returns: {path}"
            );
        }
    } else {
        let admin = sqlx::MySqlPool::connect(&support::config().database.url)
            .await
            .unwrap();
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME=?",
        )
        .bind(&name)
        .fetch_one(&admin)
        .await
        .unwrap();
        admin.close().await;
        assert_eq!(
            remaining, 0,
            "the database must be gone when Drop returns: {name}"
        );
    }
    assert!(!root.exists());
    let peer = Cache::connect(&redis).await.unwrap();
    assert!(peer.get_json::<i64>(&key).await.unwrap().is_none());
    drop(open);
}
