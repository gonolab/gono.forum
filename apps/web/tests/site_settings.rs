//! Storage contract of "one item per row" for site settings and policy documents.
//!
//! Keys all live under `settings.{group}.{field}` (docs/settings-and-caching.md, "Keys and groups"), and reads have
//! no fallback: missing rows, bad values and out-of-range values are always errors stating the key, the stored value
//! and the expected value (same doc, "Reading settings").
use gono_forum_web::{
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::Actor,
    runtime,
    test_support::TestFixture,
};

#[path = "support/http_auth.rs"]
mod http_auth;
#[path = "support/mod.rs"]
mod support;
use gono_forum_web::mail::templates;
use sqlx::{Column, Row};
use std::sync::Arc;

const ADMIN: Actor = Actor {
    uid: 9801,
    credentials_version: 1,
    api_key: None,
};

async fn fixture() -> TestFixture {
    let f = TestFixture::migrated().await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,'settings-admin','synthetic-hash','settings-admin@example.test',1,1,1)")
        .bind(ADMIN.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    f
}

async fn rows(db: &Database, low: &str, high: &str) -> Vec<(String, String)> {
    db::query(
        "SELECT skey,svalue FROM gonof_common_settings WHERE skey>=? AND skey<? ORDER BY skey",
    )
    .bind(low)
    .bind(high)
    .fetch_all(db.executor())
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            db::text(row, "skey").unwrap(),
            db::text(row, "svalue").unwrap(),
        )
    })
    .collect()
}

/// Scalar settings: `settings.` minus the three families of mail templates, navigation and policies. The 130 keys the
/// write gate reads by primary key `IN` (`runtime::scalar_keys`) cover the effective settings, and
/// [`migrations_seed_every_setting_once_under_settings`] checks that both sides agree.
async fn settings_rows(db: &Database) -> Vec<(String, String)> {
    let mut all = Vec::new();
    for (low, high) in [
        // Documents are left out: the domain layout documents, the layout document (docs/skins.md), mail settings, the
        // navigation document and policies.
        ("settings.", "settings.domain_layout."),
        ("settings.domain_layout/", "settings.layout."),
        ("settings.layout/", "settings.mail."),
        ("settings.mail/", "settings.navigation."),
        ("settings.navigation/", "settings.policy."),
        ("settings.policy/", "settings/"),
    ] {
        all.extend(rows(db, low, high).await);
    }
    all
}

async fn set_raw(db: &Database, key: &str, value: &str) {
    db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
        .bind(value)
        .bind(key)
        .execute(db.executor())
        .await
        .unwrap();
}

async fn save(f: &TestFixture, edit: impl FnOnce(&mut runtime::SiteSettings)) {
    runtime::update(&f.db, &f.global, ADMIN, "site.settings.save", edit)
        .await
        .unwrap();
}

/// The [`runtime::SettingsError`] in the error chain when reading fails.
fn settings_error(error: gono_forum_web::identity::IdentityError) -> runtime::SettingsError {
    match error {
        gono_forum_web::identity::IdentityError::Internal(error) => error
            .downcast_ref::<runtime::SettingsError>()
            .cloned()
            .unwrap_or_else(|| panic!("expected a settings error: {error:#}")),
        other => panic!("expected a settings error: {other:?}"),
    }
}

#[tokio::test]
async fn migrations_seed_every_setting_once_under_settings() {
    let f = fixture().await;
    let stored = settings_rows(&f.db).await;
    // The baseline seeds one row per scalar setting.
    assert_eq!(stored.len(), 136, "{stored:?}");
    // The gate reads by primary key and locks all scalar settings.
    assert_eq!(
        stored
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        runtime::scalar_keys()
    );
    let value = |key: &str| {
        stored
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("missing {key}"))
    };
    // The 16 existing items were renamed with groups per 9.14 and their values kept: booleans are true/false, not 1/0,
    // and empty values are empty strings, not NULL.
    for (key, expected) in [
        ("settings.api.enabled", "true"),
        ("settings.time.clock24", "true"),
        ("settings.site.contact_email", ""),
        ("settings.time.date_format", "ymd-dash"),
        (
            "settings.site.description",
            "A sample community built with Gono Forum, for running tests.",
        ),
        ("settings.seo.indexing", "false"),
        ("settings.invite.enabled", "true"),
        ("settings.links.applications", "true"),
        ("settings.site.read_only", "false"),
        (
            "settings.site.read_only_message",
            "The forum is in read-only mode for a while. Please come back soon.",
        ),
        ("settings.signup.mode", "invite"),
        ("settings.time.relative_hours", "72"),
        ("settings.time.relative", "false"),
        ("settings.site.name", "Sample Community"),
        // The migration seeds an empty string; the test fixture then writes the test domain (replacing the manual write
        // before deployment, see `TestFixture::migrated`).
        (
            "settings.site.url",
            gono_forum_web::test_support::TEST_SITE_URL,
        ),
        ("settings.time.offset_minutes", "480"),
        // The tip switch is seeded as on.
        ("settings.tip.enabled", "true"),
        ("settings.tip.amounts", "1,5,10,20"),
        ("settings.tip.daily_limit", "100"),
        // Spot checks: the cache lifetime and multi-line text with line breaks.
        ("settings.cache.ttl_seconds", "3600"),
        ("settings.cache.statistics_seconds", "600"),
        ("settings.forum.directory_statistics", "true"),
        (
            "settings.growth.level_names",
            "One-Star Member\nTwo-Star Member\nThree-Star Member\nFour-Star Member\nFive-Star Member\nSix-Star Member\nSeven-Star Member\nEight-Star Member",
        ),
        ("settings.seo.verification", ""),
    ] {
        assert_eq!(value(key), expected, "{key}");
    }
    let robots = value("settings.seo.robots");
    assert_eq!(robots.lines().count(), 12);
    assert!(robots.starts_with("User-agent: *\nDisallow: /manage\n"));
    assert!(
        robots.ends_with("Disallow: /verify-email"),
        "no trailing newline"
    );
    // The table holds the scalar settings, the navigation and layout documents and the two lock rows, nothing else.
    let total: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_common_settings")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    // Policies and mail templates live in gonof_common_localized_text.
    assert_eq!(
        total,
        136 + 3 + 3 + 2,
        "136 scalars, 3 navigation rows, 3 layout rows, 2 lock rows"
    );
    let legacy: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_settings WHERE skey LIKE 'site.%' OR skey LIKE 'credit.%'",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(legacy, 0);
    // Policies and mail templates are not in the settings table.
    assert!(
        rows(&f.db, "settings.policy.", "settings.policy/")
            .await
            .is_empty()
    );
    assert!(
        rows(&f.db, "settings.mail.", "settings.mail/")
            .await
            .is_empty()
    );
    assert_eq!(
        rows(&f.db, "settings.navigation.", "settings.navigation/")
            .await
            .len(),
        3
    );
    // All seed values are valid: they can be read, and they are exactly the current values before launch.
    let read = runtime::read(&f.db).await.unwrap();
    assert_eq!(read.site.name, "Sample Community");
    assert_eq!(read.signup.mode, "invite");
    assert_eq!(read.time.offset_minutes, 480);
    assert_eq!(read.cache.ttl_seconds, 3600);
    assert_eq!(read.posting.daily_replies, 20);
    assert_eq!(
        read.growth.levels,
        vec![0, 20, 80, 200, 500, 1200, 3000, 8000]
    );
    assert_eq!(read.growth.level_names.len(), 8);
    assert_eq!(read.tip.amounts, vec![1, 5, 10, 20]);
    assert!(
        read.invite.enabled && read.api.enabled && read.time.clock24 && read.links.applications
    );
    assert!(!read.site.read_only && !read.seo.indexing && !read.time.relative);
    f.cleanup().await.unwrap();
}

/// No fallback (docs/settings-and-caching.md, "Reading settings"): missing rows, bad values and out-of-range values
/// are all errors stating which key, what is stored and what it should be;
/// the page frame reads settings on every page, so the whole site (including admin settings pages) shows this page.
#[tokio::test]
async fn missing_bad_or_out_of_range_rows_fail_loudly_everywhere() {
    let f = fixture().await;
    // Missing row.
    db::query("DELETE FROM gonof_common_settings WHERE skey='settings.time.relative_hours'")
        .execute(f.db.executor())
        .await
        .unwrap();
    let error = settings_error(runtime::read(&f.db).await.unwrap_err());
    assert_eq!(error.key, "settings.time.relative_hours");
    assert_eq!(error.found, None);
    assert!(error.expected.contains("1 to 720"), "{}", error.expected);
    db::query(
        "INSERT INTO gonof_common_settings(skey,svalue) VALUES('settings.time.relative_hours','72')",
    )
    .execute(f.db.executor())
    .await
    .unwrap();
    runtime::read(&f.db).await.unwrap();

    // Bad values: an integer that cannot be parsed, a boolean that is not true/false, an enum not in the allow list —
    // none fall back to defaults any more.
    for (key, bad, good) in [
        ("settings.time.offset_minutes", "abc", "480"),
        ("settings.time.clock24", "1", "true"),
        // The registration mode especially must not slip through: with an unrecognised value stored, every equality check
        // by consumers fails.
        ("settings.signup.mode", "xyz", "invite"),
        // Out of range: the lifetime is 0–86400.
        ("settings.cache.ttl_seconds", "86401", "3600"),
        // Cross-item constraint: the daily thread quota cannot be less than the first-day quota.
        ("settings.posting.daily_threads", "0", "5"),
    ] {
        set_raw(&f.db, key, bad).await;
        let error = settings_error(runtime::read(&f.db).await.unwrap_err());
        assert_eq!(error.key, key);
        assert_eq!(error.found.as_deref(), Some(bad));
        // The write gate reports the error too, and protected writes cannot get in.
        let mut tx = f.db.begin_write().await.unwrap();
        assert!(runtime::locked(&f.db, &mut tx).await.is_err(), "{key}");
        tx.rollback().await.unwrap();
        set_raw(&f.db, key, good).await;
    }

    // Pages: the home page, policy pages and admin settings pages (with the administrator logged in) are all the same
    // error page, stating the key, the stored value and the expected value.
    let identity = gono_forum_web::identity::Identity::new(
        f.db.clone(),
        f.config.identity.clone(),
        f.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let admin = http_auth::login(&identity, &f.db, ADMIN.uid).await;
    set_raw(&f.db, "settings.signup.mode", "xyz").await;
    let app = gono_forum_web::router::build_with_global(
        support::config_for(&f.db),
        &gono_forum_web::project_root(),
        f.db.clone(),
        f.cache.clone(),
        f.global.clone(),
    );
    for path in ["/", "/rules", "/manage/settings"] {
        let request = axum::http::Request::builder()
            .uri(path)
            .header(
                axum::http::header::COOKIE,
                format!("gonof_session__port58833={}", admin.session_token),
            )
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), request)
            .await
            .unwrap();
        let status = response.status();
        let body = String::from_utf8(
            http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            status,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "{path}"
        );
        assert!(body.contains("settings.signup.mode"), "{path}: {body}");
        assert!(body.contains("\u{201c}xyz\u{201d}"), "{path}: {body}");
        assert!(
            body.contains("The registration mode must be closed, invite or open"),
            "{path}: {body}"
        );
        assert!(body.contains("data-settings-error"), "{path}: {body}");
    }
    // Once that row is fixed, the next request recovers: failed source reads are not cached.
    set_raw(&f.db, "settings.signup.mode", "invite").await;
    let (status, _, _) = support::fetch(&app, "/", false).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn saving_one_page_leaves_every_other_row_byte_identical() {
    let f = fixture().await;
    save(&f, |s| {
        s.site.description = "Description".into();
        s.signup.mode = "closed".into();
        s.site.read_only = true;
        s.site.read_only_message = "Under maintenance".into();
        s.time.offset_minutes = 0;
    })
    .await;
    let before = settings_rows(&f.db).await;
    save(&f, |s| s.site.name = "Only the name changes".into()).await;
    let after = settings_rows(&f.db).await;
    assert_eq!(after.len(), 136, "saving neither adds nor removes rows");
    for ((key, old), (_, new)) in before.iter().zip(&after) {
        if key == "settings.site.name" {
            assert_eq!(
                (old.as_str(), new.as_str()),
                ("Sample Community", "Only the name changes")
            );
        } else {
            assert_eq!(old, new, "{key} must not change with this save");
        }
    }
    // The audit log records which item changed: without optimistic locking, this is the only clue to "who reverted
    // whose change".
    let data: String = db::query_scalar(
        "SELECT data FROM gonof_common_log WHERE type='site.settings.save' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(data.contains("settings.site.name"), "{data}");
    assert!(!data.contains("settings.site.description"), "{data}");
    // Validation failure: the expected value is reported, and not a character in the database changes.
    let error = runtime::update(&f.db, &f.global, ADMIN, "site.settings.save", |s| {
        s.cache.ttl_seconds = 100_000;
    })
    .await
    .unwrap_err();
    assert!(
        matches!(error, gono_forum_web::identity::IdentityError::Validation(message) if message.contains("0 to 86400")),
    );
    assert_eq!(settings_rows(&f.db).await, after);
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_page_frame_still_reads_settings_in_a_single_query() {
    let f = fixture().await;
    let profile = Arc::new(RequestProfile::new());
    let rows = trace::scope(profile.clone(), runtime::read_with_navigation(&f.db))
        .await
        .unwrap();
    assert_eq!(rows.settings.site.name, "Sample Community");
    assert!(rows.navigation.body.is_empty() && rows.navigation.updated_at == 0);
    // No skin was chosen: the layout document is empty and no domain overrides it.
    assert!(rows.layout.is_none() && rows.domain_layouts.is_empty());
    // The source read runs at least once per cache period: the scalar keys plus the navigation and layout documents
    // (docs/skins.md, "Choosing a skin"), written as one statement with a primary key IN and the range of the domain
    // layout documents, so the round trip count does not grow.
    let queries: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .collect();
    assert_eq!(queries.len(), 1, "{:?}", queries);
    // What is recorded is the text actually executed: on SQLite the rewriter removed the index hint.
    let hint = if f.db.backend() == gono_forum_web::db::Backend::MySql {
        " FORCE INDEX (PRIMARY)"
    } else {
        ""
    };
    assert!(queries[0].sql.contains(&format!(
        " FROM gonof_common_settings{hint} WHERE skey IN ("
    )));
    assert!(queries[0].sql.contains(
        "'settings.navigation.body','settings.navigation.title','settings.navigation.updated_at'"
    ));
    assert!(
        queries[0].sql.contains(
            "'settings.layout.body','settings.layout.title','settings.layout.updated_at'"
        )
    );
    // Document and template bodies are not read with the page frame: the IN list has no settings.mail. or
    // settings.policy..
    assert!(!queries[0].sql.contains("'settings.mail."));
    assert!(!queries[0].sql.contains("'settings.policy."));
    // The execution plan is only checked on MySQL (primary key range read).
    if f.db.backend() == db::Backend::MySql {
        assert_eq!(
            explain(&f.db, &queries[0].sql, &[]).await,
            // The domain layout documents' range adds one estimated row even when it is empty.
            ("range".into(), "PRIMARY".into(), 136 + 3 + 3 + 1)
        );
    }
    let rows: i64 = db::query_scalar(&format!(
        "SELECT COUNT(*) FROM ({}) t",
        queries[0].sql.replace("SELECT skey,svalue", "SELECT skey")
    ))
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(rows, 136 + 3 + 3);
    f.cleanup().await.unwrap();
}

/// Policies keep one row per language in `gonof_common_localized_text` (docs/architecture.md, "Languages"); saving
/// overwrites it and keeps no history. The navigation document stays in the settings table, with no fallback.
#[tokio::test]
async fn documents_store_one_row_per_language_and_keep_no_snapshots() {
    let f = fixture().await;
    let localized = |db: Database| async move {
        db::query("SELECT language,title,body,updated_at FROM gonof_common_localized_text WHERE kind='policy.terms'")
            .fetch_all(db.executor())
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    db::text(row, "language").unwrap(),
                    db::text(row, "title").unwrap(),
                    db::text(row, "body").unwrap(),
                    row.try_get::<i64, _>("updated_at").unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let saved = runtime::save_document(
        &f.db,
        &f.global,
        ADMIN,
        "policy.terms",
        runtime::Document {
            title: "Terms of Use".into(),
            body: "Body text".into(),
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    assert!(saved.updated_at > 0);
    assert_eq!(
        localized(f.db.clone()).await,
        [(
            "en".to_owned(),
            "Terms of Use".to_owned(),
            "Body text".to_owned(),
            saved.updated_at
        )],
        "one row in the site language (the fixture is en)"
    );

    // Save again: no more version conflicts, and no more .v.{n} history rows growing.
    runtime::save_document(
        &f.db,
        &f.global,
        ADMIN,
        "policy.terms",
        runtime::Document {
            title: "Terms of Use".into(),
            body: "Edited body text".into(),
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(localized(f.db.clone()).await.len(), 1, "saving adds no row");
    assert_eq!(
        runtime::document(&f.db, "policy.terms").await.unwrap().body,
        "Edited body text"
    );
    let snapshots: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_common_settings WHERE skey LIKE '%.v.%'")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(snapshots, 0);
    // The navigation document has no fallback: a missing row is an error.
    db::query("DELETE FROM gonof_common_settings WHERE skey='settings.navigation.title'")
        .execute(f.db.executor())
        .await
        .unwrap();
    let error = settings_error(runtime::document(&f.db, "navigation").await.unwrap_err());
    assert_eq!(error.key, "settings.navigation.title");
    f.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_write_gate_never_observes_a_half_written_settings_page() {
    let f = fixture().await;
    let closed = |s: &mut runtime::SiteSettings| {
        s.site.read_only = true;
        s.site.read_only_message = "Under maintenance".into();
    };
    let open = |s: &mut runtime::SiteSettings| {
        s.site.read_only = false;
        s.site.read_only_message = "Open as usual".into();
    };
    // Fix the starting point first: otherwise the reader could read the initial state seeded by migrations before the
    // writer's first save, which is a legitimate third combination and would make the assertion fail randomly.
    save(&f, open).await;
    let db = f.db.clone();
    let global = f.global.clone();
    let writer = tokio::spawn(async move {
        for round in 0..30 {
            let edit: fn(&mut runtime::SiteSettings) = if round % 2 == 0 { closed } else { open };
            runtime::update(&db, &global, ADMIN, "site.settings.save", edit)
                .await
                .unwrap();
        }
    });
    let db = f.db.clone();
    let reader = tokio::spawn(async move {
        for _ in 0..120 {
            // The gate is a locking read: the shared lock is held across statements until commit, and during that time
            // the save transaction can neither get the exclusive lock nor commit, so its atomicity comes from the lock
            // rather than from "one statement". This assertion guards against replacing the locking read with a
            // non-locking one.
            let mut tx = db.begin_write().await.unwrap();
            let gate = runtime::locked(&db, &mut tx).await.unwrap();
            tx.commit().await.unwrap();
            let pair = (gate.site.read_only, gate.site.read_only_message.as_str());
            assert!(
                pair == (true, "Under maintenance") || pair == (false, "Open as usual"),
                "the gate read half-old, half-new settings: {pair:?}"
            );
            // A plain non-locking read has no lock to rely on, only on "one statement, one read view": the read-only
            // switch and the message are in two rows, and splitting into two statements would read "already read-only
            // but the message is still the previous version".
            let read = runtime::read(&db).await.unwrap();
            let pair = (read.site.read_only, read.site.read_only_message.as_str());
            assert!(
                pair == (true, "Under maintenance") || pair == (false, "Open as usual"),
                "a plain read got half-old, half-new settings: {pair:?}"
            );
        }
    });
    writer.await.unwrap();
    reader.await.unwrap();
    f.cleanup().await.unwrap();
}

/// Errors like a missing table must appear on the page with their original text, rather than collapsing into
/// "Please try again later".
///
/// This is exactly what happens when the row-splitting migration has run while an old binary is still connected to the
/// same database: the page only says "This page cannot be shown right now", and the real `1146 … doesn't exist` is only
/// visible to an administrator with the debug panel open.
#[tokio::test]
async fn a_missing_table_shows_the_database_error_on_the_page() {
    let f = fixture().await;
    db::query("RENAME TABLE gonof_common_settings TO gonof_common_settings_gone")
        .sqlite("ALTER TABLE gonof_common_settings RENAME TO gonof_common_settings_gone")
        .execute(f.db.executor())
        .await
        .unwrap();
    let app = gono_forum_web::router::build(
        support::config_for(&f.db),
        &gono_forum_web::project_root(),
        f.db.clone(),
    );
    let (status, _, body) = support::fetch(&app, "/", false).await;
    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    // The page copies the database error: MySQL with error code 1146 (42S02), SQLite with `no such table`.
    let (code, missing) = if f.db.backend() == db::Backend::MySql {
        ("1146 (42S02)", "doesn&#39;t exist")
    } else {
        ("no such table", "no such table")
    };
    assert_eq!(
        body.matches(code).count(),
        1,
        "the error must appear on the page exactly once: {body}"
    );
    assert!(
        body.contains("gonof_common_settings") && body.contains(missing),
        "the page must name the missing table: {body}"
    );
    db::query("RENAME TABLE gonof_common_settings_gone TO gonof_common_settings")
        .sqlite("ALTER TABLE gonof_common_settings_gone RENAME TO gonof_common_settings")
        .execute(f.db.executor())
        .await
        .unwrap();
    f.cleanup().await.unwrap();
}

/// The access type, index and row estimate of `EXPLAIN`. Columns are located by the result metadata at execution time
/// (same as `debug/explain.rs`).
async fn explain(db: &Database, sql: &str, binds: &[String]) -> (String, String, u64) {
    let statement = format!("EXPLAIN {sql}");
    let mut query = sqlx::query(&statement).persistent(false);
    for value in binds {
        query = query.bind(value);
    }
    let rows = query
        .fetch_all(
            db.pool
                .as_mysql()
                .expect("EXPLAIN is only checked on MySQL"),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{sql}");
    let row = &rows[0];
    let ordinal = |name: &str| {
        row.columns()
            .iter()
            .position(|column| column.name() == name)
            .unwrap_or_else(|| panic!("EXPLAIN has no {name} column"))
    };
    let text = |name: &str| {
        row.try_get_unchecked::<Option<String>, _>(ordinal(name))
            .unwrap()
            .unwrap_or_default()
    };
    let estimate: u64 = row
        .try_get_unchecked::<Option<u64>, _>(ordinal("rows"))
        .unwrap()
        .unwrap();
    (text("type"), text("key"), estimate)
}

/// The locking reads of scalar settings and the navigation document are equality lookups on the primary key
/// (docs/settings-and-caching.md, "Write gate and locks"). Mail templates and policies have their own table and are
/// written by primary key without a locking read.
///
/// The scalar keys are most of the table, and without `FORCE INDEX (PRIMARY)` both 5.7.44 and 8.4.11 use a full table
/// scan, where a locking read locks every row; this test runs once on each version to pin the execution plan.
#[tokio::test]
async fn settings_locks_seek_the_primary_key() {
    // Only verifies MySQL execution plans.
    gono_forum_web::mysql_only!();
    let f = fixture().await;
    let seek = |estimate: u64| ("range".to_owned(), "PRIMARY".to_owned(), estimate);
    assert_eq!(explain(&f.db, &runtime::GATE_SQL, &[]).await, seek(136));
    let navigation = document_keys("settings.navigation.");
    assert_eq!(
        explain(&f.db, runtime::DOCUMENT_LOCK_SQL, &navigation).await,
        seek(3)
    );
    f.cleanup().await.unwrap();
}

/// The three rows of one document, in the same order as the placeholders of `DOCUMENT_LOCK_SQL`.
fn document_keys(prefix: &str) -> Vec<String> {
    ["body", "title", "updated_at"]
        .map(|field| format!("{prefix}{field}"))
        .to_vec()
}

/// A transaction with its own connection: a lock wait reports 1205 after 1 second, the connection is closed after use,
/// and session variables do not go back to the pool.
/// `read_committed` corresponds to content writes (`begin_content_write`); otherwise, like `begin_write`, it uses the
/// instance's default REPEATABLE READ.
async fn probing(db: &Database, read_committed: bool) -> gono_forum_web::db::Tx<'static> {
    let mut connection = db.pool.acquire().await.unwrap();
    connection.close_on_drop();
    gono_forum_web::db::query("SET SESSION innodb_lock_wait_timeout=1")
        .execute(&mut *connection)
        .await
        .unwrap();
    let start = if read_committed {
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; START TRANSACTION"
    } else {
        "START TRANSACTION"
    };
    connection.begin_with(start).await.unwrap()
}

/// The kind of error where a lock wait returns 1205.
fn lock_wait_timeout(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .is_some_and(|error| error.number() == 1205)
}

/// The write gate and saves of documents and templates never wait for each other (docs/settings-and-caching.md,
/// "Write gate and locks").
///
/// Reading key ranges would also read, and when necessary wait for, the row after the end of each range, which
/// belongs to another family, so the gate and a document save could wait for each other. With equality reads by
/// primary key, the rows locked by the two sides do not intersect, so neither waits for the other. The gate runs
/// under READ COMMITTED in content writes and under REPEATABLE READ in other write transactions; both are checked.
#[tokio::test]
async fn the_write_gate_and_document_saves_never_wait_for_each_other() {
    // Only verifies MySQL row locks (SQLite writes queue as a whole).
    gono_forum_web::mysql_only!();
    let f = fixture().await;
    let patience = std::time::Duration::from_secs(5);
    for read_committed in [false, true] {
        // Direction one: the navigation document is being saved (the exclusive lock uses exactly the statement used
        // when saving). Mail templates and policies are in their own table.
        let mut documents = f.db.begin_write().await.unwrap();
        for kind in runtime::DOCUMENT_KINDS {
            let mut lock = db::query(runtime::DOCUMENT_LOCK_SQL);
            for key in document_keys(&format!("settings.{kind}.")) {
                lock = lock.bind(key);
            }
            assert_eq!(lock.fetch_all(&mut *documents).await.unwrap().len(), 3);
        }
        let mut gate = probing(&f.db, read_committed).await;
        runtime::locked(&f.db, &mut gate)
            .await
            .expect("the gate must not wait for row locks of documents and templates");
        gate.rollback().await.unwrap();
        // Saving scalar settings (exclusive lock, the same set of keys as the gate) does not wait either.
        tokio::time::timeout(
            patience,
            runtime::update(&f.db, &f.global, ADMIN, "site.settings.save", |s| {
                s.site.description = format!("lock wait probe {read_committed}");
            }),
        )
        .await
        .expect("saving scalar settings must not wait for row locks of documents and templates")
        .unwrap();
        documents.rollback().await.unwrap();

        // Direction two: with the gate holding its shared lock, saving the last template, the last policy and navigation
        // does not wait.
        let mut gate = probing(&f.db, read_committed).await;
        runtime::locked(&f.db, &mut gate).await.unwrap();
        tokio::time::timeout(
            patience,
            templates::save(
                &f.db,
                ADMIN,
                "security_notice",
                templates::MailTemplate::default(),
                true,
            ),
        )
        .await
        .expect("saving the last mail template must not wait for the gate")
        .unwrap();
        for kind in ["policy.verified", "navigation"] {
            tokio::time::timeout(
                patience,
                runtime::save_document(
                    &f.db,
                    &f.global,
                    ADMIN,
                    kind,
                    runtime::Document {
                        title: format!("{kind} lock wait probe"),
                        body: String::new(),
                        updated_at: 0,
                    },
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("saving {kind} must not wait for the gate"))
            .unwrap();
        }
        // Control: the gate really does lock the scalar settings, and another transaction taking an exclusive lock has to
        // wait, showing that the "no waiting" above is not a broken probe.
        let mut writer = probing(&f.db, read_committed).await;
        let waited = db::query(
            "SELECT skey FROM gonof_common_settings WHERE skey='settings.site.read_only' FOR UPDATE",
        )
        .fetch_all(&mut *writer)
        .await
        .expect_err("scalar settings cannot be written while the gate holds its lock");
        assert!(lock_wait_timeout(&waited), "{waited}");
        writer.rollback().await.unwrap();
        gate.rollback().await.unwrap();
    }
    f.cleanup().await.unwrap();
}
