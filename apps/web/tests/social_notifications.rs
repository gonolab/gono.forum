use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor},
    interaction::{
        self,
        social::{self, Relation},
    },
    notifications::{self, preferences},
    runtime,
};
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::raw_sql(sql)
        .execute(&db.pool)
        .await
        .unwrap();
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn member(db: &Database, uid: i64, admin: bool) {
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
        .bind(uid).bind(format!("social-{uid}")).bind(format!("social-{uid}@example.test")).bind(i64::from(admin)).execute(db.executor()).await.unwrap();
}
async fn follow(db: &Database, uid: i64, tid: i64, desired: bool) {
    social::set_follow_thread(db, actor(uid), tid, desired, &identity::random_token())
        .await
        .unwrap();
}
async fn notices(db: &Database, pid: i64, uid: i64) -> i64 {
    db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE pid=? AND uid=?")
        .bind(pid)
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
/// The reason recorded by the most recently abandoned content e-mail.
async fn skip_reason(db: &Database, uid: i64) -> String {
    db::query_scalar("SELECT last_error FROM gonof_common_outbox WHERE event_type='notification.email' AND aggregate_id=? AND status='failed' ORDER BY eventid DESC LIMIT 1")
        .bind(uid)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn reply(writer: &ContentWriter, tid: i64, body: &str) -> i64 {
    writer
        .reply(
            2000,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: body.into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .pid
}

#[tokio::test]
async fn subscriptions_preferences_blocking_and_channel_delivery_contract() {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    member(db, 2000, true).await;
    member(db, 2001, true).await;
    for uid in 2100..=2162 {
        member(db, uid, false).await;
    }
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let topic = writer
        .publish(
            2001,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Follow boundary thread".into(),
                body: "This thread checks subscriptions and notifications on both channels".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    for uid in 2100..=2160 {
        follow(db, uid, topic.tid, true).await;
    }
    follow(db, 2001, topic.tid, true).await;
    let key = identity::random_token();
    social::set_follow_thread(db, actor(2162), topic.tid, true, &key)
        .await
        .unwrap();
    follow(db, 2162, topic.tid, false).await;
    social::set_follow_thread(db, actor(2162), topic.tid, true, &key)
        .await
        .unwrap();
    assert!(
        !social::following(db, 2162, topic.tid, Relation::Thread)
            .await
            .unwrap(),
        "an old request must not undo a later unfollow"
    );
    assert!(matches!(
        social::set_block(db, actor(2162), 2000, true, &key).await,
        Err(ContentWriteError::Conflict(_))
    ));
    assert!(
        social::set_block(db, actor(2100), 2100, true, &identity::random_token())
            .await
            .is_err()
    );
    let default = preferences::get(db, actor(2100)).await.unwrap();
    assert_eq!(default.site, [true; 5]);
    assert_eq!(default.email, [false; 5]);
    let mut mail_only = default.clone();
    mail_only.site[3] = false;
    mail_only.email[3] = true;
    preferences::save(db, actor(2100), &mail_only)
        .await
        .unwrap();
    assert!(matches!(
        preferences::save(db, actor(2100), &default).await,
        Err(ContentWriteError::Conflict(_))
    ));
    let published = reply(
        &writer,
        topic.tid,
        "New top-level reply that mentions [@social-2101](member:2101)",
    )
    .await;
    follow(db, 2103, topic.tid, false).await;
    follow(db, 2103, topic.tid, true).await;
    follow(db, 2161, topic.tid, true).await;
    social::set_block(db, actor(2102), 2000, true, &identity::random_token())
        .await
        .unwrap();
    let report = notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    assert_eq!(report.failed, 0);
    assert!(
        report.claimed >= 2,
        "the follower fan-out takes at least two batches of 50"
    );
    // Continuing to the next page is a success and does not count toward retries; only the claim of the
    // last page stays in attempts.
    let attempts: i64 = db::query_scalar(
        "SELECT attempts FROM gonof_common_outbox WHERE event_key=? AND status='sent'",
    )
    .bind(format!("notification.content:{published}"))
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(
        attempts, 1,
        "continuing to the next page does not add to attempts"
    );
    assert_eq!(
        notices(db, published, 2100).await,
        0,
        "email is independent of site notifications"
    );
    assert_eq!(notices(db, published, 2102).await, 0);
    assert_eq!(
        notices(db, published, 2103).await,
        0,
        "unfollowing and following again does not get the earlier post"
    );
    assert_eq!(
        notices(db, published, 2161).await,
        0,
        "a follow made after the post does not get it"
    );
    assert_eq!(
        notices(db, published, 2001).await,
        1,
        "a direct reply and a follow merge into one notification"
    );
    let kind: String =
        db::query_scalar("SELECT type FROM gonof_home_notification WHERE pid=? AND uid=2101")
            .bind(published)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(kind, "mention");
    let total: i64 = db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE pid=?")
        .bind(published)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(total, 59);
    let replay = notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    assert_eq!(replay.delivered, 0);
    let queued:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='notification.email' AND aggregate_id=2100").fetch_one(db.executor()).await.unwrap();
    assert_eq!(queued, 1);
    let mail_directory = fixture.root.join("content-mail");
    let sent = gono_forum_web::test_support::deliver_mail(
        db,
        &mail_directory,
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 1);
    let entries = std::fs::read_dir(&mail_directory)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let body = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(body.contains(&format!("/posts/{published}")));
    assert!(!body.contains("New top-level reply that mentions"));
    // Current settings and blocks also apply after the email has been enqueued.
    let pending = reply(&writer, topic.tid, "Email alerts turned off after queueing").await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    let mut settings = preferences::get(db, actor(2100)).await.unwrap();
    settings.email[3] = false;
    preferences::save(db, actor(2100), &settings).await.unwrap();
    let sent = gono_forum_web::test_support::deliver_mail(
        db,
        &mail_directory,
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 0);
    assert_eq!(sent.skipped, 1);
    assert_eq!(
        skip_reason(db, 2100).await,
        "the recipient turned off this kind of email notification"
    );
    assert_eq!(
        notices(db, pending, 2103).await,
        1,
        "following again only gets later content"
    );
    assert_eq!(notices(db, pending, 2161).await, 1);
    // Different channels choose from the complete candidate set after preferences.
    let mut owner_preferences = preferences::get(db, actor(2001)).await.unwrap();
    owner_preferences.site[0] = false;
    owner_preferences.email[1] = true;
    preferences::save(db, actor(2001), &owner_preferences)
        .await
        .unwrap();
    let priority = reply(
        &writer,
        topic.tid,
        &format!("[@thread-author](member:2001) [quote](post:{})", topic.pid),
    )
    .await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    let kind: String =
        db::query_scalar("SELECT type FROM gonof_home_notification WHERE uid=2001 AND pid=?")
            .bind(priority)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(
        kind, "quote",
        "turning off replies must not swallow quotes that are still on"
    );
    let sent = gono_forum_web::test_support::deliver_mail(
        db,
        &mail_directory,
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 1);
    // A queued event is bound to its recipient email identity, not a later replacement.
    let mut settings = preferences::get(db, actor(2100)).await.unwrap();
    settings.email[3] = true;
    preferences::save(db, actor(2100), &settings).await.unwrap();
    reply(&writer, topic.tid, "Queued before the email change").await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    execute(
        db,
        "UPDATE gonof_common_member SET email='replacement@example.test' WHERE uid=2100",
    )
    .await;
    let sent = gono_forum_web::test_support::deliver_mail(
        db,
        &mail_directory,
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 0);
    assert_eq!(sent.skipped, 1);
    assert_eq!(
        skip_reason(db, 2100).await,
        "the recipient email address is unverified or changed"
    );
    reply(
        &writer,
        topic.tid,
        "Sender blocked after the email was queued",
    )
    .await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    social::set_block(db, actor(2100), 2000, true, &identity::random_token())
        .await
        .unwrap();
    let sent = gono_forum_web::test_support::deliver_mail(
        db,
        &mail_directory,
        gono_forum_web::test_support::TEST_SITE_URL,
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(sent.delivered, 0);
    assert_eq!(sent.skipped, 1);
    assert_eq!(
        skip_reason(db, 2100).await,
        "the recipient blocked the author"
    );
    social::set_block(db, actor(2100), 2000, false, &identity::random_token())
        .await
        .unwrap();
    // Forum subscriptions notify new topics only and do not auto-subscribe their author.
    social::set_follow_forum(db, actor(2100), fid, true, &identity::random_token())
        .await
        .unwrap();
    let new_topic = writer
        .publish(
            2000,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "New thread in the forum".into(),
                body: "Body of the new thread".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    let kind: String =
        db::query_scalar("SELECT type FROM gonof_home_notification WHERE uid=2100 AND pid=?")
            .bind(new_topic.pid)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(kind, "follow_forum");
    assert!(
        !social::following(db, 2000, new_topic.tid, Relation::Thread)
            .await
            .unwrap()
    );
    let child = reply(
        &writer,
        new_topic.tid,
        "Forum follows do not get ordinary replies",
    )
    .await;
    notifications::deliver_pending(db, identity::now())
        .await
        .unwrap();
    assert_eq!(notices(db, child, 2100).await, 0);
    // Per-user list boundaries and direct permission checks.
    assert_eq!(
        social::list(db, actor(2100), Relation::Thread, 1, &Default::default())
            .await
            .unwrap()
            .total,
        1
    );
    assert!(
        social::list(db, actor(2100), Relation::Thread, 2, &Default::default())
            .await
            .is_err()
    );
    let blocked = social::blocked_uids(db, 2102, &[2000, 2001]).await.unwrap();
    assert_eq!(blocked.into_iter().collect::<Vec<_>>(), vec![2000]);
    assert!(
        !social::following(db, 2000, 2102, Relation::Block)
            .await
            .unwrap()
    );
    // Read-only mode only blocks public content: follows, favorites, blocks and preferences only affect the member,
    // so they work as usual; like counts are visible to others, so likes are refused.
    runtime::update(
        db,
        &fixture.global,
        actor(2001),
        "site.settings.save",
        |s| {
            s.site.read_only = true;
        },
    )
    .await
    .unwrap();
    social::set_follow_forum(db, actor(2100), fid, false, &identity::random_token())
        .await
        .unwrap();
    let interactions = interaction::Interaction::new(db.clone());
    interactions
        .set_favorite(actor(2100), topic.tid, true, &identity::random_token())
        .await
        .unwrap();
    assert!(matches!(
        interactions
            .set_reaction(actor(2100), topic.pid, true, &identity::random_token())
            .await,
        Err(ContentWriteError::Forbidden(_))
    ));
    social::set_block(db, actor(2102), 2000, false, &identity::random_token())
        .await
        .unwrap();
    let mut settings = preferences::get(db, actor(2100)).await.unwrap();
    settings.site[0] = false;
    preferences::save(db, actor(2100), &settings).await.unwrap();
    // Preference switches have no value check in the database: switches only come from `bool`, and preference rows
    // are written only through `preferences::save`.
    // Blocking oneself and changing follow rows are not refused by the database: the entry point refuses blocking
    // oneself, rows written by bypassing it are found by the messaging check script, and follow and block rows have
    // no UPDATE at all, pinned by `db::source_guards`.
    assert!(matches!(
        social::set_block(db, actor(2100), 2100, true, &identity::random_token()).await,
        Err(gono_forum_web::content::writes::ContentWriteError::Validation(_))
    ));
    gono_forum_web::test_support::assert_messaging_rejected_or_flagged(
        db,
        "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES(2100,2100,0)",
        "blocks of oneself or with invalid values",
    )
    .await;
    let mismatch=scalar(db,"SELECT COUNT(*) FROM gonof_common_member m WHERE m.unread_notifications<>(SELECT COUNT(*) FROM gonof_home_notification n WHERE n.uid=m.uid AND n.read_at=0)").await;
    assert_eq!(mismatch, 0);
    social::set_block(db, actor(2102), 2162, true, &identity::random_token())
        .await
        .unwrap();
    execute(db,"UPDATE gonof_common_member SET account_state='deleted',username='deleted-2162-synthetic',email=NULL,password=NULL,emailstatus=0 WHERE uid=2162").await;
    let blocked = social::list(db, actor(2102), Relation::Block, 1, &Default::default())
        .await
        .unwrap();
    let deleted = blocked.rows.iter().find(|row| row.target == 2162).unwrap();
    assert_eq!(deleted.title, "Deleted user");
    assert!(deleted.href.is_empty());
    social::set_block(db, actor(2102), 2162, false, &identity::random_token())
        .await
        .unwrap();
    execute(db,"UPDATE gonof_common_member SET account_state='deleting',credentials_version=credentials_version+1 WHERE uid=2100").await;
    assert!(preferences::get(db, actor(2100)).await.is_err());
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn fanout_checkpoint_survives_a_later_batch_failure_without_duplicate_delivery() {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    member(db, 2000, true).await;
    member(db, 2001, true).await;
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    let writer = ContentWriter::new(db.clone(), WriteConfig::default());
    let topic = writer
        .publish(
            2001,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Batch recovery".into(),
                body: "Checks batch commits after a failure".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    for uid in 2200..2270 {
        member(db, uid, false).await;
        db::query("INSERT INTO gonof_home_thread_follow(uid,tid,dateline) VALUES(?,?,?)")
            .bind(uid)
            .bind(topic.tid)
            .bind(identity::now())
            .execute(db.executor())
            .await
            .unwrap();
    }
    gono_forum_web::test_support::fail_when(
        db,
        "social_delivery_failure",
        "BEFORE INSERT",
        "gonof_home_notification",
        "NEW.uid=2255",
        "synthetic fanout failure",
    )
    .await
    .unwrap();
    let published = reply(&writer, topic.tid, "Follow notification failure recovery").await;
    let now = identity::now();
    let first = notifications::deliver_pending(db, now).await.unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.delivered, 51);
    let first_count: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE pid=?")
            .bind(published)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(
        first_count, 51,
        "every notification of the failed batch rolls back"
    );
    let payload:String=db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE event_type='notification.content' AND aggregate_id=?").bind(published).fetch_one(db.executor()).await.unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["subscription"]["cursor"], 50);
    assert_eq!(payload["direct_done"], true);
    gono_forum_web::test_support::drop_trigger(db, "social_delivery_failure")
        .await
        .unwrap();
    follow(db, 2268, topic.tid, false).await;
    follow(db, 2268, topic.tid, true).await;
    let retried = notifications::deliver_pending(db, now + 4000)
        .await
        .unwrap();
    assert_eq!(retried.failed, 0);
    assert_eq!(retried.delivered, 19);
    let final_count: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE pid=?")
            .bind(published)
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(final_count, 70);
    assert_eq!(
        notices(db, published, 2268).await,
        0,
        "a retry must not add a new subscription below the original high-water mark"
    );
    assert_eq!(
        notifications::deliver_pending(db, now + 5000)
            .await
            .unwrap()
            .delivered,
        0
    );
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn repeating_a_relation_without_change_writes_no_log() {
    let fixture = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    member(db, 2300, false).await;
    member(db, 2301, false).await;
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
    // Of two consecutive operations in the same direction only the first changes state, so the log should have only
    // that one entry.
    for desired in [true, true, false, false] {
        social::set_block(db, actor(2300), 2301, desired, &identity::random_token())
            .await
            .unwrap();
        social::set_follow_forum(db, actor(2300), fid, desired, &identity::random_token())
            .await
            .unwrap();
    }
    for kind in ["interaction.block", "interaction.forum"] {
        let logs: i64 =
            db::query_scalar("SELECT COUNT(*) FROM gonof_common_log WHERE uid=2300 AND type=?")
                .bind(kind)
                .fetch_one(db.executor())
                .await
                .unwrap();
        assert_eq!(logs, 2, "{kind}");
    }
    assert!(
        !social::following(db, 2300, 2301, Relation::Block)
            .await
            .unwrap()
    );
    fixture.cleanup().await.unwrap();
}
