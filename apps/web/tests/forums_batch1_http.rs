//! Forum management: parent-forum moderators manage child forums, forum statistics and the directory page
//! `/forums`, the admin "Forum settings" block, deleting empty forums and merging via an admin task, cross-forum level-2
//! and level-3 pins.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::forum_filter::FilterMode,
    manage, moderation, organization,
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Forums-batch-one-password-2026!";
const ADMIN: i64 = 9960;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

/// A member with a verified e-mail address, registered for 30 days; `role` 1 administrator, 2 global moderator,
/// 3 forum moderator.
async fn member(db: &Database, uid: i64, role: i64) {
    let salt = SaltString::encode_b64(b"forums-batch-one-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, 1, ?, ?)")
        .bind(uid)
        .bind(format!("forum-{uid}"))
        .bind(format!("forum-{uid}@example.test"))
        .bind(hash)
        .bind(role)
        .bind(identity::now() - 30 * 86_400)
        .execute(db.executor())
        .await
        .unwrap();
}

async fn actor(db: &Database, uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: scalar(
            db,
            &format!("SELECT credentials_version FROM gonof_common_member WHERE uid={uid}"),
        )
        .await,
        api_key: None,
    }
}

async fn forum(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
    )
    .await
}

fn topic(fid: i64, title: &str) -> NewThread {
    NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: title.into(),
        body: format!("Body of {title}"),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

/// Loosen the posting quota enough (the fixture defaults to a new site's quota).
async fn generous_quotas(db: &Database) {
    for (key, value) in [
        ("settings.posting.daily_threads", "1000"),
        ("settings.posting.daily_replies", "10000"),
    ] {
        set_setting(db, key, value).await.unwrap();
    }
}

fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL)
}

async fn change(
    editor: &ContentEditor,
    actor: Actor,
    pid: i64,
    action: ContentAction,
) -> Result<(), ContentWriteError> {
    let post = editor.editable(actor, pid).await?;
    editor
        .change(
            actor,
            ChangeRequest {
                pid,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                request_key: identity::random_token(),
                reason: "Forum management test".into(),
                action,
            },
        )
        .await
        .map(drop)
}

struct Http {
    f: TestFixture,
    app: Router,
    identity: Identity,
}

impl Http {
    fn new(f: TestFixture) -> Self {
        let identity = Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, identity }
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        self.identity
            .login(&format!("forum-{uid}"), PASSWORD)
            .await
            .unwrap()
    }

    async fn page(&self, path: &str, grant: Option<&SessionGrant>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(grant) = grant {
            request = request.header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn form(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String, Option<String>) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &grant.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("HX-Request", "false")
            .header(header::ORIGIN, TEST_SITE_URL)
            .header(header::HOST, host_of(TEST_SITE_URL))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            )
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap(), location)
    }
}

/// A parent forum's moderator manages its child forums but not other top-level forums; a child forum's moderator
/// does not manage the parent forum. Covers write authorization (hide, move, report review) and read-only checks
/// (the thread page's moderation entry, admin drop-downs, content list, logs, move targets).
#[tokio::test]
async fn parent_forum_moderators_manage_their_child_forums() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    let showcase = forum(&db, "showcase").await;
    let ai = forum(&db, "ai").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    // 9961 manages showcase (parent of ai), 9962 manages daily, 9963 manages only the child forum ai, 9964 is the
    // author.
    for (uid, role) in [(9961, 3), (9962, 3), (9963, 3), (9964, 0)] {
        member(&db, uid, role).await;
    }
    execute(
        &db,
        &format!(
            "INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9961,{showcase}),(9962,{daily}),(9963,{ai})"
        ),
    )
    .await;
    let now = identity::now();
    let writer = writer(&db);
    let in_child = writer
        .publish_at(9964, topic(ai, "Child forum thread"), now)
        .await
        .unwrap();
    let in_daily = writer
        .publish_at(9964, topic(daily, "Daily thread"), now)
        .await
        .unwrap();
    let in_parent = writer
        .publish_at(9964, topic(showcase, "Parent forum thread"), now)
        .await
        .unwrap();

    // Read-only checks.
    for (uid, fid, expected) in [
        (9961, ai, true),
        (9961, showcase, true),
        (9961, daily, false),
        (9962, ai, false),
        (9963, ai, true),
        (9963, showcase, false),
    ] {
        assert_eq!(
            identity::can_manage_forum(&db, uid, fid).await.unwrap(),
            expected,
            "uid {uid} fid {fid}"
        );
    }
    let parent = actor(&db, 9961).await;
    let mut dropdown: Vec<i64> = manage::forums(&db, parent)
        .await
        .unwrap()
        .into_iter()
        .map(|forum| forum.fid)
        .collect();
    dropdown.sort_unstable();
    assert_eq!(dropdown, {
        let mut both = vec![showcase, ai];
        both.sort_unstable();
        both
    });
    let mut targets: Vec<i64> = organization::managed_forums(&db, parent)
        .await
        .unwrap()
        .into_iter()
        .map(|forum| forum.fid)
        .collect();
    targets.sort_unstable();
    assert_eq!(targets, dropdown, "move targets match the drop-down");
    let listed: Vec<i64> = manage::content(&db, parent, &manage::ContentQuery::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .map(|post| post.tid)
        .collect();
    assert!(listed.contains(&in_child.tid) && listed.contains(&in_parent.tid));
    assert!(
        !listed.contains(&in_daily.tid),
        "does not manage another top-level forum"
    );
    assert!(manage::post(&db, parent, in_child.pid, 1).await.is_ok());
    assert!(manage::post(&db, parent, in_daily.pid, 1).await.is_err());

    // Write authorization: hide (locks the appointment rows of this forum and its parent forum inside the
    // transaction).
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86_400);
    change(&editor, parent, in_child.pid, ContentAction::Hide)
        .await
        .expect("a parent-forum moderator hides a thread in a child forum");
    change(&editor, parent, in_child.pid, ContentAction::Unhide)
        .await
        .unwrap();
    let daily_moderator = actor(&db, 9962).await;
    assert!(
        change(&editor, daily_moderator, in_child.pid, ContentAction::Hide)
            .await
            .is_err(),
        "a moderator of another top-level forum cannot"
    );
    let child_moderator = actor(&db, 9963).await;
    assert!(
        change(&editor, child_moderator, in_parent.pid, ContentAction::Hide)
            .await
            .is_err(),
        "a child-forum moderator cannot manage the parent forum"
    );
    // Logs: a parent-forum moderator sees moderation records in child forums; a moderator of another top-level forum
    // does not.
    let target = format!("PID {}", in_child.pid);
    let (logs, _) = manage::logs(&db, parent, 1).await.unwrap();
    assert!(logs.iter().any(|entry| entry.target == target), "{target}");
    let (daily_logs, _) = manage::logs(&db, daily_moderator, 1).await.unwrap();
    assert!(!daily_logs.iter().any(|entry| entry.target == target));

    // Move: only allowed when both source and target are under the moderator's jurisdiction.
    let thread_version = |tid: i64| {
        let db = db.clone();
        async move {
            scalar(
                &db,
                &format!("SELECT version FROM gonof_forum_thread WHERE tid={tid}"),
            )
            .await
        }
    };
    organization::move_thread(
        &db,
        parent,
        in_child.tid,
        showcase,
        thread_version(in_child.tid).await,
        &identity::random_token(),
        "Move to the parent forum",
    )
    .await
    .expect("move from the child forum to the parent forum");
    assert!(matches!(
        organization::move_thread(
            &db,
            parent,
            in_child.tid,
            daily,
            thread_version(in_child.tid).await,
            &identity::random_token(),
            "Move elsewhere",
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));

    // Report review: a parent-forum moderator handles reports in child forums.
    let in_child2 = writer
        .publish_at(9964, topic(ai, "Another child forum thread"), now)
        .await
        .unwrap();
    let reporter = actor(&db, 9962).await;
    let report = moderation::submit_report(
        &db,
        reporter,
        &moderation::ReportTarget {
            target_type: "thread".into(),
            target_id: in_child2.tid,
        },
        "Report reason",
        &identity::random_token(),
        now,
        10,
    )
    .await
    .unwrap();
    assert!(
        moderation::report_list(&db, parent, true, "pending", 0)
            .await
            .unwrap()
            .iter()
            .any(|listed| listed.id == report),
        "the parent-forum moderator's queue has the child forum's report"
    );
    moderation::resolve_report(&db, parent, report, "dismissed", "Reviewed", now)
        .await
        .expect("a parent-forum moderator resolves the child forum's report");
    f.cleanup().await.unwrap();
}

/// Forum statistics count only public content readable by anonymous visitors, with parent forums including child
/// forums; the directory page groups by parent and child, does not list blocked forums, and shows statistics
/// according to the switch; the admin forum list is grouped and shows statistics; the "Forum settings" block takes effect
/// immediately after saving; statistics are cached per segment.
#[tokio::test]
async fn forum_statistics_directory_page_and_settings_block() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    let showcase = forum(&db, "showcase").await;
    let ai = forum(&db, "ai").await;
    let hosting = forum(&db, "hosting").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET visibility='members' WHERE fid={hosting}"),
    )
    .await;
    member(&db, ADMIN, 1).await;
    member(&db, 9971, 0).await;
    let now = identity::now();
    let yesterday = now - 2 * 86_400;
    let writer = writer(&db);
    // showcase: 2 today, 1 two days ago; ai: 1 thread + 1 reply today, plus 1 withdrawn; hosting (members-only) 1.
    for title in ["Showcase one", "Showcase two"] {
        writer
            .publish_at(9971, topic(showcase, title), now)
            .await
            .unwrap();
    }
    writer
        .publish_at(9971, topic(showcase, "Old showcase thread"), yesterday)
        .await
        .unwrap();
    let ai_thread = writer
        .publish_at(9971, topic(ai, "AI thread"), now)
        .await
        .unwrap();
    writer
        .reply_at(
            9971,
            NewReply {
                tid: ai_thread.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A quick reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now,
        )
        .await
        .unwrap();
    let withdrawn = writer
        .publish_at(9971, topic(ai, "Withdrawn thread"), yesterday)
        .await
        .unwrap();
    execute(
        &db,
        &format!(
            "UPDATE gonof_forum_thread SET deleted_at={now},cache_revision=cache_revision+1 WHERE tid={}",
            withdrawn.tid
        ),
    )
    .await;
    execute(
        &db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at={now} WHERE first_tid={}",
            withdrawn.tid
        ),
    )
    .await;
    writer
        .publish_at(9971, topic(hosting, "Members-only thread"), now)
        .await
        .unwrap();

    let statistics = content::forum_statistics_at(&db, now, 480).await.unwrap();
    let count = |slug: &str| statistics.forums.get(slug).copied().unwrap_or_default();
    assert_eq!(
        count("showcase"),
        content::ForumCount {
            threads: 3,
            today: 2
        }
    );
    assert_eq!(
        count("ai"),
        content::ForumCount {
            threads: 1,
            today: 2
        },
        "a withdrawn thread is not counted"
    );
    assert_eq!(
        count("hosting"),
        content::ForumCount::default(),
        "members-only forums are not counted"
    );
    assert_eq!(
        statistics.total(["showcase", "ai"]),
        content::ForumCount {
            threads: 4,
            today: 4
        }
    );
    let _ = daily;

    let h = Http::new(f);
    // Anonymous directory page: grouping, parent-forum totals, members-only forums not listed.
    let (status, page) = h.page("/forums", None).await;
    assert_eq!(status, StatusCode::OK);
    let showcase_at = page.find("data-forum=\"showcase\"").unwrap();
    let ai_at = page.find("data-forum=\"ai\"").unwrap();
    assert!(
        showcase_at < ai_at,
        "the child forum is listed inside its parent"
    );
    let after_showcase = &page[showcase_at..ai_at];
    // Each count is split into its number and word (docs/skins.md, "Counts"), reading "4 threads" and "Today 4".
    assert!(
        after_showcase.contains(
            "<span class=\"stat__value\">4</span> <span class=\"stat__label\">threads</span>"
        ) && after_showcase.contains(
            "<span class=\"stat__label\">Today</span> <span class=\"stat__value\">4</span>"
        ),
        "the parent forum includes its child forum: {after_showcase}"
    );
    assert!(page[ai_at..].contains(
        "<span class=\"stat__value\">1</span> <span class=\"stat__label\">thread</span>"
    ));
    assert!(
        !page.contains("data-forum=\"hosting\""),
        "anonymous visitors do not see members-only forums"
    );
    assert!(page.contains("<link rel=\"canonical\""));
    // Members see members-only forums but without numbers; blocked forums are not listed.
    let reader = h.login(9971).await;
    gono_forum_web::interaction::forum_filter::set(
        &h.f.db,
        &h.f.cache,
        actor(&h.f.db, 9971).await,
        showcase,
        Some(FilterMode::Block),
        &identity::random_token(),
    )
    .await
    .unwrap();
    let (_, page) = h.page("/forums", Some(&reader)).await;
    let hosting_at = page.find("data-forum=\"hosting\"").expect("members see it");
    let hosting_item = &page[hosting_at..page[hosting_at..].find("</li>").unwrap() + hosting_at];
    assert!(!hosting_item.contains("data-forum-count"), "{hosting_item}");
    assert!(
        !page.contains("data-forum=\"showcase\"") && !page.contains("data-forum=\"ai\""),
        "blocking the parent forum also hides its child forum"
    );

    // Statistics are cached per segment: posts made within the segment do not change the directory page's numbers;
    // only clearing the process cache recomputes them.
    h.f.global.clear_local();
    let (_, before) = h.page("/forums", None).await;
    writer
        .publish_at(9971, topic(ai, "New thread within the segment"), now)
        .await
        .unwrap();
    let (_, cached) = h.page("/forums", None).await;
    assert_eq!(
        before.matches("<span>4 threads</span>").count(),
        cached.matches("<span>4 threads</span>").count(),
        "the same segment hits the process cache"
    );
    h.f.global.clear_local();
    if !h.f.cache.enabled() {
        let (_, fresh) = h.page("/forums", None).await;
        assert!(
            fresh.contains("<span>5 threads</span>"),
            "recomputed after clearing"
        );
    }

    // Admin area: the forum settings block and the grouped list.
    let admin = h.login(ADMIN).await;
    let (status, manage_page) = h.page("/manage/forums", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(manage_page.contains("name=\"directory_statistics\" value=\"1\" checked"));
    let row = |page: &str, fid: i64| {
        let at = page.find(&format!("data-forum-row=\"{fid}\"")).unwrap();
        page[at..at + page[at..].find("</li>").unwrap()].to_owned()
    };
    assert!(row(&manage_page, hosting).contains("Not counted"));
    assert!(
        row(&manage_page, ai).contains("└"),
        "the child forum is indented"
    );
    assert!(row(&manage_page, showcase).contains(">View</a>"));
    let showcase_row = manage_page
        .find(&format!("data-forum-row=\"{showcase}\""))
        .unwrap();
    let ai_row = manage_page
        .find(&format!("data-forum-row=\"{ai}\""))
        .unwrap();
    let daily_row = manage_page
        .find(&format!("data-forum-row=\"{daily}\""))
        .unwrap();
    assert!(
        daily_row < showcase_row && showcase_row < ai_row,
        "grouped by parent and child"
    );
    let (status, _, location) = h.form("/manage/forums/settings", &admin, &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/manage/forums?saved=settings"));
    assert_eq!(
        scalar(
            &h.f.db,
            "SELECT svalue='false' FROM gonof_common_settings WHERE skey='settings.forum.directory_statistics'"
        )
        .await,
        1
    );
    let (_, saved) = h.page("/manage/forums?saved=settings", Some(&admin)).await;
    assert!(saved.contains("Forum settings saved."));
    assert!(!saved.contains("name=\"directory_statistics\" value=\"1\" checked"));
    let (_, page) = h.page("/forums", None).await;
    assert!(
        !page.contains("data-forum-count"),
        "the directory page hides statistics once switched off"
    );
    assert!(page.contains("data-forum=\"showcase\""));
    // An ordinary member cannot save forum settings.
    let (status, _, _) = h
        .form(
            "/manage/forums/settings",
            &reader,
            &[("directory_statistics", "1")],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    h.f.cleanup().await.unwrap();
}

/// Deleting an empty forum also cleans up appointments, follows and filters; it cannot be deleted when it has
/// threads, has child forums or is a merge target. Merging is queued as an admin task that moves threads in batches
/// and then disables the source forum; the same source forum cannot be queued twice; after the lease expires (the
/// process exited midway) the next round continues.
#[tokio::test]
async fn deleting_empty_forums_and_merging_through_the_task_queue() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    member(&db, ADMIN, 1).await;
    member(&db, 9981, 3).await;
    member(&db, 9982, 0).await;
    let admin = actor(&db, ADMIN).await;
    let daily = forum(&db, "daily").await;
    let meta = forum(&db, "meta").await;
    let releases = forum(&db, "releases").await;

    // Delete: first create an empty forum and attach appointments, follows and filters.
    let empty = organization::save_forum(
        &db,
        admin,
        &organization::ForumInput {
            name: "Scratch forum".into(),
            slug: "scratch".into(),
            status: 1,
            visibility: "public".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    execute(
        &db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9981,{empty})"),
    )
    .await;
    gono_forum_web::interaction::forum_filter::set(
        &db,
        &f.cache,
        actor(&db, 9982).await,
        empty,
        Some(FilterMode::Hide),
        &identity::random_token(),
    )
    .await
    .unwrap();
    execute(
        &db,
        &format!(
            "INSERT INTO gonof_home_forum_follow(uid,fid,dateline) VALUES(9982,{empty},{})",
            identity::now()
        ),
    )
    .await;
    let version = |fid: i64| {
        let db = db.clone();
        async move {
            scalar(
                &db,
                &format!("SELECT version FROM gonof_forum_forum WHERE fid={fid}"),
            )
            .await
        }
    };
    assert!(matches!(
        organization::delete_forum(&db, admin, empty, version(empty).await, "", 86_400).await,
        Err(ContentWriteError::Validation(_))
    ));
    assert!(matches!(
        organization::delete_forum(
            &db,
            actor(&db, 9981).await,
            empty,
            version(empty).await,
            "Delete",
            86_400
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    organization::delete_forum(
        &db,
        admin,
        empty,
        version(empty).await,
        "Created by mistake",
        86_400,
    )
    .await
    .unwrap();
    for table in [
        "gonof_forum_forum",
        "gonof_forum_moderator",
        "gonof_home_forum_follow",
        "gonof_home_forum_filter",
    ] {
        assert_eq!(
            scalar(
                &db,
                &format!("SELECT COUNT(*) FROM {table} WHERE fid={empty}")
            )
            .await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='organization.forum.delete'"
        )
        .await,
        1
    );
    // A forum with threads or child forums cannot be deleted.
    let now = identity::now();
    let writer = writer(&db);
    writer
        .publish_at(9982, topic(daily, "Daily"), now)
        .await
        .unwrap();
    assert!(matches!(
        organization::delete_forum(&db, admin, daily, version(daily).await, "Delete", 86_400).await,
        Err(ContentWriteError::Validation(message)) if message.contains("still has threads")
    ));
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={meta} WHERE fid={releases}"),
    )
    .await;
    assert!(matches!(
        organization::delete_forum(&db, admin, meta, version(meta).await, "Delete", 86_400).await,
        Err(ContentWriteError::Validation(message)) if message.contains("still has child forums")
    ));
    assert!(matches!(
        organization::request_merge(&db, admin, meta, daily, version(meta).await, "Merge").await,
        Err(ContentWriteError::Validation(message)) if message.contains("with child forums cannot be merged")
    ));

    // Merge: releases (101 threads, more than one batch) into daily.
    let mut moved = Vec::new();
    for index in 0..=organization::MERGE_BATCH {
        moved.push(
            writer
                .publish_at(9982, topic(releases, &format!("Release {index}")), now)
                .await
                .unwrap()
                .tid,
        );
    }
    let eventid = organization::request_merge(
        &db,
        admin,
        releases,
        daily,
        version(releases).await,
        "Merge into daily",
    )
    .await
    .unwrap();
    assert!(matches!(
        organization::request_merge(
            &db,
            admin,
            releases,
            meta,
            version(releases).await,
            "Merge again"
        )
        .await,
        Err(ContentWriteError::Conflict(_))
    ));
    let hosting = forum(&db, "hosting").await;
    assert!(matches!(
        organization::request_merge(&db, admin, hosting, releases, version(hosting).await, "Merge into a forum being merged").await,
        Err(ContentWriteError::Validation(message)) if message.contains("is itself being merged")
    ));
    assert!(matches!(
        organization::delete_forum(
            &db,
            admin,
            daily,
            version(daily).await,
            "Delete the target",
            86_400
        )
        .await,
        Err(ContentWriteError::Validation(_))
    ));
    assert_eq!(
        organization::pending_merge(&db, releases).await.unwrap(),
        Some((daily, eventid))
    );
    // Simulate the process exiting midway: the task is in processing, the lease has expired, and the next round
    // claims it as usual.
    execute(
        &db,
        &format!(
            "UPDATE gonof_common_outbox SET status='processing',lease_token='gone',lease_until=1 WHERE eventid={eventid}"
        ),
    )
    .await;
    let report = organization::process_merges(&db, &h_global(&f), identity::now())
        .await
        .unwrap();
    assert_eq!(report.moved, moved.len() as u64);
    assert_eq!(report.finished, 1);
    let tids = moved
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_thread WHERE fid={daily} AND tid IN ({tids})"
            )
        )
        .await,
        moved.len() as i64,
        "thread ids stay and their forum copies point at the target"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT COUNT(*) FROM gonof_forum_post WHERE fid={releases}")
        )
        .await,
        0,
        "post forum ids follow through the foreign key cascade"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT status FROM gonof_forum_forum WHERE fid={releases}")
        )
        .await,
        0,
        "the source forum is disabled after the move"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT status='sent' FROM gonof_common_outbox WHERE eventid={eventid}")
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type IN ('organization.forum.merge','organization.forum.merge.done')"
        )
        .await,
        2
    );
    assert_eq!(
        organization::pending_merge(&db, releases).await.unwrap(),
        None
    );
    assert_eq!(
        organization::process_merges(&db, &h_global(&f), identity::now())
            .await
            .unwrap(),
        organization::MergeReport::default(),
        "nothing to do without tasks"
    );
    // The merged source forum is now empty: it can be deleted (moving it out from under meta first is not required;
    // it has no child forums).
    organization::delete_forum(
        &db,
        admin,
        releases,
        version(releases).await,
        "Delete after merging",
        86_400,
    )
    .await
    .unwrap();

    // Admin pages: the delete and merge forms, the merge-in-progress notice, the "Forum" label on the task page.
    let h = Http::new(f);
    let admin_session = h.login(ADMIN).await;
    let (_, page) = h
        .page(&format!("/manage/forums?fid={meta}"), Some(&admin_session))
        .await;
    assert!(page.contains(&format!("action=\"/manage/forums/{meta}/delete\"")));
    assert!(page.contains(&format!("action=\"/manage/forums/{meta}/merge\"")));
    let current = version(hosting).await;
    let (status, _, location) = h
        .form(
            &format!("/manage/forums/{hosting}/merge"),
            &admin_session,
            &[
                ("version", &current.to_string()),
                ("target_fid", &daily.to_string()),
                ("reason", "Merge into daily"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location.as_deref(),
        Some(format!("/manage/forums?fid={hosting}&saved=merging").as_str())
    );
    let (_, page) = h
        .page(
            &format!("/manage/forums?fid={hosting}&saved=merging"),
            Some(&admin_session),
        )
        .await;
    assert!(page.contains("Merge queued: a background task moves the threads"));
    assert!(page.contains("data-forum-merge="));
    let (_, tasks) = h.page("/manage/tasks", Some(&admin_session)).await;
    assert!(tasks.contains("forum.merge") && tasks.contains("Forum #"));
    let (status, page, _) = h
        .form(
            &format!("/manage/forums/{daily}/delete"),
            &admin_session,
            &[
                ("version", &version(daily).await.to_string()),
                ("reason", "Delete"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("This forum still has threads and cannot be deleted."));
    h.f.cleanup().await.unwrap();
}

fn h_global(f: &TestFixture) -> gono_forum_web::cache::GlobalCache {
    f.global.clone()
}

/// Level-2 pins show on all forum pages under the same parent forum and level-3 pins on all forum pages site-wide,
/// only in a separate block above the page-1 list; the forum's own pins do not go into the block; the home page does
/// not merge them; pins in forums the member has hidden do not appear; pinning and unpinning take effect in this
/// process immediately after submitting.
#[tokio::test]
async fn cross_forum_pins_show_above_page_one_of_forum_pages() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    let showcase = forum(&db, "showcase").await;
    let ai = forum(&db, "ai").await;
    let hosting = forum(&db, "hosting").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    member(&db, ADMIN, 1).await;
    member(&db, 9991, 0).await;
    let now = identity::now();
    let writer = writer(&db);
    let level2 = writer
        .publish_at(9991, topic(ai, "Level-2 pin in a child forum"), now)
        .await
        .unwrap();
    let level3 = writer
        .publish_at(9991, topic(daily, "Site-wide level-3 pin"), now)
        .await
        .unwrap();
    let level1 = writer
        .publish_at(9991, topic(ai, "Level-1 pin in a child forum"), now)
        .await
        .unwrap();
    writer
        .publish_at(
            9991,
            topic(showcase, "Ordinary thread in the parent forum"),
            now,
        )
        .await
        .unwrap();
    writer
        .publish_at(9991, topic(hosting, "Ordinary thread elsewhere"), now)
        .await
        .unwrap();
    let admin = actor(&db, ADMIN).await;
    let editor = ContentEditor::new(db.clone(), WriteConfig::default(), 86_400)
        .with_global(f.global.clone());
    for (pid, action) in [
        (level2.pid, ContentAction::Pin2),
        (level3.pid, ContentAction::Pin3),
        (level1.pid, ContentAction::Pin),
    ] {
        change(&editor, admin, pid, action).await.unwrap();
    }
    let h = Http::new(f);
    let block = |page: &str| -> String {
        page.find("data-cross-pins")
            .map(|at| page[at..at + page[at..].find("</ul>").unwrap()].to_owned())
            .unwrap_or_default()
    };
    let title = |id: i64| format!("data-thread-id=\"{id}\"");

    let (status, page) = h.page("/showcase", None).await;
    assert_eq!(status, StatusCode::OK);
    let pins = block(&page);
    // The parent forum page aggregates child forums: level-1 and level-2 pins in child forums are in the list
    // itself, and the block only has other families' level-3 pins.
    assert!(!pins.contains(&title(level2.tid)) && !pins.contains(&title(level1.tid)));
    assert!(
        page.contains(&title(level2.tid)) && page.contains(&title(level1.tid)),
        "aggregated into the parent forum's list"
    );
    assert!(
        pins.contains(&title(level3.tid)),
        "level-3 pins are site-wide"
    );

    let (_, page) = h.page("/ai", None).await;
    let pins = block(&page);
    assert!(pins.contains(&title(level3.tid)));
    assert!(
        !pins.contains(&title(level2.tid)),
        "the forum's own pins stay out of the block"
    );
    assert!(page.contains(&title(level2.tid)) && page.contains(&title(level1.tid)));

    let (_, page) = h.page("/hosting", None).await;
    let pins = block(&page);
    assert!(pins.contains(&title(level3.tid)));
    assert!(
        !pins.contains(&title(level2.tid)),
        "another family's level-2 pins are not merged"
    );

    let (_, page) = h.page("/daily", None).await;
    assert!(
        !block(&page).contains(&title(level3.tid)),
        "the forum's own level-3 pin is in its list"
    );
    let (_, page) = h.page("/", None).await;
    assert!(block(&page).is_empty(), "the home page does not merge pins");
    let (_, page) = h.page("/hosting?sort=created", None).await;
    assert!(
        block(&page).contains(&title(level3.tid)),
        "page 1 has them in every sort"
    );

    // The logged-in member hid daily: level-3 pins in daily do not appear on other forum pages.
    let reader = h.login(9991).await;
    let (_, page) = h.page("/hosting", Some(&reader)).await;
    assert!(block(&page).contains(&title(level3.tid)));
    gono_forum_web::interaction::forum_filter::set(
        &h.f.db,
        &h.f.cache,
        actor(&h.f.db, 9991).await,
        daily,
        Some(FilterMode::Hide),
        &identity::random_token(),
    )
    .await
    .unwrap();
    let (_, page) = h.page("/hosting", Some(&reader)).await;
    assert!(
        !block(&page).contains(&title(level3.tid)),
        "pins in hidden forums do not appear"
    );

    // After removing a level-3 pin it disappears immediately; a new level-3 pin appears immediately (submitting
    // cleared this process's pin index).
    change(&editor, admin, level3.pid, ContentAction::Unpin)
        .await
        .unwrap();
    let (_, page) = h.page("/hosting", None).await;
    assert!(!block(&page).contains(&title(level3.tid)));
    change(&editor, admin, level1.pid, ContentAction::Pin3)
        .await
        .unwrap();
    let (_, page) = h.page("/hosting", None).await;
    assert!(
        block(&page).contains(&title(level1.tid)),
        "visible site-wide once made level 3"
    );
    h.f.cleanup().await.unwrap();
}
