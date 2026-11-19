//! Financial and participation contracts against an isolated MySQL database.
use gono_forum_web::{
    activities::{self, ActivitySpec},
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor},
    test_support::TestFixture,
};

const NOW: i64 = 1_800_000_000;
fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default())
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}
async fn topic(db: &Database, fid: i64, spec: ActivitySpec) -> Published {
    writer(db)
        .publish_at(
            2,
            NewThread {
                visibility: Default::default(),
                activity: spec,
                fid,
                title: "Activity test thread".into(),
                body: "Activity test body".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}
async fn reply(db: &Database, uid: i64, tid: i64, root: Option<i64>) -> Published {
    writer(db)
        .reply_at(
            uid,
            NewReply {
                tid,
                root_pid: root,
                reply_to_pid: root,
                body: "A valid activity reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 1,
        )
        .await
        .unwrap()
}
async fn withdraw(db: &Database, uid: i64, pid: i64) {
    let editor = ContentEditor::new(db.clone(), Default::default(), 86400);
    let post = editor.editable_at(actor(uid), pid, NOW + 2).await.unwrap();
    editor
        .change_at(
            actor(uid),
            ChangeRequest {
                pid,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                request_key: identity::random_token(),
                reason: String::new(),
                action: ContentAction::Withdraw,
            },
            NOW + 2,
        )
        .await
        .unwrap();
}
fn paid(kind: i64, budget: i64, shares: i64) -> ActivitySpec {
    ActivitySpec {
        kind,
        budget,
        shares,
        deadline: NOW + 3600,
        ..Default::default()
    }
}

#[tokio::test]
async fn mysql_activity_ledger_and_participation_contract() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=8 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("activity-{uid}")).bind(format!("activity-{uid}@example.test")).bind(NOW-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    assert!(
        activities::rules(db)
            .await
            .unwrap()
            .iter()
            .all(|r| !r.enabled)
    );
    activities::adjust(
        db,
        actor(1),
        2,
        1000,
        &identity::random_token(),
        "test activity budget",
        NOW,
    )
    .await
    .unwrap();

    let packet = topic(db, fid, paid(2, 12, 3)).await;
    let packet_card = activities::card(db, packet.tid, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(packet_card.shares, 3);
    assert_eq!(packet_card.remaining_shares, 3);
    assert_eq!(packet_card.distribution, "equal");
    assert_eq!(packet_card.max_choices, 1);
    assert_eq!(packet_card.voters, 0);
    assert!(
        packet_card.source_url.is_empty()
            && packet_card.source_name.is_empty()
            && packet_card.source_author.is_empty()
    );
    let first = reply(db, 3, packet.tid, None).await;
    reply(db, 3, packet.tid, None).await;
    reply(db, 4, packet.tid, Some(first.pid)).await;
    reply(db, 2, packet.tid, None).await;
    let (a, b) = tokio::join!(
        reply(db, 5, packet.tid, None),
        reply(db, 6, packet.tid, None)
    );
    assert_ne!(a.pid, b.pid);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_redpacket_claim WHERE tid={}",
                packet.tid
            )
        )
        .await,
        3
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT CAST(SUM(amount) AS SIGNED) FROM gonof_forum_redpacket_claim WHERE tid={}",
                packet.tid
            )
        )
        .await,
        12
    );
    assert_eq!(
        scalar(
            db,
            "SELECT frozen FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_redpacket_claim WHERE tid={} AND uid=2",
                packet.tid
            )
        )
        .await,
        0
    );

    let bounty = topic(db, fid, paid(1, 21, 1)).await;
    let bounty_card = activities::card(db, bounty.tid, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bounty_card.shares, 0);
    assert_eq!(bounty_card.remaining_shares, 0);
    assert_eq!(bounty_card.accepted_pid, 0);
    let target = reply(db, 3, bounty.tid, None).await;
    let before = scalar(
        db,
        "SELECT available FROM gonof_common_credit_account WHERE uid=3",
    )
    .await;
    let key = identity::random_token();
    assert!(
        activities::accept(
            db,
            actor(2),
            bounty.tid,
            target.pid,
            &key.to_uppercase(),
            NOW + 2
        )
        .await
        .is_err(),
        "an uppercase request key is invalid"
    );
    activities::accept(db, actor(2), bounty.tid, target.pid, &key, NOW + 2)
        .await
        .unwrap();
    activities::accept(db, actor(2), bounty.tid, target.pid, &key, NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT available FROM gonof_common_credit_account WHERE uid=3"
        )
        .await,
        before + 21
    );
    assert!(
        activities::accept(
            db,
            actor(3),
            bounty.tid,
            target.pid,
            &identity::random_token(),
            NOW + 3
        )
        .await
        .is_err()
    );
    // No trigger makes the ledger "unchangeable and undeletable": the source code only INSERTs into the ledger
    // (db::source_guards), and accounts and ledger corroborate each other through the
    // credit reconciliation; that tampering can be detected is shown by
    // `credit_rules_hold_in_code_after_the_triggers_are_gone`.
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert!(reconciliation.is_consistent(), "{reconciliation:?}");
    assert!(
        activities::adjust(
            db,
            actor(1),
            8,
            -1,
            &identity::random_token(),
            "no overdraft",
            NOW
        )
        .await
        .is_err()
    );

    let cancel = topic(db, fid, paid(2, 30, 3)).await;
    reply(db, 3, cancel.tid, None).await;
    let owner_before = scalar(
        db,
        "SELECT available FROM gonof_common_credit_account WHERE uid=2",
    )
    .await;
    withdraw(db, 2, cancel.pid).await;
    assert_eq!(
        scalar(
            db,
            "SELECT available FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        owner_before + 20
    );
    assert_eq!(scalar(db,&format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={} AND state='cancelled' AND remaining=0",cancel.tid)).await,1);
    reply(db, 4, cancel.tid, None).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_redpacket_claim WHERE tid={}",
                cancel.tid
            )
        )
        .await,
        1
    );
    let editor = ContentEditor::new(db.clone(), Default::default(), 86400);
    let snapshot = editor
        .editable_at(actor(2), cancel.pid, NOW + 3)
        .await
        .unwrap();
    editor
        .change_at(
            actor(2),
            ChangeRequest {
                pid: cancel.pid,
                expected_post_version: snapshot.version,
                expected_thread_version: snapshot.thread_version,
                request_key: identity::random_token(),
                reason: String::new(),
                action: ContentAction::Restore,
            },
            NOW + 3,
        )
        .await
        .unwrap();
    reply(db, 6, cancel.tid, None).await;
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_redpacket_claim WHERE tid={}",
                cancel.tid
            )
        )
        .await,
        1,
        "restoring the thread does not reopen an ended red packet"
    );

    let lottery = topic(db, fid, paid(3, 20, 2)).await;
    let lottery_card = activities::card(db, lottery.tid, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lottery_card.shares, 2);
    assert_eq!(lottery_card.remaining_shares, 0);
    assert_eq!(lottery_card.distribution, "equal");
    let r3 = reply(db, 3, lottery.tid, None).await;
    reply(db, 3, lottery.tid, None).await;
    let r4 = reply(db, 4, lottery.tid, None).await;
    reply(db, 5, lottery.tid, None).await;
    withdraw(db, 3, r3.pid).await; // The other valid reply keeps UID 3 eligible.
    withdraw(db, 4, r4.pid).await;
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=5").await;
    let owner_before = scalar(
        db,
        "SELECT available FROM gonof_common_credit_account WHERE uid=2",
    )
    .await;
    let (first_worker, second_worker) = tokio::join!(
        activities::process_due(db, NOW + 3601, 100),
        activities::process_due(db, NOW + 3601, 100)
    );
    first_worker.unwrap();
    second_worker.unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT amount FROM gonof_forum_lottery_entry WHERE tid={} AND uid=3",
                lottery.tid
            )
        )
        .await,
        10
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT CAST(SUM(amount) AS SIGNED) FROM gonof_forum_lottery_entry WHERE tid={}",
                lottery.tid
            )
        )
        .await,
        10
    );
    assert_eq!(
        scalar(
            db,
            "SELECT available FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        owner_before + 10
    );
    let events = scalar(db, "SELECT COUNT(*) FROM gonof_common_credit_ledger").await;
    activities::process_due(db, NOW + 3602, 100).await.unwrap();
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM gonof_common_credit_ledger").await,
        events
    );
    execute(db, "UPDATE gonof_common_member SET `freeze`=0 WHERE uid=5").await;

    let expired = topic(db, fid, paid(1, 99, 1)).await;
    activities::process_due(db, NOW + 3601, 100).await.unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={} AND state='expired'",
                expired.tid
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT frozen FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        0
    );

    let poll = topic(
        db,
        fid,
        ActivitySpec {
            kind: 4,
            deadline: NOW + 3600,
            max_choices: 2,
            options: vec![
                "Option one".into(),
                "Option two".into(),
                "Option three".into(),
            ],
            ..Default::default()
        },
    )
    .await;
    let key = identity::random_token();
    activities::vote(db, actor(3), poll.tid, vec![1, 3], &key, NOW + 1)
        .await
        .unwrap();
    activities::vote(db, actor(3), poll.tid, vec![3, 1], &key, NOW + 2)
        .await
        .unwrap();
    assert!(
        activities::vote(
            db,
            actor(3),
            poll.tid,
            vec![2],
            &identity::random_token(),
            NOW + 2
        )
        .await
        .is_err()
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT voters FROM gonof_forum_poll WHERE tid={}", poll.tid)
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT CAST(SUM(votes) AS SIGNED) FROM gonof_forum_poll_option WHERE tid={}",
                poll.tid
            )
        )
        .await,
        2
    );

    let news = topic(
        db,
        fid,
        ActivitySpec {
            kind: 5,
            source_url: "https://example.test/source".into(),
            source_name: "Test source".into(),
            source_author: "Original author".into(),
            ..Default::default()
        },
    )
    .await;
    let poll_card = activities::card(db, poll.tid, Some(activities::CardViewer::participant(3)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(poll_card.max_choices, 2);
    assert_eq!(poll_card.voters, 1);
    assert!(poll_card.has_voted);
    let news_card = activities::card(db, news.tid, None).await.unwrap().unwrap();
    assert_eq!(news_card.source_name, "Test source");
    assert_eq!(news_card.source_author, "Original author");
    assert_eq!(news_card.source_url, "https://example.test/source");
    assert_eq!(news_card.shares, 0);
    withdraw(db, 2, news.pid).await;
    assert!(
        activities::card(db, news.tid, None)
            .await
            .unwrap()
            .is_none()
    );
    let normal = topic(db, fid, ActivitySpec::default()).await;
    assert!(
        activities::card(db, normal.tid, None)
            .await
            .unwrap()
            .is_none()
    );

    // A rule whose daily cap is smaller than the per-award amount could never award anything, so it is rejected
    // when saved and the version stays the same.
    let rejected =
        activities::update_rule(db, &fixture.global, actor(1), "signin", true, 3, 2, 1, NOW)
            .await
            .unwrap_err();
    assert!(
        rejected
            .downcast_ref::<activities::ActivityFailure>()
            .is_some_and(|failure| failure
                .0
                .contains("cannot be smaller than the amount per award"))
    );
    assert_eq!(
        scalar(
            db,
            "SELECT version FROM gonof_common_credit_rule WHERE rule_key='signin'"
        )
        .await,
        1
    );
    activities::update_rule(db, &fixture.global, actor(1), "signin", true, 2, 2, 1, NOW)
        .await
        .unwrap();
    assert!(activities::check_in(db, actor(8), NOW).await.unwrap());
    assert!(!activities::check_in(db, actor(8), NOW + 1).await.unwrap());
    assert!(
        activities::check_in(db, actor(8), NOW + 86400)
            .await
            .unwrap()
    );
    activities::update_rule(db, &fixture.global, actor(1), "reply", true, 3, 5, 1, NOW)
        .await
        .unwrap();
    let before = scalar(
        db,
        "SELECT available FROM gonof_common_credit_account WHERE uid=8",
    )
    .await;
    reply(db, 8, normal.tid, None).await;
    reply(db, 8, normal.tid, None).await;
    assert_eq!(
        scalar(
            db,
            "SELECT available FROM gonof_common_credit_account WHERE uid=8"
        )
        .await,
        before + 3,
        "no award when the rest of the daily cap is less than one full award"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_common_credit_account WHERE available<0 OR frozen<0"
        )
        .await,
        0
    );
    // Every request record must store the request time; a row that misses it has dateline 0 and would be deleted by
    // the first cleanup.
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE dateline<=0"
        )
        .await,
        0,
        "the request record must store the request time"
    );
    assert!(scalar(db, "SELECT COUNT(*) FROM gonof_forum_post_request").await > 0);
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn mysql_live_actions_recheck_deadline_after_waiting_for_the_thread_lock() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    let now = gono_forum_web::identity::now();
    for uid in 1..=3 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)")
            .bind(uid).bind(format!("deadline-{uid}")).bind(format!("deadline-{uid}@example.test"))
            .bind(now-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    activities::adjust(
        db,
        actor(1),
        2,
        100,
        &identity::random_token(),
        "deadline boundary budget",
        now,
    )
    .await
    .unwrap();
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    for kind in [4, 2] {
        let now = gono_forum_web::identity::now();
        let spec = ActivitySpec {
            kind,
            budget: 10,
            shares: 1,
            deadline: now + 2,
            options: vec!["One".into(), "Two".into()],
            ..Default::default()
        };
        let thread = writer(db)
            .publish_at(
                2,
                NewThread {
                    visibility: Default::default(),
                    activity: spec,
                    fid,
                    title: "Lock wait across the deadline".into(),
                    body: "The deadline is judged by the database time after taking the lock"
                        .into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                now,
            )
            .await
            .unwrap();
        let mut blocker = db.begin_write().await.unwrap();
        db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
            .bind(thread.tid)
            .fetch_one(db.observe(&mut *blocker))
            .await
            .unwrap();
        let copy = db.clone();
        let pending = tokio::spawn(async move {
            if kind == 4 {
                activities::vote_live(
                    &copy,
                    actor(3),
                    thread.tid,
                    vec![1],
                    &identity::random_token(),
                )
                .await
                .map(|_| ())
            } else {
                writer(&copy)
                    .reply_actor(
                        actor(3),
                        NewReply {
                            tid: thread.tid,
                            root_pid: None,
                            reply_to_pid: None,
                            body: "A plain reply can be posted but cannot claim a red packet late"
                                .into(),
                            attachment_ids: vec![],
                            request_key: identity::random_token(),
                        },
                    )
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !pending.is_finished(),
            "the request must wait for the thread lock"
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        blocker.commit().await.unwrap();
        let result = pending.await.unwrap();
        if kind == 4 {
            assert!(result.is_err());
            assert_eq!(
                scalar(db, "SELECT COUNT(*) FROM gonof_forum_poll_vote").await,
                0
            )
        } else {
            result.unwrap();
            assert_eq!(
                scalar(db, "SELECT COUNT(*) FROM gonof_forum_redpacket_claim").await,
                0
            )
        }
    }
    // An already enrolled member must not replace a withdrawn qualifying reply
    // with a reply that waited across the deadline and retained its old HTTP time.
    let now = gono_forum_web::identity::now();
    let lottery = writer(db)
        .publish_at(
            2,
            NewThread {
                activity: ActivitySpec {
                    kind: 3,
                    budget: 10,
                    shares: 1,
                    deadline: now + 4,
                    ..Default::default()
                },
                fid,
                title: "Lottery replacement reply at the deadline".into(),
                body: "Only valid replies posted before the deadline qualify for the draw".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
                visibility: Default::default(),
            },
            now,
        )
        .await
        .unwrap();
    let initial = writer(db)
        .reply_at(
            3,
            NewReply {
                tid: lottery.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "First entry reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now,
        )
        .await
        .unwrap();
    let editor = ContentEditor::new(db.clone(), Default::default(), 86400);
    let editable = editor
        .editable_at(actor(3), initial.pid, now)
        .await
        .unwrap();
    editor
        .change_at(
            actor(3),
            ChangeRequest {
                pid: initial.pid,
                expected_post_version: editable.version,
                expected_thread_version: editable.thread_version,
                request_key: identity::random_token(),
                reason: String::new(),
                action: ContentAction::Withdraw,
            },
            now,
        )
        .await
        .unwrap();
    let mut blocker = db.begin_write().await.unwrap();
    db::query("SELECT tid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
        .bind(lottery.tid)
        .fetch_one(db.observe(&mut *blocker))
        .await
        .unwrap();
    assert!(gono_forum_web::identity::now() < now + 4);
    let copy = db.clone();
    let pending = tokio::spawn(async move {
        writer(&copy)
            .reply_actor(
                actor(3),
                NewReply {
                    tid: lottery.tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: "Replacement reply queued across the deadline".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!pending.is_finished());
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    blocker.commit().await.unwrap();
    pending.await.unwrap().unwrap();
    let now = gono_forum_web::identity::now();
    activities::process_due(db, now, 20).await.unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM gonof_forum_lottery_entry WHERE amount>0"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            db,
            "SELECT frozen FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        0
    );
    fixture.cleanup().await.unwrap();
}

// Activities judge "readable by members" by the inherited forum visibility: when the parent forum is disabled,
// a lottery in its child forum is refunded instead of drawn; in a members-only forum a bounty can be accepted
// and a poll voted on, and the card is shown only to signed-in members.
#[tokio::test]
async fn mysql_activities_follow_inherited_member_visibility() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=4 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("visible-{uid}")).bind(format!("visible-{uid}@example.test")).bind(NOW-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    activities::adjust(
        db,
        actor(1),
        2,
        1000,
        &identity::random_token(),
        "test activity budget",
        NOW,
    )
    .await
    .unwrap();
    let forums: Vec<i64> = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid")
        .fetch_all(db.executor())
        .await
        .unwrap();
    let (parent, child, members) = (forums[0], forums[1], forums[2]);
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={parent} WHERE fid={child}"),
    )
    .await;

    let lottery = topic(db, child, paid(3, 10, 1)).await;
    reply(db, 3, lottery.tid, None).await;
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET status=0 WHERE fid={parent}"),
    )
    .await;
    // The forum copies on a thread are maintained only by code; after changing the forum with raw SQL they are
    // synced through the same code.
    gono_forum_web::test_support::sync_forum_copies(db, parent)
        .await
        .unwrap();
    activities::process_due(db, NOW + 3601, 100).await.unwrap();
    activities::process_due(db, NOW + 3602, 100).await.unwrap();
    let tid = lottery.tid;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={tid} AND state='cancelled' AND remaining=0")
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE uid=3 AND event_key LIKE 'activity:{tid}:%'")
        )
        .await,
        0,
        "no awards to participants once the parent forum is disabled"
    );

    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='members' WHERE fid={members}"),
    )
    .await;
    let bounty = topic(db, members, paid(1, 20, 1)).await;
    let answer = reply(db, 3, bounty.tid, None).await;
    assert!(
        activities::card(db, bounty.tid, Some(activities::CardViewer::participant(4)))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        activities::card(db, bounty.tid, None)
            .await
            .unwrap()
            .is_none()
    );
    activities::accept(
        db,
        actor(2),
        bounty.tid,
        answer.pid,
        &identity::random_token(),
        NOW + 2,
    )
    .await
    .unwrap();
    let poll = topic(
        db,
        members,
        ActivitySpec {
            kind: 4,
            deadline: NOW + 3600,
            options: vec!["A".into(), "B".into()],
            ..Default::default()
        },
    )
    .await;
    activities::vote(
        db,
        actor(3),
        poll.tid,
        vec![1],
        &identity::random_token(),
        NOW + 2,
    )
    .await
    .unwrap();
    assert_eq!(
        activities::card(db, poll.tid, Some(activities::CardViewer::participant(3)))
            .await
            .unwrap()
            .unwrap()
            .voters,
        1
    );
    fixture.cleanup().await.unwrap();
}

/// A settlement that keeps failing must not block expired activities queued after it; the failed
/// item backs off and is retried when due.
#[tokio::test]
async fn mysql_a_failing_settlement_neither_blocks_the_batch_nor_retries_every_round() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=3 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("due-{uid}")).bind(format!("due-{uid}@example.test")).bind(NOW-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    activities::adjust(
        db,
        actor(1),
        2,
        1000,
        &identity::random_token(),
        "test activity budget",
        NOW,
    )
    .await
    .unwrap();
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1").await;
    let stuck = topic(db, fid, paid(1, 10, 1)).await;
    let later = topic(
        db,
        fid,
        ActivitySpec {
            deadline: NOW + 3700,
            ..paid(1, 10, 1)
        },
    )
    .await;
    // The item with the earlier deadline fails on every settlement, and not with a retryable lock error.
    gono_forum_web::test_support::fail_when(
        db,
        "fail_due_settlement",
        "BEFORE UPDATE",
        "gonof_forum_activity",
        &format!("NEW.tid={} AND NEW.state<>OLD.state", stuck.tid),
        "due settlement contract failure",
    )
    .await
    .unwrap();
    let state = |tid: i64| {
        format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={tid} AND state='open'")
    };
    let backoff = activities::DueBackoff::default();
    let report = activities::process_due_with(db, NOW + 3701, 100, &backoff)
        .await
        .expect("one failing item must not fail the whole batch");
    // The failure is reported, so a caller can tell it from an empty queue (#35).
    assert_eq!(
        report,
        activities::DueReport {
            settled: 1,
            paying: 0,
            failed: 1
        }
    );
    assert_eq!(scalar(db, &state(stuck.tid)).await, 1);
    assert_eq!(
        scalar(db, &state(later.tid)).await,
        0,
        "later due activities settle as usual"
    );
    gono_forum_web::test_support::drop_trigger(db, "fail_due_settlement")
        .await
        .unwrap();
    activities::process_due_with(db, NOW + 3702, 100, &backoff)
        .await
        .unwrap();
    assert_eq!(
        scalar(db, &state(stuck.tid)).await,
        1,
        "no retry during the backoff"
    );
    activities::process_due_with(db, NOW + 3701 + 600, 100, &backoff)
        .await
        .unwrap();
    assert_eq!(
        scalar(db, &state(stuck.tid)).await,
        0,
        "retried once the backoff is over"
    );
    fixture.cleanup().await.unwrap();
}

/// Drawing a lottery does not lock the entrants who did not win, and prizes are awarded in batches
/// of 100; a failure midway rolls back only the current batch, the ledger and budget are consistent after each
/// batch commits, and the next round continues from the breakpoint without awarding anything twice.
#[tokio::test]
async fn mysql_large_lottery_pays_in_batches_and_resumes_after_a_failure() {
    use std::time::Duration;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    const ENTRANTS: i64 = 210;
    for uid in (1..=3).chain(9..10 + ENTRANTS) {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("draw-{uid}")).bind(format!("draw-{uid}@example.test")).bind(NOW-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    activities::adjust(
        db,
        actor(1),
        2,
        1000,
        &identity::random_token(),
        "test activity budget",
        NOW,
    )
    .await
    .unwrap();
    let fid = scalar(db, "SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1").await;
    let lottery = topic(db, fid, paid(3, ENTRANTS * 2, ENTRANTS)).await;
    let tid = lottery.tid;
    for uid in 9..10 + ENTRANTS {
        reply(db, uid, tid, None).await;
    }
    // Member 9 entered but is frozen, so is not eligible to win.
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=9").await;
    // Fails when deducting remaining after the second batch is awarded: the first batch is already committed,
    // and the second batch is rolled back as a whole.
    gono_forum_web::test_support::fail_when(
        db,
        "fail_second_draw_batch",
        "BEFORE UPDATE",
        "gonof_forum_activity",
        &format!(
            "NEW.tid={tid} AND NEW.state='drawing' AND NEW.remaining<{}",
            ENTRANTS * 2 - 200
        ),
        "draw batch contract failure",
    )
    .await
    .unwrap();
    // Another transaction holds member 9's member row: neither the draw nor the awarding should wait for it.
    // This is a guarantee only MySQL row locks give; SQLite write transactions queue as a whole and do not take
    // this lock.
    let other = if db.backend() == gono_forum_web::db::Backend::MySql {
        let mut other = db.begin_write().await.unwrap();
        db::query("SELECT uid FROM gonof_common_member WHERE uid=9 FOR UPDATE")
            .fetch_one(&mut *other)
            .await
            .unwrap();
        Some(other)
    } else {
        None
    };
    let backoff = activities::DueBackoff::default();
    let settled = tokio::time::timeout(
        Duration::from_secs(20),
        activities::process_due_with(db, NOW + 3601, 100, &backoff),
    )
    .await
    .expect("the draw must not lock entrants who did not win")
    .unwrap();
    if let Some(other) = other {
        other.commit().await.unwrap();
    }
    assert_eq!(
        (settled.settled, settled.failed),
        (0, 1),
        "the second batch failed, so this round is not fully settled"
    );
    let state =
        format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={tid} AND state='drawing'");
    assert_eq!(scalar(db, &state).await, 1);
    let remaining = format!("SELECT remaining FROM gonof_forum_activity WHERE tid={tid}");
    assert_eq!(scalar(db, &remaining).await, ENTRANTS * 2 - 200);
    let frozen = "SELECT frozen FROM gonof_common_credit_account WHERE uid=2";
    assert_eq!(
        scalar(db, frozen).await,
        ENTRANTS * 2 - 200,
        "the author's frozen points always equal the unpaid budget"
    );
    let winners =
        format!("SELECT COUNT(*) FROM gonof_forum_lottery_entry WHERE tid={tid} AND amount>0");
    assert_eq!(
        scalar(db, &winners).await,
        100,
        "the first batch of 100 winners was paid"
    );
    let payouts = format!(
        "SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE event_key LIKE 'activity:{tid}:draw:%'"
    );
    assert_eq!(scalar(db, &payouts).await, 200);
    let names: String =
        db::query_scalar("SELECT winners FROM gonof_forum_activity_draw WHERE tid=?")
            .bind(tid)
            .fetch_one(db.executor())
            .await
            .unwrap();
    let names: Vec<i64> = serde_json::from_str(&names).unwrap();
    assert_eq!(names.len(), ENTRANTS as usize);
    assert!(!names.contains(&9));

    gono_forum_web::test_support::drop_trigger(db, "fail_second_draw_batch")
        .await
        .unwrap();
    let settled = activities::process_due_with(db, NOW + 3601 + 600, 100, &backoff)
        .await
        .unwrap();
    assert_eq!(settled.settled, 1);
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={tid} AND state='settled' AND remaining=0")).await,
        1
    );
    assert_eq!(scalar(db, &winners).await, ENTRANTS);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT CAST(SUM(amount) AS SIGNED) FROM gonof_forum_lottery_entry WHERE tid={tid}"
            )
        )
        .await,
        ENTRANTS * 2
    );
    assert_eq!(
        scalar(db, &payouts).await,
        ENTRANTS * 2,
        "resuming does not pay twice"
    );
    assert_eq!(scalar(db, frozen).await, 0);
    assert_eq!(
        scalar(
            db,
            "SELECT CAST(SUM(available) AS SIGNED) FROM gonof_common_credit_account WHERE uid>=10"
        )
        .await,
        ENTRANTS * 2
    );
    fixture.cleanup().await.unwrap();
}

/// The credit ledger pages in descending (time, id) order, which is exactly the order of the
/// `(uid, dateline, id)` index; the paging parameter is still the id of the last entry of the previous page,
/// and someone else's id is treated as having no next page.
#[tokio::test]
async fn mysql_credit_history_pages_follow_time_then_id() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    for uid in 1..=3 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,0)").bind(uid).bind(format!("ledger-{uid}")).bind(format!("ledger-{uid}@example.test")).bind(NOW-172800).execute(db.executor()).await.unwrap();
    }
    db::query("INSERT INTO gonof_common_credit_account(uid,available,frozen) VALUES(2,0,0)")
        .execute(db.executor())
        .await
        .unwrap();
    // 120 ledger entries, ids ascending but times shuffled relative to the ids, plus a few at the same instant.
    for n in 0..120_i64 {
        let dateline = NOW + (n * 37) % 120 / 3;
        db::query("INSERT INTO gonof_common_credit_ledger(event_key,uid,available_delta,frozen_delta,available_after,frozen_after,kind,reason,dateline) VALUES(?,2,1,0,?,0,'admin_adjust','test',?)")
            .bind(format!("ledger-order:{n}"))
            .bind(n)
            .bind(dateline)
            .execute(db.executor())
            .await
            .unwrap();
    }
    db::query("INSERT INTO gonof_common_credit_ledger(event_key,uid,available_delta,frozen_delta,available_after,frozen_after,kind,reason,dateline) VALUES('ledger-order:other',3,1,0,1,0,'admin_adjust','test',?)")
        .bind(NOW)
        .execute(db.executor())
        .await
        .unwrap();
    let expected: Vec<i64> = db::query_scalar(
        "SELECT id FROM gonof_common_credit_ledger WHERE uid=2 ORDER BY dateline DESC,id DESC",
    )
    .fetch_all(db.executor())
    .await
    .unwrap();
    let mut seen = Vec::new();
    let mut before = 0;
    loop {
        let page = activities::credit_page(
            db,
            2,
            before,
            NOW,
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
        )
        .await
        .unwrap();
        assert!(page.entries.len() <= 50);
        seen.extend(page.entries.iter().map(|e| e.id));
        if page.next_before == 0 {
            break;
        }
        before = page.next_before;
    }
    assert_eq!(
        seen, expected,
        "newest first by time, then by id; pages neither repeat nor skip"
    );
    let other: i64 = db::query_scalar("SELECT id FROM gonof_common_credit_ledger WHERE uid=3")
        .fetch_one(db.executor())
        .await
        .unwrap();
    assert!(
        activities::credit_page(
            db,
            2,
            other,
            NOW,
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES
        )
        .await
        .unwrap()
        .entries
        .is_empty(),
        "another member's ledger id cannot be used as the page cursor"
    );
    fixture.cleanup().await.unwrap();
}

/// The activity and credit tables have no check, relation or "ledger is unchangeable" triggers; code keeps those
/// rules (docs/architecture.md, "Data").
/// Every section here holds only because of code: remove the corresponding check and that section fails.
#[tokio::test]
async fn credit_rules_hold_in_code_after_the_triggers_are_gone() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=4 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("rules-{uid}")).bind(format!("rules-{uid}@example.test")).bind(NOW-172800).bind(if uid==1{1}else{0}).execute(db.executor()).await.unwrap();
    }
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    activities::adjust(
        db,
        actor(1),
        2,
        100,
        &identity::random_token(),
        "reconciliation budget",
        NOW,
    )
    .await
    .unwrap();
    let packet = topic(db, fid, paid(2, 12, 3)).await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert!(reconciliation.is_consistent(), "{reconciliation:?}");

    // 1. An activity's category and initiator must be exactly the thread's category and author: a poll cannot be
    // attached to an ordinary thread, nor to someone else's thread.
    let plain = topic(db, fid, ActivitySpec::default()).await;
    let poll = ActivitySpec {
        kind: 4,
        deadline: NOW + 3600,
        max_choices: 1,
        options: vec!["A".into(), "B".into()],
        ..Default::default()
    };
    for (uid, spec) in [
        (2, poll.clone()),
        (
            3,
            ActivitySpec {
                kind: 0,
                ..poll.clone()
            },
        ),
    ] {
        let mut tx = db.begin_write().await.unwrap();
        let result =
            activities::publish(db, &mut tx, uid, plain.tid, &spec, None, "Coins", NOW).await;
        tx.rollback().await.unwrap();
        if spec.kind == 0 {
            assert!(result.is_ok(), "the plain type creates no activity row");
            continue;
        }
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("The activity must match the thread's type and author"),
            "{error}"
        );
    }
    // The poll thread's author is 2; member 3 cannot attach an activity to it.
    let voting = topic(db, fid, poll.clone()).await;
    let mut tx = db.begin_write().await.unwrap();
    let error = activities::publish(db, &mut tx, 3, voting.tid, &poll, None, "Coins", NOW)
        .await
        .unwrap_err()
        .to_string();
    tx.rollback().await.unwrap();
    assert!(
        error.contains("The activity must match the thread's type and author"),
        "{error}"
    );

    // 2. After a red packet is claimed, the unpaid budget cannot exceed the budget: if the remaining amount is
    // raised, code rejects the claim and the reply is rolled back as a whole.
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_activity SET remaining=100 WHERE tid={}",
            packet.tid
        ),
    )
    .await;
    let rejected = writer(db)
        .reply_at(
            3,
            NewReply {
                tid: packet.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Claim refused".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW + 1,
        )
        .await;
    assert!(
        rejected.is_err(),
        "no claim when the remaining amount exceeds the budget"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_redpacket_claim WHERE tid={}",
                packet.tid
            )
        )
        .await,
        0
    );
    // The frozen amount 12 does not match the unpaid budget 100 of the open activity, and reconciliation
    // detects it.
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert_eq!(reconciliation.frozen_activity, vec![2]);
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_activity SET remaining=12 WHERE tid={}",
            packet.tid
        ),
    )
    .await;
    reply(db, 3, packet.tid, None).await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert!(reconciliation.is_consistent(), "{reconciliation:?}");

    // 3. The ledger is unchangeable and accounts change only with the ledger: changing the ledger or an account
    // bypassing the code is detected by reconciliation.
    let last = scalar(
        db,
        "SELECT MAX(id) FROM gonof_common_credit_ledger WHERE uid=3",
    )
    .await;
    execute(
        db,
        &format!(
            "UPDATE gonof_common_credit_ledger SET available_delta=available_delta+1 WHERE id={last}"
        ),
    )
    .await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert_eq!(reconciliation.account_sum, vec![3]);
    assert!(reconciliation.account_last.is_empty());
    execute(
        db,
        &format!(
            "UPDATE gonof_common_credit_ledger SET available_delta=available_delta-1 WHERE id={last}"
        ),
    )
    .await;
    execute(
        db,
        "UPDATE gonof_common_credit_account SET available=available+5 WHERE uid=3",
    )
    .await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert_eq!(reconciliation.account_sum, vec![3]);
    assert_eq!(reconciliation.account_last, vec![3]);
    execute(
        db,
        "INSERT INTO gonof_common_credit_account(uid,available) VALUES(4,-1)",
    )
    .await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert_eq!(reconciliation.negative_account, vec![4]);
    execute(
        db,
        "UPDATE gonof_common_credit_ledger SET available_after=-1 WHERE id=(SELECT id FROM (SELECT MIN(id) AS id FROM gonof_common_credit_ledger) x)",
    )
    .await;
    let reconciliation = activities::reconcile_credits(db).await.unwrap();
    assert_eq!(reconciliation.negative_ledger.len(), 1);
    fixture.cleanup().await.unwrap();
}

/// A lottery with many winners pays at most `PAYOUT_BATCHES_PER_PASS` batches per settlement pass (#163): an activity
/// due after it is settled in the same pass instead of waiting for every batch, and the next pass resumes the payout.
#[tokio::test]
async fn a_large_lottery_pays_a_few_batches_per_pass() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    let per_pass = (activities::PAYOUT_BATCHES_PER_PASS * 100) as i64;
    let entrants = per_pass + 110;
    let members: Vec<String> = (1..=3)
        .chain(10..10 + entrants)
        .map(|uid| {
            format!(
                "({uid},'batch-{uid}','batch-{uid}@example.test','test-password',1,{},{})",
                NOW - 172800,
                i64::from(uid == 1)
            )
        })
        .collect();
    execute(
        db,
        &format!(
            "INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES {}",
            members.join(",")
        ),
    )
    .await;
    activities::adjust(
        db,
        actor(1),
        2,
        entrants + 10,
        &identity::random_token(),
        "test activity budget",
        NOW,
    )
    .await
    .unwrap();
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    let coins = |winners: i64| activities::PrizeSpec {
        name: "Coins".into(),
        winners,
        kind: "credit".into(),
        amount: 1,
        note: String::new(),
        codes: vec![],
    };
    let click = |winners: i64| ActivitySpec {
        kind: 3,
        deadline: NOW + 3600,
        entry_mode: "click".into(),
        prizes: vec![coins(winners)],
        ..Default::default()
    };
    let large = topic(db, fid, click(entrants)).await.tid;
    let small = topic(db, fid, click(1)).await.tid;
    let signups: Vec<String> = (10..10 + entrants)
        .map(|uid| format!("({large},{uid},{})", NOW + 1))
        .collect();
    execute(
        db,
        &format!(
            "INSERT INTO gonof_forum_lottery_signup(tid,uid,dateline) VALUES {}",
            signups.join(",")
        ),
    )
    .await;
    activities::enter(db, actor(3), small, &[], NOW + 1)
        .await
        .unwrap();
    let state = |tid: i64| {
        format!("SELECT COUNT(*) FROM gonof_forum_activity WHERE tid={tid} AND state='settled'")
    };
    let paid =
        format!("SELECT COUNT(*) FROM gonof_forum_lottery_signup WHERE tid={large} AND amount>0");
    let first = activities::process_due(db, NOW + 3601, 20).await.unwrap();
    assert_eq!(
        first,
        activities::DueReport {
            settled: 1,
            paying: 1,
            failed: 0
        }
    );
    assert_eq!(scalar(db, &paid).await, per_pass);
    assert_eq!(scalar(db, &state(large)).await, 0);
    assert_eq!(
        scalar(db, &state(small)).await,
        1,
        "the activity due after the large lottery did not wait for all of its batches"
    );
    let second = activities::process_due(db, NOW + 3606, 20).await.unwrap();
    assert_eq!(
        second,
        activities::DueReport {
            settled: 1,
            paying: 0,
            failed: 0
        }
    );
    assert_eq!(scalar(db, &paid).await, entrants);
    assert_eq!(scalar(db, &state(large)).await, 1);
    assert_eq!(
        scalar(
            db,
            "SELECT frozen FROM gonof_common_credit_account WHERE uid=2"
        )
        .await,
        0
    );
    fixture.cleanup().await.unwrap();
}

/// `settle-due` tells a failed campaign from an empty queue (#35): it exits non-zero, its report counts the failure,
/// and the warning with the reason reaches standard error (the command logs through tracing).
#[tokio::test]
async fn settle_due_exits_non_zero_and_logs_when_a_campaign_fails() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    let now = identity::now();
    for uid in 1..=3 {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)").bind(uid).bind(format!("cli-{uid}")).bind(format!("cli-{uid}@example.test")).bind(now - 172800).bind(i64::from(uid == 1)).execute(db.executor()).await.unwrap();
    }
    activities::adjust(
        db,
        actor(1),
        2,
        100,
        &identity::random_token(),
        "test activity budget",
        now - 7300,
    )
    .await
    .unwrap();
    let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
    let bounty = writer(db)
        .publish_at(
            2,
            NewThread {
                visibility: Default::default(),
                activity: ActivitySpec {
                    deadline: now - 3600,
                    ..paid(1, 10, 1)
                },
                fid,
                title: "Due bounty".into(),
                body: "Due bounty body".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now - 7200,
        )
        .await
        .unwrap()
        .tid;
    gono_forum_web::test_support::fail_when(
        db,
        "fail_cli_settlement",
        "BEFORE UPDATE",
        "gonof_forum_activity",
        &format!("NEW.tid={bounty} AND NEW.state<>OLD.state"),
        "cli settlement contract failure",
    )
    .await
    .unwrap();
    let config = fixture.root.join("settle-due.toml");
    std::fs::write(&config, "[redis]\nenabled = false\n").unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_gono-forum-cli"))
            .args(["settle-due", "10"])
            .env("GONOF_CONFIG", &config)
            .env("GONOF_DATABASE_URL", &fixture.config.database.url)
            .env("GONOF_AUTH_COOKIE_KEY", "11".repeat(32))
            .env("GONOF_TWO_FACTOR_KEY", "22".repeat(32))
            .env_remove("GONOF_AUTH_COOKIE_PREVIOUS_KEYS")
            .env_remove("GONOF_TWO_FACTOR_PREVIOUS_KEYS")
            .env_remove("RUST_LOG")
            .output()
            .unwrap()
    };
    let failed = run();
    let stdout = String::from_utf8_lossy(&failed.stdout);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("Settled 0 due campaigns") && stdout.contains("1 failed"),
        "{stdout}"
    );
    assert!(
        stderr.contains("settling a due activity failed")
            && stderr.contains("cli settlement contract failure"),
        "{stderr}"
    );
    gono_forum_web::test_support::drop_trigger(db, "fail_cli_settlement")
        .await
        .unwrap();
    let settled = run();
    let stdout = String::from_utf8_lossy(&settled.stdout);
    assert!(
        settled.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&settled.stderr)
    );
    assert!(stdout.contains("Settled 1 due campaigns"), "{stdout}");
    fixture.cleanup().await.unwrap();
}
