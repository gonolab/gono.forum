//! Mail taken all the way through the mail worker to the local mailbox: which addresses may receive the links frozen
//! members use and the sanction mail, and what the delivered message says. Tests that only read the queued event
//! cannot see a mail the worker then refuses to send.
use std::path::PathBuf;

use gono_forum_web::{
    administration,
    content::{
        self,
        writes::{ContentWriter, NewThread, WriteConfig},
    },
    db::{self, Backend},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
    interaction::social,
    mail::DeliveryReport,
    moderation::{self as gov, SanctionRequest},
    notifications::{self, preferences},
    privacy,
    test_support::{self, TEST_SITE_URL, TestFixture},
};

const ADMIN: i64 = 8801;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

struct Fixture {
    f: TestFixture,
    mailbox: PathBuf,
}

/// One outbox event as the admin task list sees it.
#[derive(Debug)]
struct Event {
    eventid: i64,
    status: String,
    last_error: Option<String>,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,'mail-admin','admin-hash','mail-admin@example.test',1,1,1)")
            .bind(ADMIN)
            .execute(f.db.executor())
            .await
            .unwrap();
        let mailbox = f.root.join(".local/mail");
        Self { f, mailbox }
    }

    /// A member at `mail-{uid}@example.test`, with or without a password, and with the address verified or not.
    async fn member(&self, uid: i64, password: bool, verified: bool) -> String {
        let email = format!("mail-{uid}@example.test");
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,?,?,?,1)")
            .bind(uid)
            .bind(format!("mail-{uid}"))
            .bind(password.then_some("member-hash"))
            .bind(&email)
            .bind(i64::from(verified))
            .execute(self.f.db.executor())
            .await
            .unwrap();
        email
    }

    async fn sanction(&self, uid: i64, kind: &str, now: i64) -> i64 {
        let version: i64 =
            db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_one(self.f.db.executor())
                .await
                .unwrap();
        gov::issue_sanction(
            &self.f.db,
            actor(ADMIN),
            &SanctionRequest {
                uid,
                kind: kind.into(),
                hours: if kind == "warning" { 0 } else { 1 },
                reason: "Broke the test community rules".into(),
                expected_version: version,
                request_key: identity::random_token(),
            },
            now,
        )
        .await
        .unwrap()
    }

    async fn deliver(&self, now: i64) -> DeliveryReport {
        test_support::deliver_mail(&self.f.db, &self.mailbox, TEST_SITE_URL, now)
            .await
            .unwrap()
    }

    async fn events(&self, uid: i64, kind: &str) -> Vec<Event> {
        db::query("SELECT eventid,status,last_error FROM gonof_common_outbox WHERE aggregate_type='member' AND aggregate_id=? AND event_type=? ORDER BY eventid")
            .bind(uid)
            .bind(kind)
            .fetch_all(self.f.db.executor())
            .await
            .unwrap()
            .into_iter()
            .map(|row| Event {
                eventid: row.try_get("eventid").unwrap(),
                status: db::text(&row, "status").unwrap(),
                last_error: row.try_get("last_error").unwrap(),
            })
            .collect()
    }

    /// The only event of this kind for the member.
    async fn event(&self, uid: i64, kind: &str) -> Event {
        let mut events = self.events(uid, kind).await;
        assert_eq!(events.len(), 1, "{kind} for {uid}: {events:?}");
        events.remove(0)
    }

    /// The message the local transport wrote for the event, if any.
    fn delivered(&self, eventid: i64) -> Option<String> {
        std::fs::read_to_string(self.mailbox.join(format!("{eventid}.eml"))).ok()
    }

    fn draft(&self, eventid: i64) -> PathBuf {
        self.mailbox.join(format!("prepared/{eventid}.eml"))
    }

    /// Runs the content notification worker until the queue is empty.
    async fn drain_notifications(&self, now: i64) {
        for _ in 0..10 {
            if notifications::deliver_pending(&self.f.db, now)
                .await
                .unwrap()
                .claimed
                == 0
            {
                return;
            }
        }
        panic!("the notification queue did not drain");
    }

    /// Turns on mail for new threads in followed forums.
    async fn mail_new_threads(&self, uid: i64) {
        let mut prefs = preferences::get(&self.f.db, actor(uid)).await.unwrap();
        prefs.email[4] = true;
        preferences::save(&self.f.db, actor(uid), &prefs)
            .await
            .unwrap();
    }

    async fn follow_forum(&self, uid: i64, fid: i64, desired: bool) {
        social::set_follow_forum(
            &self.f.db,
            actor(uid),
            fid,
            desired,
            &identity::random_token(),
        )
        .await
        .unwrap();
    }

    async fn forum(&self, slug: &str) -> i64 {
        db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug=?")
            .bind(slug)
            .fetch_one(self.f.db.executor())
            .await
            .unwrap()
    }

    /// A new thread by the administrator; content mail only goes out about other members' posts.
    async fn publish(&self, fid: i64, now: i64) {
        ContentWriter::new(self.f.db.clone(), WriteConfig::default())
            .publish_at(
                ADMIN,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: "A thread for followers".into(),
                    body: "The body of a thread followers hear about".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                now,
            )
            .await
            .unwrap();
    }

    /// After every update that marks an outbox event sent, turns mail sending off, as an administrator would while
    /// a slow SMTP server is still taking the first mail of a batch.
    async fn switch_off_after_each_sent_mail(&self) {
        let sql = match self.f.db.backend() {
            Backend::MySql => {
                "CREATE TRIGGER gonof_test_mail_switch AFTER UPDATE ON gonof_common_outbox FOR EACH ROW BEGIN IF NEW.status='sent' THEN UPDATE gonof_common_settings SET svalue='false' WHERE skey='settings.email.enabled'; END IF; END"
            }
            Backend::Sqlite => {
                "CREATE TRIGGER gonof_test_mail_switch AFTER UPDATE ON gonof_common_outbox FOR EACH ROW WHEN NEW.status='sent' BEGIN UPDATE gonof_common_settings SET svalue='false' WHERE skey='settings.email.enabled'; END"
            }
        };
        db::raw_sql(sql).execute(&self.f.db.pool).await.unwrap();
    }
}

/// A frozen member without a password (signed up with a text message code, Google or GitHub) handles data requests
/// through the link sent to a verified email. The worker used to drop that mail for want of a password while the
/// page said it was sent, and the member, unable to sign in, had no other way in.
#[tokio::test]
async fn frozen_members_without_a_password_receive_the_data_request_link() {
    let f = Fixture::new().await;
    let now = identity::now();
    let email = f.member(8811, false, true).await;
    f.sanction(8811, "freeze", now).await;
    privacy::request_access(&f.f.db, &email, now + 1)
        .await
        .unwrap();
    let report = f.deliver(now + 2).await;
    let event = f.event(8811, "privacy_access").await;
    assert_eq!(
        (event.status.as_str(), event.last_error.as_deref()),
        ("sent", None),
        "{report:?}"
    );
    let message = f.delivered(event.eventid).expect("the link was delivered");
    assert!(
        message.contains(&format!("To: {email}"))
            && message.contains(&format!("{TEST_SITE_URL}/data/access?token=")),
        "{message}"
    );
    f.f.cleanup().await.unwrap();
}

/// The appeal link opens the member's sanction records and their one appeal, so it follows the rule of the data
/// request link: a password, or a verified email. Checked when the link is issued and again by the worker.
#[tokio::test]
async fn appeal_links_need_a_password_or_a_verified_email() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    // Signed up through a provider with an address typed in and never confirmed: no link.
    let unconfirmed = f.member(8821, false, false).await;
    // The same with a confirmed address, and a member with a password whose address was never confirmed.
    let confirmed = f.member(8822, false, true).await;
    let with_password = f.member(8823, true, false).await;
    for uid in [8821, 8822, 8823] {
        f.sanction(uid, "freeze", now).await;
    }
    for email in [&unconfirmed, &confirmed, &with_password] {
        gov::request_appeal_access(db, email, now + 1)
            .await
            .unwrap();
    }
    assert!(
        f.events(8821, "appeal_access").await.is_empty(),
        "no link for an account without a password at an unverified email"
    );
    let tokens: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid=8821 AND purpose='appeal_access'",
    )
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(tokens, 0);
    // The address loses its verification after the link was queued: the worker drops the mail.
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=8822")
        .execute(db.executor())
        .await
        .unwrap();
    f.deliver(now + 2).await;
    let dropped = f.event(8822, "appeal_access").await;
    assert_eq!(dropped.status, "failed");
    assert!(
        dropped
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("unverified")),
        "{dropped:?}"
    );
    assert!(f.delivered(dropped.eventid).is_none());
    let sent = f.event(8823, "appeal_access").await;
    assert_eq!(sent.status, "sent", "{sent:?}");
    assert!(
        f.delivered(sent.eventid)
            .is_some_and(|message| message.contains("/appeals/access?token=")),
    );
    f.f.cleanup().await.unwrap();
}

/// Sanction mail carries the operator's reason, so it goes only to a verified address; the notification on the site
/// is written either way. Mail queued while the address was verified is dropped if it no longer is.
#[tokio::test]
async fn sanction_mail_goes_only_to_verified_addresses() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    f.member(8831, true, false).await;
    f.member(8832, true, true).await;
    f.sanction(8831, "warning", now).await;
    f.sanction(8832, "warning", now).await;
    assert!(
        f.events(8831, "sanction_issued").await.is_empty(),
        "no sanction mail to an unverified address"
    );
    let notices: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_home_notification WHERE uid=8831")
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(notices, 1, "the member is still notified on the site");
    f.events(8832, "sanction_issued").await;
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=8832")
        .execute(db.executor())
        .await
        .unwrap();
    f.deliver(now + 1).await;
    let dropped = f.event(8832, "sanction_issued").await;
    assert_eq!(dropped.status, "failed");
    assert!(
        dropped
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("unverified")),
        "{dropped:?}"
    );
    assert!(f.delivered(dropped.eventid).is_none());
    f.f.cleanup().await.unwrap();
}

/// The worker has no request to take the site's time style from, so sanction mail is written with the site settings
/// (here UTC) and names the zone, like the sign-in alert; it used to fall back to UTC+8 without saying so.
#[tokio::test]
async fn sanction_mail_times_follow_the_site_time_zone() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    for (key, value) in [
        ("settings.time.offset_minutes", "0"),
        ("settings.time.clock24", "true"),
        ("settings.time.date_format", "ymd-dash"),
    ] {
        test_support::set_setting(db, key, value).await.unwrap();
    }
    f.member(8841, true, true).await;
    let id = f.sanction(8841, "mute", now).await;
    let row = db::query("SELECT starts_at,expires_at FROM gonof_common_member_sanction WHERE id=?")
        .bind(id)
        .fetch_one(db.executor())
        .await
        .unwrap();
    let utc = |timestamp: i64| {
        chrono::DateTime::from_timestamp(timestamp, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M (UTC)")
            .to_string()
    };
    let (starts, ends) = (
        utc(row.try_get("starts_at").unwrap()),
        utc(row.try_get("expires_at").unwrap()),
    );
    f.deliver(now + 1).await;
    let event = f.event(8841, "sanction_issued").await;
    assert_eq!(event.status, "sent", "{event:?}");
    let message = f.delivered(event.eventid).unwrap();
    assert!(
        message.contains(&starts) && message.contains(&ends),
        "expected {starts} and {ends} in:\n{message}"
    );
    f.f.cleanup().await.unwrap();
}

/// Turning mail off stops the worker before its next claim, not after the batch: a slow SMTP server could otherwise
/// keep it sending, including mail queued after the switch was turned off.
#[tokio::test]
async fn turning_mail_off_stops_the_worker_before_its_next_mail() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    // No process cache, so the worker sees the switch as soon as it changes, as in the process that saved it.
    test_support::set_setting(db, "settings.cache.ttl_seconds", "0")
        .await
        .unwrap();
    for uid in [8871, 8872, 8873] {
        f.member(uid, true, true).await;
        f.sanction(uid, "warning", now).await;
    }
    f.switch_off_after_each_sent_mail().await;
    let report = f.deliver(now + 1).await;
    assert_eq!((report.claimed, report.delivered), (1, 1), "{report:?}");
    let mut statuses = Vec::new();
    for uid in [8871, 8872, 8873] {
        statuses.push(f.event(uid, "sanction_issued").await.status);
    }
    assert_eq!(statuses, ["sent", "pending", "pending"]);
    f.f.cleanup().await.unwrap();
}

/// A database restored from an older backup reuses event ids that a newer `mail/prepared/` still holds drafts for.
/// The draft names the event it was written for; one written for another event is discarded and the mail written
/// again, instead of sending the earlier recipient's mail to the new one.
#[tokio::test]
async fn a_draft_left_by_another_event_with_the_same_id_is_written_again() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    f.member(8851, true, true).await;
    f.member(8852, true, true).await;
    f.sanction(8851, "warning", now).await;
    let first = f.event(8851, "sanction_issued").await.eventid;
    // Sent, then the process stopped before recording it: the draft stays for the retry.
    test_support::fail_when(
        db,
        "gonof_test_crash_before_ack",
        "BEFORE UPDATE",
        "gonof_common_outbox",
        "NEW.status='sent'",
        "simulated crash before ack",
    )
    .await
    .unwrap();
    f.deliver(now + 1).await;
    test_support::drop_trigger(db, "gonof_test_crash_before_ack")
        .await
        .unwrap();
    assert!(f.draft(first).exists());
    // The restore: the event is gone, and the next one, for another member, gets its id. The delivered copy is
    // removed as SMTP would leave none.
    std::fs::remove_file(f.mailbox.join(format!("{first}.eml"))).unwrap();
    db::query("DELETE FROM gonof_common_outbox WHERE eventid=?")
        .bind(first)
        .execute(db.executor())
        .await
        .unwrap();
    f.sanction(8852, "warning", now + 2).await;
    db::query("UPDATE gonof_common_outbox SET eventid=? WHERE aggregate_id=8852 AND event_type='sanction_issued'")
        .bind(first)
        .execute(db.executor())
        .await
        .unwrap();
    f.deliver(now + 3).await;
    let event = f.event(8852, "sanction_issued").await;
    assert_eq!((event.eventid, event.status.as_str()), (first, "sent"));
    let message = f.delivered(first).unwrap();
    assert!(
        message.contains("To: mail-8852@example.test") && !message.contains("mail-8851"),
        "{message}"
    );
    f.f.cleanup().await.unwrap();
}

/// The draft of a content mail is read before the worker locks the member, forum and post rows: a slow mail folder
/// must not hold up posting in that forum, or every write on SQLite. A draft that cannot be read fails the attempt
/// before any of those rows is read.
#[tokio::test]
async fn content_mail_reads_its_draft_before_taking_locks() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    content::seed_forums(db).await.unwrap();
    let fid = f.forum("daily").await;
    f.member(8881, true, true).await;
    f.mail_new_threads(8881).await;
    f.follow_forum(8881, fid, true).await;
    f.publish(fid, now).await;
    f.drain_notifications(now + 1).await;
    let eventid = f.event(8881, "notification.email").await.eventid;
    std::fs::create_dir_all(f.draft(eventid)).unwrap();
    let profile = std::sync::Arc::new(RequestProfile::new());
    let report = trace::scope(profile.clone(), Box::pin(f.deliver(now + 2))).await;
    assert_eq!((report.claimed, report.failed), (1, 1), "{report:?}");
    // SQLite records the statements without `FOR UPDATE`, so look for any read of those rows: the write transaction
    // locks them as it reads them.
    let locks: Vec<String> = profile
        .snapshot()
        .into_iter()
        .filter(|record| {
            record.source == QuerySource::Application
                && ["gonof_common_member", "gonof_forum_"]
                    .iter()
                    .any(|table| record.sql.contains(table))
        })
        .map(|record| record.sql)
        .collect();
    assert!(
        locks.is_empty(),
        "locked before reading the draft: {locks:?}"
    );
    f.f.cleanup().await.unwrap();
}

/// A new thread in a child forum reaches a member who follows the child forum and its parent once. The mail records
/// both follows, so undoing one before it goes out does not drop the mail the other still asks for.
#[tokio::test]
async fn content_mail_survives_undoing_one_of_two_overlapping_follows() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    content::seed_forums(db).await.unwrap();
    let (parent, child) = (f.forum("daily").await, f.forum("meta").await);
    db::query("UPDATE gonof_forum_forum SET parent_fid=? WHERE fid=?")
        .bind(parent)
        .bind(child)
        .execute(db.executor())
        .await
        .unwrap();
    f.member(8891, true, true).await;
    f.mail_new_threads(8891).await;
    f.follow_forum(8891, child, true).await;
    f.follow_forum(8891, parent, true).await;
    f.publish(child, now).await;
    f.drain_notifications(now + 1).await;
    // The child forum's followers come first, then the parent's: one mail for the member.
    let event = f.event(8891, "notification.email").await;
    f.follow_forum(8891, child, false).await;
    f.deliver(now + 2).await;
    let event = f
        .events(8891, "notification.email")
        .await
        .into_iter()
        .find(|row| row.eventid == event.eventid)
        .unwrap();
    assert_eq!(
        (event.status.as_str(), event.last_error.as_deref()),
        ("sent", None)
    );
    assert!(f.delivered(event.eventid).is_some());
    f.f.cleanup().await.unwrap();
}

/// The admin task list labels every kind of mail as mail; it used to look for an `email.` prefix that no mail event
/// has, and showed them all as generic background tasks.
#[tokio::test]
async fn the_task_list_labels_mail_tasks_as_mail() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    for (n, kind) in [
        "password_reset",
        "sanction_issued",
        "login_alert",
        "notification.email",
        "message.email",
    ]
    .into_iter()
    .enumerate()
    {
        db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'member',?,'{}',?,?)")
            .bind(format!("label-test:{n}"))
            .bind(kind)
            .bind(ADMIN)
            .bind(now)
            .bind(now)
            .execute(db.executor())
            .await
            .unwrap();
    }
    let (tasks, total) = administration::tasks(db, actor(ADMIN), &Default::default())
        .await
        .unwrap();
    assert_eq!(total, 5);
    let mail = gono_forum_web::i18n::current().maintenance_tasks_kind_email();
    for task in &tasks {
        assert_eq!(task.kind_label(), mail, "{}", task.kind);
    }
    f.f.cleanup().await.unwrap();
}

/// A private message mail alert waits ten minutes so a message read in that time needs no mail. Retrying it from the
/// task list keeps that wait instead of making it due at once.
#[tokio::test]
async fn retrying_a_message_mail_alert_keeps_its_reading_window() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let now = identity::now();
    let window = 600;
    let eventid = db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES('message.email:8801:1','message.email','member',?,'{}',?,?)")
        .bind(ADMIN)
        .bind(now)
        .bind(now + window)
        .execute(db.executor())
        .await
        .unwrap()
        .last_insert_id() as i64;
    administration::retry_task(
        db,
        actor(ADMIN),
        eventid,
        0,
        now + window,
        0,
        "pending",
        "Checking the queue",
    )
    .await
    .unwrap();
    let due: i64 = db::query_scalar("SELECT available_at FROM gonof_common_outbox WHERE eventid=?")
        .bind(eventid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert_eq!(due, now + window);
    f.f.cleanup().await.unwrap();
}
