//! Mail taken all the way through the mail worker to the local mailbox: which addresses may receive the links frozen
//! members use and the sanction mail, and what the delivered message says. Tests that only read the queued event
//! cannot see a mail the worker then refuses to send.
use std::path::PathBuf;

use gono_forum_web::{
    db,
    identity::{self, Actor},
    mail::DeliveryReport,
    moderation::{self as gov, SanctionRequest},
    privacy,
    test_support::{self, TEST_SITE_URL, TestFixture},
};

const ADMIN: i64 = 8801;

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
            Actor {
                uid: ADMIN,
                credentials_version: 1,
                api_key: None,
            },
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
