mod support;

use axum::{Router, http::StatusCode};
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
    },
    db::Database,
    identity::{self, Actor},
    project_root, router,
    urls::Sort,
};

const TITLE: &str = "VISIBILITY_PRIVATE_TITLE_103";
const OPENING: &str = "VISIBILITY_PRIVATE_OPENING_103";
const ROOT: &str = "VISIBILITY_PRIVATE_ROOT_103002";
const CHILD: &str = "VISIBILITY_SURVIVING_CHILD_103056";
const LATE_CHILD: &str = "VISIBILITY_SURVIVING_CHILD_103076";

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    db: Database,
    app: Router,
    editor: ContentEditor,
    first_pid: i64,
}

impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let config = services.config.clone();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        let mut threads = content::fixtures();
        let thread = threads.iter_mut().find(|thread| thread.id == 103).unwrap();
        thread.title = TITLE.into();
        thread.author = "visibility-opening".into();
        thread.body = vec![OPENING.into()];
        thread.created = (identity::now() - 120) as u64;
        thread.updated = (identity::now() - 60) as u64;
        for post in &mut thread.posts {
            post.dateline = (identity::now() - 90) as u64;
            match post.pid {
                103002 => {
                    post.author = "visibility-root".into();
                    post.body = ROOT.into();
                }
                103056 => {
                    post.author = "visibility-child".into();
                    post.body = CHILD.into();
                }
                103076 => {
                    post.author = "visibility-child".into();
                    post.body = LATE_CHILD.into();
                }
                _ => {}
            }
        }
        content::import_fixture(&db, &threads).await.unwrap();
        for username in ["visibility-opening", "visibility-root", "visibility-child"] {
            gono_forum_web::db::query("UPDATE gonof_common_member SET password='unusable-visibility-test-password',email=?,emailstatus=1 WHERE username=?")
                .bind(format!("{username}@example.test")).bind(username).execute(db.executor()).await.unwrap();
        }
        gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,adminid,regdate) VALUES(9000,'visibility-admin','unusable-visibility-test-password',1,1)")
            .execute(db.executor()).await.unwrap();
        let first_pid = gono_forum_web::db::query_scalar(
            "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        Self {
            _services: services,
            editor: ContentEditor::new(
                db.clone(),
                config.content.clone(),
                config.media.orphan_seconds,
            ),
            app: router::build(config, &project_root(), db.clone()),
            db,
            first_pid,
        }
    }

    async fn change(&self, pid: i64, action: ContentAction) {
        let uid = if action == ContentAction::Withdraw {
            gono_forum_web::db::query_scalar("SELECT authorid FROM gonof_forum_post WHERE pid=?")
                .bind(pid)
                .fetch_one(self.db.executor())
                .await
                .unwrap()
        } else {
            9000
        };
        let actor = Actor {
            uid,
            credentials_version: 1,
            api_key: None,
        };
        let post = self.editor.editable(actor, pid).await.unwrap();
        self.editor
            .change(
                actor,
                ChangeRequest {
                    pid,
                    action,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: "Public read permission regression".into(),
                },
            )
            .await
            .unwrap();
    }

    async fn search(&self, text: &str) -> Vec<u64> {
        content::list_threads(&self.db, "", Sort::Latest, Some(text), 1, 100)
            .await
            .unwrap()
            .threads
            .iter()
            .map(|thread| thread.id)
            .collect()
    }

    async fn page(&self, path: &str) -> String {
        let (status, headers, body) = support::fetch(&self.app, path, false).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers["cache-control"], "private, no-store");
        body
    }

    async fn space(&self, username: &str) -> String {
        let uid = gono_forum_web::db::query_scalar::<i64>(
            "SELECT uid FROM gonof_common_member WHERE username=?",
        )
        .bind(username)
        .fetch_one(self.db.executor())
        .await
        .unwrap();
        gono_forum_web::urls::space(uid)
    }
}

#[tokio::test]
async fn withdrawn_topic_masks_title_opening_metadata_and_profile_but_keeps_discussion() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.search(TITLE).await, [103]);
    for needle in [OPENING, ROOT, CHILD] {
        assert!(fixture.search(needle).await.is_empty());
    }
    fixture
        .change(fixture.first_pid, ContentAction::Withdraw)
        .await;
    for needle in [TITLE, OPENING] {
        assert!(fixture.search(needle).await.is_empty());
    }
    for needle in [ROOT, CHILD, LATE_CHILD] {
        assert!(fixture.search(needle).await.is_empty());
    }
    let opening_space = fixture.space("visibility-opening").await;
    let child_space = fixture.space("visibility-child").await;
    for path in [
        "/",
        "/showcase",
        "/thread-103",
        "/thread-103-2?view=flat",
        "/search?q=SURVIVING",
        opening_space.as_str(),
        child_space.as_str(),
    ] {
        let page = fixture.page(path).await;
        assert!(
            !page.contains(TITLE),
            "the withdrawn title leaked into {path}"
        );
        assert!(
            !page.contains(OPENING),
            "the withdrawn body leaked into {path}"
        );
    }
    let topic = fixture.page("/thread-103").await;
    let (status, _, htmx) = support::fetch(&fixture.app, "/thread-103", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!htmx.contains(TITLE));
    assert!(!htmx.contains(OPENING));
    let model = content::thread(&fixture.db, 103).await.unwrap().unwrap();
    assert!(model.withdrawn);
    assert_eq!(model.title, "Thread withdrawn");
    assert_eq!(model.body, [String::new()]);
    let serialized = serde_json::to_string(&model).unwrap();
    assert!(!serialized.contains(TITLE));
    assert!(!serialized.contains(OPENING));
    let list = content::list_threads(&fixture.db, "", Sort::Latest, None, 1, 100)
        .await
        .unwrap();
    let model = list.threads.iter().find(|thread| thread.id == 103).unwrap();
    assert!(model.withdrawn);
    assert_eq!(model.title, "Thread withdrawn");
    assert_eq!(
        (model.first_pid, model.first_version, model.edited_at),
        (0, 0, 0)
    );
    assert_eq!(model.body, [String::new()]);
    let head = topic.split("</head>").next().unwrap();
    assert!(head.contains("<title>Thread withdrawn"));
    assert!(head.contains("property=\"og:title\" content=\"Thread withdrawn"));
    assert!(topic.contains(ROOT));
    assert!(topic.contains(CHILD));
    assert!(topic.contains("id=\"topic-reply-count\">78 replies"));
    let profile = fixture.page(&child_space).await;
    assert!(profile.contains(CHILD));
    assert!(profile.contains(LATE_CHILD));
    assert!(profile.contains("/thread-103?sub_root=103002&#38;sub_page=2#floor-76"));
    let stored = gono_forum_web::db::query("SELECT t.subject,p.message FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.tid=t.tid AND p.first=1 WHERE t.tid=103")
        .fetch_one(fixture.db.executor()).await.unwrap();
    assert_eq!(stored.get::<String, _>("subject"), TITLE);
    assert_eq!(stored.get::<String, _>("message"), OPENING);
    fixture
        .change(fixture.first_pid, ContentAction::Restore)
        .await;
    let restored = fixture.page("/thread-103").await;
    assert!(restored.contains(TITLE));
    assert!(restored.contains(OPENING));
    assert_eq!(fixture.search(TITLE).await, [103]);
    assert!(fixture.search(OPENING).await.is_empty());
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn hidden_topic_and_parent_replies_disappear_from_search_and_profiles() {
    let fixture = Fixture::new().await;
    let opening_space = fixture.space("visibility-opening").await;
    let root_space = fixture.space("visibility-root").await;
    let child_space = fixture.space("visibility-child").await;
    fixture.change(103002, ContentAction::Withdraw).await;
    let withdrawn = fixture.page("/thread-103").await;
    assert!(!withdrawn.contains(ROOT));
    assert!(withdrawn.contains("This reply was withdrawn. The discussion stays."));
    assert!(withdrawn.contains(CHILD));
    assert!(withdrawn.contains("id=\"topic-reply-count\">78 replies"));
    assert!(fixture.search(ROOT).await.is_empty());
    assert!(fixture.search(CHILD).await.is_empty());
    assert!(!fixture.page(&root_space).await.contains(ROOT));
    assert!(fixture.page(&child_space).await.contains(LATE_CHILD));
    fixture.change(103002, ContentAction::Hide).await;
    let hidden = fixture.page("/thread-103").await;
    assert!(hidden.contains("This reply is hidden."));
    assert!(hidden.contains("id=\"floor-2\""));
    assert!(hidden.contains("id=\"floor-56\""));
    assert!(hidden.contains("id=\"topic-reply-count\">78 replies"));
    for needle in [ROOT, CHILD, LATE_CHILD] {
        assert!(!hidden.contains(needle));
        assert!(fixture.search(needle).await.is_empty());
    }
    let mut model = content::thread(&fixture.db, 103).await.unwrap().unwrap();
    content::cached_replies(
        &fixture.db,
        &fixture
            ._services
            .cache
            .timed(gono_forum_web::cache::Lifetimes {
                member: 30,
                page: 60,
                list: 60,
                statistics: 600,
                forum_statistics: 600,
            }),
        &mut model,
        1,
        false,
        None,
        (1, gono_forum_web::test_support::SEEDED_PAGING),
    )
    .await
    .unwrap();
    for pid in [103002, 103056] {
        let post = model.posts.iter().find(|post| post.pid == pid).unwrap();
        assert!(post.hidden);
        assert!(
            post.body.is_empty(),
            "a hidden body must not enter the public model or cache"
        );
    }
    let serialized = serde_json::to_string(&model).unwrap();
    for needle in [ROOT, CHILD, LATE_CHILD] {
        assert!(!serialized.contains(needle));
    }
    assert!(!fixture.page(&child_space).await.contains(LATE_CHILD));
    assert!(
        fixture
            .page("/search?q=SURVIVING")
            .await
            .contains("No threads match.")
    );
    fixture.change(103002, ContentAction::Unhide).await;
    assert!(fixture.page("/thread-103").await.contains(CHILD));
    assert!(!fixture.page("/thread-103").await.contains(ROOT));
    fixture.change(103002, ContentAction::Restore).await;
    fixture.change(fixture.first_pid, ContentAction::Hide).await;
    for path in [
        "/thread-103",
        "/thread-103-2?view=flat",
        "/thread-103?sub_root=103002&sub_page=2",
    ] {
        let (status, _, body) = support::fetch(&fixture.app, path, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        for needle in [TITLE, OPENING, ROOT, CHILD, LATE_CHILD] {
            assert!(!body.contains(needle));
        }
    }
    for needle in [TITLE, OPENING, ROOT, CHILD, LATE_CHILD] {
        assert!(fixture.search(needle).await.is_empty());
    }
    assert!(
        fixture
            .page("/search?q=VISIBILITY")
            .await
            .contains("No threads match.")
    );
    for path in [
        "/",
        "/showcase",
        opening_space.as_str(),
        root_space.as_str(),
        child_space.as_str(),
    ] {
        let body = fixture.page(path).await;
        for needle in [TITLE, OPENING, ROOT, CHILD, LATE_CHILD] {
            assert!(!body.contains(needle), "{needle} leaked into {path}");
        }
    }
    fixture
        .change(fixture.first_pid, ContentAction::Unhide)
        .await;
    assert_eq!(fixture.search(TITLE).await, [103]);
    assert!(fixture.page(&child_space).await.contains(LATE_CHILD));
    fixture._services.cleanup_ref().await.unwrap();
}

fn stable_positions(body: &str) -> (Vec<usize>, Vec<String>) {
    let positions = body
        .split("id=\"floor-")
        .skip(1)
        .map(|part| part.split('"').next().unwrap().parse().unwrap())
        .collect();
    let links = body
        .split("class=\"floor__number\" href=\"")
        .skip(1)
        .map(|part| part.split('"').next().unwrap().to_owned())
        .collect();
    (positions, links)
}

#[tokio::test]
async fn tombstones_keep_fifty_reply_twenty_child_pages_and_permanent_links_stable() {
    let fixture = Fixture::new().await;
    let paths = [
        "/thread-103",
        "/thread-103-2",
        "/thread-103?sub_root=103002&sub_page=2",
        "/thread-103?view=flat",
        "/thread-103-2?view=flat",
    ];
    let mut baseline = Vec::new();
    for path in paths {
        baseline.push(stable_positions(&fixture.page(path).await));
    }
    assert_eq!(
        baseline.iter().map(|page| page.0.len()).collect::<Vec<_>>(),
        [71, 5, 55, 51, 29]
    );
    assert!(baseline[1].1.contains(&"/thread-103-2#floor-52".into()));
    assert!(
        baseline[2]
            .1
            .contains(&"/thread-103?sub_root=103002&#38;sub_page=2#floor-76".into())
    );
    let original = gono_forum_web::test_support::thread_with_all_pages(&fixture.db, 103)
        .await
        .unwrap()
        .unwrap();
    let identities = original
        .posts
        .iter()
        .map(|post| (post.pid, post.position, post.root_pid, post.reply_to_pid))
        .collect::<Vec<_>>();
    for (pid, action) in [
        (103002, ContentAction::Withdraw),
        (103075, ContentAction::Hide),
        (103002, ContentAction::Hide),
        (103002, ContentAction::Unhide),
        (103075, ContentAction::Unhide),
        (103002, ContentAction::Restore),
    ] {
        fixture.change(pid, action).await;
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                stable_positions(&fixture.page(path).await),
                baseline[index],
                "{action:?} pid={pid} changed {path}"
            );
        }
        let current = gono_forum_web::test_support::thread_with_all_pages(&fixture.db, 103)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current
                .posts
                .iter()
                .map(|post| (post.pid, post.position, post.root_pid, post.reply_to_pid))
                .collect::<Vec<_>>(),
            identities
        );
    }
    fixture._services.cleanup_ref().await.unwrap();
}

/// Lists for a logged-in viewer are read in index order, without a filesort over all public threads.
/// Unlisted threads have a negative `displayorder`, so a member's public and members-only
/// threads sit in the same index range, and the paged results match a one-shot sort row by row.
#[tokio::test]
async fn member_lists_merge_visibility_pairs_in_index_order() {
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    use std::sync::Arc;
    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    content::seed_fixture(&db).await.unwrap();
    // All four visibility combinations are needed: make one forum members-only, then make some threads members-only.
    let execute = |sql: &'static str| {
        let db = db.clone();
        async move {
            gono_forum_web::db::query(sql)
                .execute(db.executor())
                .await
                .unwrap();
        }
    };
    let fid: i64 = gono_forum_web::db::query_scalar("SELECT MIN(fid) FROM gonof_forum_thread")
        .fetch_one(db.executor())
        .await
        .unwrap();
    gono_forum_web::db::query("UPDATE gonof_forum_forum SET visibility='members' WHERE fid=?")
        .bind(fid)
        .execute(db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::sync_forum_copies(&db, fid)
        .await
        .unwrap();
    execute("UPDATE gonof_forum_thread SET visibility='members' WHERE tid%3=0").await;
    execute("UPDATE gonof_forum_thread SET displayorder=2 WHERE tid%7=0").await;
    gono_forum_web::test_support::refresh_listing(&db)
        .await
        .unwrap();
    // Multi-column COUNT(DISTINCT a,b) is MySQL syntax; count the deduplicated rows instead (one statement for both
    // backends).
    let pairs: i64 = gono_forum_web::db::query_scalar("SELECT COUNT(*) FROM (SELECT DISTINCT f.visibility AS forum_visibility,t.visibility FROM gonof_forum_thread t JOIN gonof_forum_forum f ON f.fid=t.fid WHERE t.publication_state='published') pairs")
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(
        pairs, 4,
        "the fixture must cover all four visibility combinations"
    );
    let visibilities: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(DISTINCT visibility) FROM gonof_forum_thread WHERE displayorder >= 0",
    )
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(
        visibilities, 2,
        "the member list must have both public and members-only threads"
    );
    let viewer: i64 = gono_forum_web::db::query_scalar("SELECT MIN(uid) FROM gonof_common_member")
        .fetch_one(db.executor())
        .await
        .unwrap();
    let expected: Vec<i64> = gono_forum_web::db::query_scalar(
        "SELECT t.tid FROM gonof_forum_thread t JOIN gonof_forum_forum f ON f.fid=t.fid LEFT JOIN gonof_forum_forum pf ON pf.fid=f.parent_fid
         WHERE f.status=1 AND f.visibility IN ('public','members') AND (pf.fid IS NULL OR (pf.status=1 AND pf.visibility IN ('public','members')))
         AND t.visibility IN ('public','members') AND t.publication_state='published' AND t.readperm=0 AND t.displayorder>=0
         AND NOT EXISTS(SELECT 1 FROM gonof_home_member_block b WHERE b.uid=? AND b.blocked_uid=t.authorid)
         ORDER BY t.displayorder DESC,t.lastpost DESC,t.tid DESC",
    )
    .bind(viewer)
    .fetch_all(db.executor())
    .await
    .unwrap();
    assert!(expected.len() > 14);
    let profile = Arc::new(RequestProfile::new());
    let mut seen = Vec::new();
    for page in 1..=expected.len().div_ceil(7) {
        let listed = trace::scope(
            profile.clone(),
            Box::pin(content::list_threads_for_viewer(
                &db,
                "",
                Sort::Latest,
                None,
                None,
                page,
                7,
                Some(viewer),
                &[],
            )),
        )
        .await
        .unwrap();
        assert_eq!(listed.total, expected.len());
        seen.extend(listed.threads.iter().map(|t| t.id as i64));
    }
    assert_eq!(
        seen, expected,
        "paging through merged segments must match a single sort"
    );
    // The debug trace records the text actually executed: on SQLite the rewriter removed FORCE INDEX, so recognize
    // the list statements by the thread table's alias.
    let sqlite = db.backend() == gono_forum_web::db::Backend::Sqlite;
    let lists: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|r| {
            r.source == QuerySource::Application
                && if sqlite {
                    r.sql.contains("FROM gonof_forum_thread t ")
                } else {
                    r.sql.contains("FORCE INDEX")
                }
        })
        .collect();
    assert!(!lists.is_empty());
    for record in lists {
        if sqlite {
            let plan = gono_forum_web::test_support::sqlite_plan(&db, &record.expanded_sql)
                .await
                .unwrap();
            gono_forum_web::test_support::assert_sqlite_index_order(&plan, &record.sql);
            continue;
        }
        let plan = gono_forum_web::db::query(&format!("EXPLAIN {}", record.expanded_sql))
            .fetch_all(db.executor())
            .await
            .unwrap();
        for row in plan {
            // SQLx only gets the column names of EXPLAIN at execution time, so lookups by name fail; convert to ordinals
            // first.
            let text = |column: &str| {
                let ordinal = row
                    .columns()
                    .iter()
                    .position(|c| c.name() == column)
                    .unwrap();
                row.try_get::<Option<Vec<u8>>, _>(ordinal)
                    .unwrap()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default()
            };
            if text("table") == "t" {
                assert!(
                    !text("Extra").contains("filesort"),
                    "the thread table must be read in index order: {}",
                    record.sql
                );
            }
        }
    }
    services.cleanup().await.unwrap();
}
