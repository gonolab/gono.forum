//! Anti-spam rules: minimum interval between threads and between replies, hourly posting limit, duplicate content
//! blocking, and external link blocking for new members (bodies, thread titles, news source links and bios); the admin
//! form "Security › Anti-spam & governance", and the errors of the web pages and the member API.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use futures_util::future::join_all;
use gono_forum_web::{
    activities::ActivitySpec,
    content::{
        self,
        antispam::DUPLICATE_SCAN_LIMIT,
        edits::{ContentEditor, EditRequest, EditablePost},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    i18n::Lang,
    identity::{self, Actor, Identity, SessionGrant},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const NOW: i64 = 1_800_050_000;
const PASSWORD: &str = "Anti-spam-fixture-password-2026!";
const ADMIN: i64 = 9910;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn set(db: &Database, key: &str, value: &str) {
    set_setting(db, key, value).await.unwrap();
}

/// A member with a verified email; `role` 1 is administrator.
async fn member(db: &Database, uid: i64, regdate: i64, role: i64) {
    let salt = SaltString::encode_b64(b"anti-spam-fixture-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, 1, ?, ?)")
        .bind(uid)
        .bind(format!("spam-{uid}"))
        .bind(format!("spam-{uid}@example.test"))
        .bind(hash)
        .bind(role)
        .bind(regdate)
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

fn topic(fid: i64, body: &str) -> NewThread {
    NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: "Anti-spam thread".into(),
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

fn reply(tid: i64, body: &str) -> NewReply {
    NewReply {
        tid,
        root_pid: None,
        reply_to_pid: None,
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

fn child(tid: i64, root: i64, body: &str) -> NewReply {
    NewReply {
        root_pid: Some(root),
        reply_to_pid: Some(root),
        ..reply(tid, body)
    }
}

/// A news thread whose source link is `source`.
fn news(fid: i64, source: &str, body: &str) -> NewThread {
    NewThread {
        activity: ActivitySpec {
            kind: 5,
            source_url: source.into(),
            source_name: "Wire".into(),
            source_author: "Reporter".into(),
            ..Default::default()
        },
        ..topic(fid, body)
    }
}

async fn forum(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
    )
    .await
}

/// The author edits the body themselves (no reason); a non-empty `reason` means a staff member editing on their behalf.
fn edit_request(pid: i64, body: &str, version: (i64, i64), reason: &str) -> EditRequest {
    EditRequest {
        pid,
        expected_post_version: version.0,
        expected_thread_version: version.1,
        title: None,
        body: Some(body.into()),
        request_key: identity::random_token(),
        reason: reason.into(),
        visibility: None,
    }
}

/// The author changes the title (and keeps the body) of an opening post.
fn title_request(pid: i64, title: &str, version: (i64, i64)) -> EditRequest {
    EditRequest {
        title: Some(title.into()),
        body: None,
        ..edit_request(pid, "", version, "")
    }
}

fn versions(post: EditablePost) -> (i64, i64) {
    (post.version, post.thread_version)
}

fn quota(result: Result<Published, ContentWriteError>, expected: &str) {
    match result {
        Err(ContentWriteError::Quota(message)) => assert!(message.contains(expected), "{message}"),
        other => panic!("expected the quota error {expected:?}: {other:?}"),
    }
}

fn duplicate(result: Result<Published, ContentWriteError>) {
    match result {
        Err(ContentWriteError::Validation(message)) => {
            assert_eq!(
                message,
                "You recently posted the same text. Please do not post it again"
            )
        }
        other => panic!("expected the duplicate content error: {other:?}"),
    }
}

fn restricted(result: Result<Published, ContentWriteError>, days: i64) {
    match result {
        Err(ContentWriteError::Restricted(message)) => {
            assert_eq!(message, Lang::EN.editor_error_links_days(days))
        }
        other => panic!("expected the new member link error: {other:?}"),
    }
}

fn below_level(result: Result<Published, ContentWriteError>) {
    match result {
        Err(ContentWriteError::Restricted(message)) => assert!(
            message.starts_with("You can post links to other sites once you reach"),
            "{message}"
        ),
        other => panic!("expected the link level error: {other:?}"),
    }
}

/// All four rules are checked inside the posting transaction against the settings read by the write gate: database
/// changes take effect immediately, and concurrency and replay are treated consistently with quotas.
#[tokio::test]
async fn rules_are_checked_inside_the_write_transaction() {
    let services = TestFixture::migrated().await.unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    let fid = scalar(&db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    // Loosen the quota so this test's posts do not hit the daily quota first.
    set(&db, "settings.posting.daily_threads", "1000").await;
    set(&db, "settings.posting.daily_replies", "10000").await;
    let old = NOW - 30 * 86_400;
    let fresh = NOW - 100;
    for (uid, regdate, role) in [
        (ADMIN, fresh, 1),
        (9911, old, 0),
        (9912, old, 0),
        (9913, old, 0),
        (9914, old, 0),
        (9915, fresh, 0),
        (9917, old, 0),
    ] {
        member(&db, uid, regdate, role).await;
    }
    let writer =
        ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL);

    // The seed values are all off: rapid posting, identical bodies and new members posting external links all work as
    // usual.
    let base = writer
        .publish_at(9911, topic(fid, "the same body"), NOW)
        .await
        .unwrap();
    let other = writer
        .publish_at(9911, topic(fid, "the same body"), NOW)
        .await
        .unwrap();
    let tid = base.tid;
    writer
        .reply_at(
            9915,
            reply(tid, "[not blocked by default](https://spam.example)"),
            NOW,
        )
        .await
        .unwrap();

    // Minimum interval between two threads: only the member's previous thread counts, retries with the same key replay
    // as usual, and replies are not affected by the thread interval.
    set(&db, "settings.posting.thread_interval_seconds", "60").await;
    let t0 = NOW + 1_000;
    let first = topic(fid, "interval one");
    let published = writer.publish_at(9912, first.clone(), t0).await.unwrap();
    quota(
        writer
            .publish_at(9912, topic(fid, "interval two"), t0 + 59)
            .await,
        "You are posting threads too fast",
    );
    assert_eq!(
        writer.publish_at(9912, first, t0 + 59).await.unwrap(),
        Published {
            replayed: true,
            ..published
        },
        "the same request within the interval replays the first result"
    );
    writer
        .publish_at(9912, topic(fid, "interval three"), t0 + 60)
        .await
        .unwrap();
    writer
        .reply_at(
            9912,
            reply(tid, "the thread interval does not apply to replies"),
            t0 + 60,
        )
        .await
        .unwrap();
    // The member row lock queues posts by the same member, so only one of the concurrent submissions passes.
    let results = join_all(
        (0..6).map(|i| writer.publish_at(9912, topic(fid, &format!("concurrent {i}")), t0 + 200)),
    )
    .await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    for result in results.into_iter().filter(Result::is_err) {
        quota(result, "You are posting threads too fast");
    }

    // Minimum interval between two replies, including nested replies.
    set(&db, "settings.posting.reply_interval_seconds", "30").await;
    let root = writer
        .reply_at(9912, reply(tid, "reply one"), t0 + 300)
        .await
        .unwrap();
    quota(
        writer
            .reply_at(
                9912,
                child(tid, root.pid, "nested reply too soon"),
                t0 + 329,
            )
            .await,
        "You are replying too fast",
    );
    writer
        .reply_at(9912, child(tid, root.pid, "nested reply"), t0 + 330)
        .await
        .unwrap();
    set(&db, "settings.posting.thread_interval_seconds", "0").await;
    set(&db, "settings.posting.reply_interval_seconds", "0").await;

    // Hourly posting limit: threads and replies combined, over a rolling last hour; administrators are not limited.
    set(&db, "settings.posting.hourly_limit", "3").await;
    let h = NOW + 5_000;
    let own = writer
        .publish_at(9913, topic(fid, "hourly one"), h)
        .await
        .unwrap();
    writer
        .reply_at(9913, reply(own.tid, "hourly two"), h + 1)
        .await
        .unwrap();
    let floor = writer
        .reply_at(9913, reply(own.tid, "hourly three"), h + 2)
        .await
        .unwrap();
    quota(
        writer
            .reply_at(9913, child(own.tid, floor.pid, "hourly four"), h + 3)
            .await,
        "You have reached the hourly posting limit",
    );
    quota(
        writer
            .publish_at(9913, topic(fid, "hourly four"), h + 3_599)
            .await,
        "You have reached the hourly posting limit",
    );
    writer
        .publish_at(9913, topic(fid, "first post left the window"), h + 3_600)
        .await
        .unwrap();
    for i in 0..5 {
        writer
            .publish_at(ADMIN, topic(fid, &format!("admin {i}")), h)
            .await
            .unwrap();
    }
    set(&db, "settings.posting.hourly_limit", "0").await;

    // Duplicate content: only compared with the member's own posts of the same kind, identical after trimming
    // surrounding whitespace; posts outside the look-back period and count, and the member's own withdrawn posts, do
    // not count.
    set(&db, "settings.posting.duplicate_check", "true").await;
    let d = NOW + 10_000;
    writer
        .reply_at(9914, reply(tid, "the same sentence"), d)
        .await
        .unwrap();
    duplicate(
        writer
            .reply_at(9914, reply(tid, "  the same sentence\n"), d + 10)
            .await,
    );
    duplicate(
        writer
            .reply_at(9914, reply(other.tid, "the same sentence"), d + 20)
            .await,
    );
    writer
        .reply_at(9911, reply(tid, "the same sentence"), d + 20)
        .await
        .expect("other members' posts do not count");
    writer
        .publish_at(9914, topic(fid, "the same sentence"), d + 30)
        .await
        .expect("threads are only compared with threads");
    duplicate(
        writer
            .publish_at(9914, topic(fid, "the same sentence"), d + 40)
            .await,
    );
    writer
        .reply_at(9914, reply(tid, "the same sentence"), d + 3_601)
        .await
        .expect("posts before the 60-minute look-back do not count");
    let withdrawn = writer
        .reply_at(9914, reply(tid, "posted again after withdrawal"), d + 4_000)
        .await
        .unwrap();
    execute(
        &db,
        &format!(
            "UPDATE gonof_forum_post SET deleted_at={} WHERE pid={}",
            d + 4_001,
            withdrawn.pid
        ),
    )
    .await;
    writer
        .reply_at(9914, reply(tid, "posted again after withdrawal"), d + 4_010)
        .await
        .expect("the member's own withdrawn posts do not count");
    let hidden = writer
        .reply_at(9914, reply(tid, "hidden by moderation"), d + 4_020)
        .await
        .unwrap();
    execute(
        &db,
        &format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            hidden.pid
        ),
    )
    .await;
    duplicate(
        writer
            .reply_at(9914, reply(tid, "hidden by moderation"), d + 4_030)
            .await,
    );
    writer
        .reply_at(9914, reply(tid, "the earliest sentence"), d + 5_000)
        .await
        .unwrap();
    for i in 0..DUPLICATE_SCAN_LIMIT {
        writer
            .reply_at(
                9914,
                reply(tid, &format!("later sentence {i}")),
                d + 5_001 + i,
            )
            .await
            .unwrap();
    }
    writer
        .reply_at(9914, reply(tid, "the earliest sentence"), d + 5_100)
        .await
        .expect("only a fixed number of recent posts are compared");
    set(&db, "settings.posting.duplicate_check", "false").await;

    // External links from new members: rejected during the probation period, with days computed from the current
    // setting; in-site URLs do not count; established members and administrators are not limited.
    set(&db, "settings.posting.new_member_link_block", "true").await;
    let l = NOW + 20_000;
    restricted(
        writer
            .publish_at(9915, topic(fid, "see [here](https://spam.example/buy)"), l)
            .await,
        3,
    );
    restricted(
        writer
            .reply_at(
                9915,
                reply(tid, "plain text http://spam.example counts too"),
                l,
            )
            .await,
        3,
    );
    restricted(
        writer
            .reply_at(
                9915,
                child(tid, root.pid, "<a href=\"//spam.example\">by hand</a>"),
                l,
            )
            .await,
        3,
    );
    set(&db, "settings.posting.new_member_link_days", "1").await;
    restricted(
        writer
            .publish_at(9915, topic(fid, "`https://spam.example`"), l)
            .await,
        1,
    );
    set(&db, "settings.posting.new_member_link_days", "3").await;
    let inside = writer
        .publish_at(
            9915,
            topic(
                fid,
                &format!("[rules](/rules) and {TEST_SITE_URL}/thread-{tid}"),
            ),
            l,
        )
        .await
        .expect("addresses on this site are not external links");
    writer
        .publish_at(9917, topic(fid, "https://spam.example/old"), l)
        .await
        .expect("established members are not limited");
    writer
        .publish_at(ADMIN, topic(fid, "https://spam.example/admin"), l)
        .await
        .expect("administrators are not limited");

    // Editing: an author in the probation period cannot add external links through editing, existing ones do not count
    // as new; administrators editing others' posts are not limited.
    let editor = ContentEditor::new(
        db.clone(),
        WriteConfig::default(),
        services.config.media.orphan_seconds,
    )
    .with_site_url(TEST_SITE_URL);
    let newbie = actor(&db, 9915).await;
    let edit = edit_request;
    let current = versions(
        editor
            .editable_at(newbie, inside.pid, l + 10)
            .await
            .unwrap(),
    );
    restricted(
        editor
            .edit_at(
                newbie,
                edit(
                    inside.pid,
                    "[rules](/rules) plus https://spam.example/x",
                    current,
                    "",
                ),
                l + 20,
            )
            .await,
        3,
    );
    set(&db, "settings.posting.new_member_link_block", "false").await;
    let linked = writer
        .reply_at(9915, reply(tid, "existing https://ok.example/a"), l + 30)
        .await
        .unwrap();
    set(&db, "settings.posting.new_member_link_block", "true").await;
    let current = versions(
        editor
            .editable_at(newbie, linked.pid, l + 40)
            .await
            .unwrap(),
    );
    editor
        .edit_at(
            newbie,
            edit(
                linked.pid,
                "existing https://ok.example/a with a typo fixed",
                current,
                "",
            ),
            l + 40,
        )
        .await
        .expect("existing external links do not count as added");
    let current = versions(
        editor
            .editable_at(newbie, linked.pid, l + 50)
            .await
            .unwrap(),
    );
    restricted(
        editor
            .edit_at(
                newbie,
                edit(
                    linked.pid,
                    "existing https://ok.example/a plus https://spam.example/b",
                    current,
                    "",
                ),
                l + 50,
            )
            .await,
        3,
    );
    let admin = actor(&db, ADMIN).await;
    editor
        .edit_at(
            admin,
            edit(
                linked.pid,
                "existing https://ok.example/a with the source https://source.example",
                current,
                "added the source",
            ),
            l + 60,
        )
        .await
        .expect("administrators editing others' posts are not limited");

    // Once the probation period after registration has passed, posting works.
    writer
        .publish_at(
            9915,
            topic(fid, "https://spam.example/later"),
            fresh + 3 * 86_400,
        )
        .await
        .expect("the probation period is over");
    services.cleanup().await.unwrap();
}

/// People who manage the target forum are exempt from all four rules (docs/roles.md, "Exemptions"): moderators in
/// their appointed forums and those forums' child forums, global moderators and administrators site-wide; the same
/// applies to the external link rule when editing.
#[tokio::test]
async fn moderators_are_exempt_in_the_forums_they_manage() {
    let services = TestFixture::migrated().await.unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    let daily = forum(&db, "daily").await;
    let showcase = forum(&db, "showcase").await;
    let ai = forum(&db, "ai").await;
    execute(
        &db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    for (key, value) in [
        ("settings.posting.first_day_threads", "100"),
        ("settings.posting.first_day_replies", "100"),
        ("settings.posting.daily_threads", "1000"),
        ("settings.posting.daily_replies", "10000"),
        ("settings.posting.thread_interval_seconds", "60"),
        ("settings.posting.reply_interval_seconds", "60"),
        ("settings.posting.hourly_limit", "2"),
        ("settings.posting.duplicate_check", "true"),
        ("settings.posting.new_member_link_block", "true"),
    ] {
        set(&db, key, value).await;
    }
    let fresh = NOW - 100;
    for (uid, role) in [(9931, 3), (9932, 3), (9933, 2), (9934, 0)] {
        member(&db, uid, fresh, role).await;
    }
    execute(
        &db,
        &format!(
            "INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9931,{daily}),(9932,{showcase})"
        ),
    )
    .await;
    let writer =
        ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL);
    let spam = "Try https://spam.example/buy";

    // A moderator in an appointed forum: interval, hourly limit, duplicate check and external links are not blocked.
    let mut own = Vec::new();
    for _ in 0..3 {
        own.push(
            writer
                .publish_at(9931, topic(daily, spam), NOW)
                .await
                .expect("moderators are exempt in their own forum"),
        );
    }
    for _ in 0..3 {
        writer
            .reply_at(9931, reply(own[0].tid, spam), NOW)
            .await
            .expect("moderators replying in their own forum are exempt too");
    }
    // No exemption in other forums: external links are blocked first, then the interval.
    restricted(writer.publish_at(9931, topic(showcase, spam), NOW).await, 3);
    quota(
        writer
            .publish_at(9931, topic(showcase, "another forum"), NOW)
            .await,
        "You are posting threads too fast",
    );
    // A parent forum's moderator is also exempt in child forums, but not in other top-level forums.
    let mut child = Vec::new();
    for fid in [showcase, ai, ai] {
        child.push(
            writer
                .publish_at(9932, topic(fid, spam), NOW)
                .await
                .expect("a parent forum's moderator is exempt there and in child forums"),
        );
    }
    restricted(writer.publish_at(9932, topic(daily, spam), NOW).await, 3);
    // Global moderators are exempt site-wide; ordinary members are still blocked.
    for fid in [daily, ai] {
        writer
            .publish_at(9933, topic(fid, spam), NOW)
            .await
            .expect("global moderators are exempt site-wide");
    }
    restricted(writer.publish_at(9934, topic(daily, spam), NOW).await, 3);

    // Editing: the author adding external links in a forum they manage is not blocked, but in other forums it is.
    let editor = ContentEditor::new(
        db.clone(),
        WriteConfig::default(),
        services.config.media.orphan_seconds,
    )
    .with_site_url(TEST_SITE_URL);
    let moderator = actor(&db, 9931).await;
    let elsewhere = writer
        .publish_at(
            9931,
            topic(showcase, "no links in another forum"),
            NOW + 3_700,
        )
        .await
        .unwrap();
    let current = versions(
        editor
            .editable_at(moderator, elsewhere.pid, NOW + 3_710)
            .await
            .unwrap(),
    );
    restricted(
        editor
            .edit_at(
                moderator,
                edit_request(elsewhere.pid, spam, current, ""),
                NOW + 3_720,
            )
            .await,
        3,
    );
    let current = versions(
        editor
            .editable_at(moderator, own[0].pid, NOW + 3_730)
            .await
            .unwrap(),
    );
    editor
        .edit_at(
            moderator,
            edit_request(
                own[0].pid,
                "Try https://spam.example/buy and https://another.example",
                current,
                "",
            ),
            NOW + 3_740,
        )
        .await
        .expect("adding links to a post in their own forum is not blocked");
    let parent_moderator = actor(&db, 9932).await;
    let current = versions(
        editor
            .editable_at(parent_moderator, child[1].pid, NOW + 3_750)
            .await
            .unwrap(),
    );
    editor
        .edit_at(
            parent_moderator,
            edit_request(
                child[1].pid,
                "Try https://spam.example/buy and https://another.example",
                current,
                "",
            ),
            NOW + 3_760,
        )
        .await
        .expect("adding links to a post in a child forum is not blocked");
    services.cleanup().await.unwrap();
}

/// A news thread's source link and a thread title are shown like the body, so the link rules check them too: when
/// publishing, and for titles when editing; the source link cannot be edited, and editing the body only compares the
/// body (docs/roles.md, "Level gates").
#[tokio::test]
async fn news_source_links_and_titles_follow_the_link_rule() {
    let services = TestFixture::migrated().await.unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    let daily = forum(&db, "daily").await;
    set(&db, "settings.posting.daily_threads", "1000").await;
    set(&db, "settings.posting.first_day_threads", "1000").await;
    let old = NOW - 30 * 86_400;
    let fresh = NOW - 100;
    for (uid, regdate, role) in [
        (ADMIN, fresh, 1),
        (9941, fresh, 0),
        (9942, old, 0),
        (9943, fresh, 3),
    ] {
        member(&db, uid, regdate, role).await;
    }
    execute(
        &db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9943,{daily})"),
    )
    .await;
    let writer =
        ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL);
    let source = "https://wire.example/story";

    // Rule off: a new member's news thread with a source link to another site, and a title with an address, are
    // published; they are used for the edits below.
    let story = writer
        .publish_at(9941, news(daily, source, "No links in the body"), NOW)
        .await
        .expect("the rule is off");
    let titled = writer
        .publish_at(
            9941,
            NewThread {
                title: "Coverage at https://wire.example/live".into(),
                ..topic(daily, "No links in the body")
            },
            NOW,
        )
        .await
        .expect("the rule is off");

    // Publishing: the source link and the title count like the body; this site's addresses do not.
    set(&db, "settings.posting.new_member_link_block", "true").await;
    restricted(
        writer
            .publish_at(9941, news(daily, source, "No links in the body"), NOW + 10)
            .await,
        3,
    );
    restricted(
        writer
            .publish_at(
                9941,
                news(daily, "HTTPS://WWW.Wire.Example./other", "No links"),
                NOW + 10,
            )
            .await,
        3,
    );
    restricted(
        writer
            .publish_at(
                9941,
                NewThread {
                    title: "Cheap at http://spam.example now".into(),
                    ..topic(daily, "No links in the body")
                },
                NOW + 10,
            )
            .await,
        3,
    );
    writer
        .publish_at(
            9941,
            news(daily, &format!("{TEST_SITE_URL}/thread-1"), "Own news"),
            NOW + 20,
        )
        .await
        .expect("a source link to this site is not external");
    writer
        .publish_at(
            9941,
            NewThread {
                title: format!("See {TEST_SITE_URL}/rules first"),
                ..topic(daily, "No links in the body")
            },
            NOW + 20,
        )
        .await
        .expect("this site's addresses in a title are not external");
    writer
        .publish_at(9942, news(daily, source, "Established"), NOW + 20)
        .await
        .expect("established members are not limited");
    writer
        .publish_at(9943, news(daily, source, "Moderated"), NOW + 20)
        .await
        .expect("moderators are exempt in the forums they manage");
    // The level gate reads the same fields.
    set(&db, "settings.growth.link_level", "2").await;
    below_level(
        writer
            .publish_at(9942, news(daily, source, "Below the level"), NOW + 30)
            .await,
    );
    below_level(
        writer
            .publish_at(
                9942,
                NewThread {
                    title: "Below the level https://spam.example".into(),
                    ..topic(daily, "No links in the body")
                },
                NOW + 30,
            )
            .await,
    );
    set(&db, "settings.growth.link_level", "1").await;

    // Editing a news thread: its existing source link does not stand in the way of fixing the body, but new links in
    // the body are refused, even the source link's own address.
    let editor = ContentEditor::new(
        db.clone(),
        WriteConfig::default(),
        services.config.media.orphan_seconds,
    )
    .with_site_url(TEST_SITE_URL);
    let newbie = actor(&db, 9941).await;
    let current = versions(
        editor
            .editable_at(newbie, story.pid, NOW + 40)
            .await
            .unwrap(),
    );
    editor
        .edit_at(
            newbie,
            edit_request(story.pid, "No links in the body, typo fixed", current, ""),
            NOW + 40,
        )
        .await
        .expect("the existing source link does not count as added");
    let current = versions(
        editor
            .editable_at(newbie, story.pid, NOW + 50)
            .await
            .unwrap(),
    );
    for body in [
        "Now with https://spam.example/b",
        "Now with the source https://wire.example/story",
    ] {
        restricted(
            editor
                .edit_at(newbie, edit_request(story.pid, body, current, ""), NOW + 50)
                .await,
            3,
        );
    }
    restricted(
        editor
            .edit_at(
                newbie,
                title_request(story.pid, "Story, more at https://spam.example/t", current),
                NOW + 50,
            )
            .await,
        3,
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_news WHERE tid={} AND source_url='{source}'",
                story.tid
            )
        )
        .await,
        1,
        "the source link is unchanged"
    );

    // Editing a title: an address already there does not count, an added one does; staff editing others' titles are
    // not limited.
    let current = versions(
        editor
            .editable_at(newbie, titled.pid, NOW + 60)
            .await
            .unwrap(),
    );
    editor
        .edit_at(
            newbie,
            title_request(
                titled.pid,
                "Live coverage at https://wire.example/live",
                current,
            ),
            NOW + 60,
        )
        .await
        .expect("an address already in the title does not count as added");
    let current = versions(
        editor
            .editable_at(newbie, titled.pid, NOW + 70)
            .await
            .unwrap(),
    );
    restricted(
        editor
            .edit_at(
                newbie,
                title_request(
                    titled.pid,
                    "Live coverage at https://wire.example/live and https://spam.example",
                    current,
                ),
                NOW + 70,
            )
            .await,
        3,
    );
    let admin = actor(&db, ADMIN).await;
    editor
        .edit_at(
            admin,
            EditRequest {
                reason: "linked the original".into(),
                ..title_request(
                    titled.pid,
                    "Live coverage at https://wire.example/live, via https://other.example",
                    current,
                )
            },
            NOW + 80,
        )
        .await
        .expect("administrators editing others' titles are not limited");
    services.cleanup().await.unwrap();
}

struct Http {
    f: TestFixture,
    app: Router,
    identity: Identity,
}

impl Http {
    async fn login(&self, uid: i64) -> SessionGrant {
        self.identity
            .login(&format!("spam-{uid}"), PASSWORD)
            .await
            .unwrap()
    }

    async fn form(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String) {
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
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn page(&self, path: &str, grant: &SessionGrant) -> (StatusCode, String) {
        let request = Request::builder()
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            )
            .body(Body::empty())
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// Saves the profile form of member `uid` (created by [`member`]) with a new bio.
    async fn save_bio(&self, grant: &SessionGrant, uid: i64, bio: &str) -> (StatusCode, String) {
        let username = format!("spam-{uid}");
        self.form(
            "/my/profile",
            grant,
            &[("username", &username), ("bio", bio)],
        )
        .await
    }

    async fn bio(&self, uid: i64) -> String {
        db::query_scalar("SELECT bio FROM gonof_common_member WHERE uid = ?")
            .bind(uid)
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }

    async fn api(&self, path: &str, secret: &str, body: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

/// The admin form saves and loads the 7 items; web posting and the member API go through the same write transaction,
/// with consistent status codes and messages for errors.
#[tokio::test]
async fn settings_page_web_and_api_report_the_rules() {
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    let fid = scalar(
        &f.db,
        "SELECT fid FROM gonof_forum_forum WHERE slug='daily'",
    )
    .await;
    let now = identity::now();
    member(&f.db, ADMIN, now - 30 * 86_400, 1).await;
    member(&f.db, 9921, now - 30 * 86_400, 0).await;
    member(&f.db, 9922, now - 60, 0).await;
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
    let h = Http { f, app, identity };
    let admin = h.login(ADMIN).await;

    let (status, page) = h.page("/manage/security/posting", &admin).await;
    assert_eq!(status, StatusCode::OK);
    for field in [
        "posting_thread_interval_seconds",
        "posting_reply_interval_seconds",
        "posting_hourly_limit",
        "posting_duplicate_check",
        "posting_duplicate_window_minutes",
        "posting_new_member_link_block",
        "posting_new_member_link_days",
    ] {
        assert!(
            page.contains(&format!("name=\"{field}\"")),
            "missing {field}"
        );
    }
    // The English page shows no Chinese, such as the list of unbuilt features it used to show.
    assert!(
        !page.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c)),
        "the English page holds no Chinese text"
    );

    let (status, page) = h
        .form(
            "/manage/security/posting",
            &admin,
            &[("posting_reply_interval_seconds", "601")],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        page.contains("The minimum time between replies must be 0 to 600 seconds (0 for no limit)"),
        "{page}"
    );
    let (status, _) = h
        .form(
            "/manage/security/posting",
            &admin,
            &[
                ("posting_thread_interval_seconds", "30"),
                ("posting_reply_interval_seconds", "0"),
                ("posting_hourly_limit", "50"),
                ("posting_duplicate_check", "true"),
                ("posting_duplicate_window_minutes", "30"),
                ("posting_new_member_link_block", "true"),
                ("posting_new_member_link_days", "7"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let stored = runtime::read(&h.f.db).await.unwrap().posting;
    assert_eq!(
        (
            stored.thread_interval_seconds,
            stored.reply_interval_seconds,
            stored.hourly_limit,
            stored.duplicate_check,
            stored.duplicate_window_minutes,
            stored.new_member_link_block,
            stored.new_member_link_days,
        ),
        (30, 0, 50, true, 30, true, 7)
    );

    // Web: a new member posting an external link gets 403, with a message stating the current probation days and the
    // input kept; rapid threads get 429.
    let fid = fid.to_string();
    let newbie = h.login(9922).await;
    let (status, page) = h
        .form(
            "/new",
            &newbie,
            &[
                ("fid", &fid),
                ("title", "A new member thread"),
                ("body", "Try https://spam.example/buy"),
                ("request_key", &identity::random_token()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    assert!(
        page.contains("You can post links to other sites 7 days after signing up"),
        "{page}"
    );
    assert!(
        page.contains("Try https://spam.example/buy"),
        "the input is kept"
    );
    let veteran = h.login(9921).await;
    let topic_form = |key: &str| {
        [
            ("fid", fid.clone()),
            ("title", "An established member thread".to_owned()),
            ("body", format!("Body {key}")),
            ("request_key", key.to_owned()),
        ]
    };
    let fields = topic_form(&identity::random_token());
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (status, page) = h.form("/new", &veteran, &fields).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
    let tid = scalar(&h.f.db, "SELECT MAX(tid) FROM gonof_forum_thread").await;
    let fields = topic_form(&identity::random_token());
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (status, page) = h.form("/new", &veteran, &fields).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        page.contains("You are posting threads too fast. Please wait a moment"),
        "{page}"
    );

    // Member API: the same write transaction, 429 quota, 422 validation, 403 forbidden.
    let secret = |grant: SessionGrant| {
        let identity = &h.identity;
        async move {
            identity
                .create_api_key(&grant.session_token, PASSWORD, 86_400)
                .await
                .unwrap()
                .secret
        }
    };
    let veteran_key = secret(veteran).await;
    let (status, body) = h
        .api(
            "/api/v1/threads",
            &veteran_key,
            r#"{"forum":"daily","title":"API thread","body":"API body"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "quota");
    let path = format!("/api/v1/threads/{tid}/replies");
    let (status, body) = h.api(&path, &veteran_key, r#"{"body":"API reply"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = h.api(&path, &veteran_key, r#"{"body":"API reply"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "validation");
    assert_eq!(
        body["error"],
        "You recently posted the same text. Please do not post it again"
    );
    let newbie_key = secret(newbie).await;
    let (status, body) = h
        .api(
            &path,
            &newbie_key,
            r#"{"body":"[ad](https://spam.example)"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");
    assert_eq!(
        body["error"],
        "You can post links to other sites 7 days after signing up"
    );
    h.f.cleanup().await.unwrap();
}

/// The web form and the member API refuse a new member's news source link to another site with the body's message;
/// the card's source link is marked as user content for search engines. Bios follow the same rule when saved from the
/// profile page, exempting only staff who manage every board.
#[tokio::test]
async fn news_links_and_bios_over_the_web_and_api() {
    let f = TestFixture::migrated().await.unwrap();
    content::seed_forums(&f.db).await.unwrap();
    let fid = forum(&f.db, "daily").await.to_string();
    let now = identity::now();
    for (uid, regdate, role) in [
        (9951, now - 60, 0),
        (9952, now - 30 * 86_400, 0),
        (9953, now - 60, 2),
        (9954, now - 60, 3),
    ] {
        member(&f.db, uid, regdate, role).await;
    }
    set(&f.db, "settings.posting.new_member_link_block", "true").await;
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
    let h = Http { f, app, identity };
    let source = "https://wire.example/story";
    let news_form = |key: String| {
        [
            ("fid", fid.clone()),
            ("category", "5".to_owned()),
            ("title", "Wire story".to_owned()),
            ("body", "No links in the body".to_owned()),
            ("source_name", "Wire".to_owned()),
            ("source_url", source.to_owned()),
            ("source_author", "Reporter".to_owned()),
            ("request_key", key),
        ]
    };

    // Web: 403 with the probation message, the input kept.
    let newbie = h.login(9951).await;
    let fields = news_form(identity::random_token());
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (status, page) = h.form("/new", &newbie, &fields).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    assert!(
        page.contains(&Lang::EN.editor_error_links_days(3)),
        "{page}"
    );
    assert!(page.contains(source), "the input is kept");

    // Member API: 403 forbidden with the same message.
    let secret = h
        .identity
        .create_api_key(&newbie.session_token, PASSWORD, 86_400)
        .await
        .unwrap()
        .secret;
    let (status, body) = h
        .api(
            "/api/v1/threads",
            &secret,
            &serde_json::json!({
                "forum": "daily", "title": "API story", "body": "No links in the body", "category": 5,
                "source_url": source, "source_name": "Wire", "source_author": "Reporter",
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["error"], Lang::EN.editor_error_links_days(3));
    assert_eq!(
        scalar(&h.f.db, "SELECT COUNT(*) FROM gonof_forum_news").await,
        0
    );

    // An established member's news card links the source with nofollow ugc, like links in bodies.
    let veteran = h.login(9952).await;
    let fields = news_form(identity::random_token());
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (status, page) = h.form("/new", &veteran, &fields).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
    let tid = scalar(&h.f.db, "SELECT MAX(tid) FROM gonof_forum_news").await;
    let (status, page) = h.page(&format!("/thread-{tid}"), &veteran).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains(&format!(
            "<a href=\"{source}\" rel=\"nofollow ugc noopener noreferrer\" target=\"_blank\">"
        )),
        "{page}"
    );

    // Bios: a new member cannot add an address of another site; this site's addresses and addresses already in the
    // bio do not count.
    let (status, page) = h
        .save_bio(&newbie, 9951, "Shop at https://spam.example/buy")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    assert!(
        page.contains(&Lang::EN.editor_error_links_days(3)),
        "{page}"
    );
    assert!(
        page.contains("Shop at https://spam.example/buy"),
        "the input is kept"
    );
    assert_eq!(h.bio(9951).await, "");
    let own = format!("Read {TEST_SITE_URL}/thread-{tid}");
    let (status, page) = h.save_bio(&newbie, 9951, &own).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
    assert_eq!(h.bio(9951).await, own);
    set(&h.f.db, "settings.posting.new_member_link_block", "false").await;
    let (status, _) = h
        .save_bio(&newbie, 9951, "Writing at https://ok.example")
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    set(&h.f.db, "settings.posting.new_member_link_block", "true").await;
    let (status, page) = h
        .save_bio(&newbie, 9951, "Mostly writing at https://ok.example")
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
    let (status, page) = h
        .save_bio(
            &newbie,
            9951,
            "Mostly writing at https://ok.example and https://spam.example",
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    assert_eq!(h.bio(9951).await, "Mostly writing at https://ok.example");
    // Established members and global moderators are not limited; a moderator manages only some boards and is.
    for (uid, status) in [
        (9952, StatusCode::SEE_OTHER),
        (9953, StatusCode::SEE_OTHER),
        (9954, StatusCode::FORBIDDEN),
    ] {
        let grant = h.login(uid).await;
        let (got, page) = h
            .save_bio(&grant, uid, "Shop at https://spam.example/buy")
            .await;
        assert_eq!(got, status, "uid {uid}: {page}");
    }
    // The level gate applies to bios as well.
    set(&h.f.db, "settings.growth.link_level", "2").await;
    let (status, page) = h
        .save_bio(&veteran, 9952, "Shop at https://other.example/more")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    assert!(
        page.contains("You can post links to other sites once you reach"),
        "{page}"
    );
    h.f.cleanup().await.unwrap();
}
