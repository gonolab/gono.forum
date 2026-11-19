//! Activity cards, poll ballots, check-ins and prize history: what a member is shown agrees with what the activity
//! transactions record.
use gono_forum_web::{
    activities::{self, ActivitySpec},
    content::{
        self,
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

/// A site with boards, member 1 as administrator and members 2 to `members` as ordinary members; returns the board.
async fn site(fixture: &TestFixture, members: i64) -> i64 {
    let db = &fixture.db;
    content::seed_forums(db).await.unwrap();
    gono_forum_web::test_support::raise_posting_quotas(db)
        .await
        .unwrap();
    for uid in 1..=members {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)")
            .bind(uid)
            .bind(format!("card-{uid}"))
            .bind(format!("card-{uid}@example.test"))
            .bind(NOW - 172_800)
            .bind(if uid == 1 { 1 } else { 0 })
            .execute(db.executor())
            .await
            .unwrap();
    }
    scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await
}

async fn topic(db: &Database, fid: i64, spec: ActivitySpec) -> Published {
    writer(db)
        .publish_at(
            2,
            NewThread {
                visibility: Default::default(),
                activity: spec,
                fid,
                title: "Activity card thread".into(),
                body: "Activity card body".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            NOW,
        )
        .await
        .unwrap()
}

async fn reply_with(db: &Database, uid: i64, tid: i64, key: &str) -> Published {
    writer(db)
        .reply_at(
            uid,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A valid activity reply".into(),
                attachment_ids: vec![],
                request_key: key.into(),
            },
            NOW + 1,
        )
        .await
        .unwrap()
}

fn poll(max_choices: i64) -> ActivitySpec {
    ActivitySpec {
        kind: 4,
        deadline: NOW + 3600,
        max_choices,
        options: vec!["Option one".into(), "Option two".into()],
        ..Default::default()
    }
}

/// A vote's request key lives in the request ledger shared with posting (docs/api.md, "Idempotency keys"): a key
/// already used for a reply is not a new ballot, and once the ledger has forgotten a key, the same key casts a ballot
/// in another poll.
#[tokio::test]
async fn a_vote_key_is_one_request_in_the_shared_request_ledger() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(&fixture, 4).await;
    let first = topic(db, fid, poll(1)).await;
    let second = topic(db, fid, poll(1)).await;

    // The key of a reply cannot also cast a ballot.
    let replied = identity::random_token();
    reply_with(db, 3, first.tid, &replied).await;
    let refused = activities::vote(db, actor(3), second.tid, vec![1], &replied, NOW + 2).await;
    assert!(
        refused.is_err(),
        "a key already used for a reply is another request"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT voters FROM gonof_forum_poll WHERE tid={}",
                second.tid
            )
        )
        .await,
        0
    );

    // A ballot replays with its own key, and is recorded in the ledger against the poll's first post.
    let key = identity::random_token();
    activities::vote(db, actor(4), first.tid, vec![2], &key, NOW + 2)
        .await
        .unwrap();
    activities::vote(db, actor(4), first.tid, vec![2], &key, NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid=4 AND pid={}",
                first.pid
            )
        )
        .await,
        1
    );
    // After the retention period the purge forgets the key: the same key is then a new request.
    execute(db, "DELETE FROM gonof_forum_post_request WHERE uid=4").await;
    activities::vote(db, actor(4), second.tid, vec![1], &key, NOW + 4)
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT voters FROM gonof_forum_poll WHERE tid={}",
                second.tid
            )
        )
        .await,
        1
    );
    fixture.cleanup().await.unwrap();
}

/// The voter total and the option counts come from one snapshot: while ballots commit, a single-choice poll's card
/// never shows more votes on its options than voters, or fewer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_poll_card_reads_its_totals_from_one_snapshot_while_ballots_commit() {
    const VOTERS: i64 = 120;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = fixture.db.clone();
    let fid = site(&fixture, VOTERS + 2).await;
    let poll = topic(&db, fid, poll(1)).await;
    let tid = poll.tid;
    let voting = {
        let db = db.clone();
        tokio::spawn(async move {
            for uid in 3..VOTERS + 3 {
                activities::vote(
                    &db,
                    actor(uid),
                    tid,
                    vec![1 + uid % 2],
                    &identity::random_token(),
                    NOW + 1,
                )
                .await
                .unwrap();
            }
        })
    };
    let mut reads = 0;
    let mut torn = Vec::new();
    while !voting.is_finished() {
        let card = activities::card(&db, tid, None).await.unwrap().unwrap();
        let votes: i64 = card.options.iter().map(|option| option.votes).sum();
        if votes != card.voters {
            torn.push((card.voters, votes));
        }
        reads += 1;
    }
    voting.await.unwrap();
    assert!(reads > 0);
    assert!(
        torn.is_empty(),
        "{} of {reads} cards mixed two moments (voters, option votes): {:?}",
        torn.len(),
        &torn[..torn.len().min(5)]
    );
    let card = activities::card(&db, tid, None).await.unwrap().unwrap();
    assert_eq!(card.voters, VOTERS);
    fixture.cleanup().await.unwrap();
}

/// After account deletion, the stored username carries the former UID; the public winners list names such a winner
/// "Deleted user" and gives no UID, so results of different lotteries cannot be linked to the same former member.
#[tokio::test]
async fn a_deleted_winner_is_listed_without_name_or_uid() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(&fixture, 4).await;
    activities::adjust(
        db,
        actor(1),
        2,
        100,
        &identity::random_token(),
        "lottery budget",
        NOW - 10,
    )
    .await
    .unwrap();
    let lottery = topic(
        db,
        fid,
        ActivitySpec {
            kind: 3,
            budget: 20,
            shares: 2,
            deadline: NOW + 3600,
            ..Default::default()
        },
    )
    .await;
    reply_with(db, 3, lottery.tid, &identity::random_token()).await;
    reply_with(db, 4, lottery.tid, &identity::random_token()).await;
    activities::process_due(db, NOW + 3601, 100).await.unwrap();
    // What account deletion leaves on the member row: the state, and a replacement name with the former UID.
    execute(
        db,
        "UPDATE gonof_common_member SET account_state='deleted',username='deleted-3-a1b2c3' WHERE uid=3",
    )
    .await;
    let card = activities::card(db, lottery.tid, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.state, "settled");
    let winners = serde_json::to_value(&card.winners).unwrap();
    let winners = winners.as_array().unwrap();
    assert_eq!(winners.len(), 2);
    let deleted = winners
        .iter()
        .find(|winner| winner["username"] == "Deleted user")
        .expect("the deleted winner is listed as a deleted user");
    assert!(deleted["uid"].is_null(), "{deleted}");
    assert!(
        winners
            .iter()
            .any(|winner| winner["uid"] == 4 && winner["username"] == "card-4")
    );
    assert!(!serde_json::to_string(&card).unwrap().contains("deleted-3"));
    fixture.cleanup().await.unwrap();
}

/// Check-in days follow the site's current time zone everywhere: a check-in counts for the day its time falls on
/// now, as on the calendar, not for the day stored in its key, so after a time zone change the credits page, the
/// sidebar button and the check-in itself agree with the calendar.
#[tokio::test]
async fn check_in_days_follow_the_current_time_zone_like_the_calendar() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    site(&fixture, 3).await;
    execute(
        db,
        "UPDATE gonof_common_credit_rule SET enabled=1,amount=1,daily_limit=1 WHERE rule_key='signin'",
    )
    .await;
    let set_offset = |minutes: &'static str| async move {
        gono_forum_web::test_support::set_setting(db, "settings.time.offset_minutes", minutes)
            .await
            .unwrap();
    };
    let rules = &activities::rules(db).await.unwrap();
    let today = |at: i64, offset: i64| async move {
        let page = activities::credit_page(db, 3, 0, at, offset).await.unwrap();
        let calendar = activities::signin_stats(db, 3, at, offset).await.unwrap();
        let cell = calendar
            .weeks
            .iter()
            .flatten()
            .flatten()
            .find(|day| day.today)
            .unwrap()
            .checked;
        let sidebar = activities::signin_state(db, 3, at, offset, rules)
            .await
            .unwrap()
            .checked_in;
        (page.checked_in, sidebar, cell)
    };
    let midnight = NOW - NOW.rem_euclid(86_400);
    // 16:30 UTC is 00:30 the next day in UTC+8: the check-in is stored with the key of that next day.
    set_offset("480").await;
    let first = midnight + 16 * 3600 + 1800;
    assert!(activities::check_in(db, actor(3), first).await.unwrap());
    // The site moves to UTC: the same check-in now falls on the UTC day it was made.
    set_offset("0").await;
    let evening = midnight + 17 * 3600;
    assert_eq!(
        today(evening, 0).await,
        (true, true, true),
        "credits page, sidebar and calendar"
    );
    assert!(
        !activities::check_in(db, actor(3), evening).await.unwrap(),
        "already checked in today"
    );
    // The next UTC day has no check-in yet, although its key is the one the UTC+8 check-in took.
    let next = midnight + 86_400 + 10 * 3600;
    assert_eq!(today(next, 0).await, (false, false, false));
    assert!(
        activities::check_in(db, actor(3), next).await.unwrap(),
        "a day the calendar shows unchecked can be checked in"
    );
    assert_eq!(today(next, 0).await, (true, true, true));
    let day = midnight / 86_400;
    let keys: Vec<String> = db::query_scalar(
        "SELECT event_key FROM gonof_common_credit_ledger WHERE uid=3 AND kind='signin' ORDER BY id",
    )
    .fetch_all(db.executor())
    .await
    .unwrap();
    assert_eq!(
        keys,
        [
            format!("signin:3:{}", day + 1),
            format!("signin:3:{}:0", day + 1)
        ]
    );
    fixture.cleanup().await.unwrap();
}

/// The credits page reads the balance and the history in one snapshot: while entries commit, the newest entry on the
/// page always ends at the balance shown above it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_credits_page_reads_balance_and_history_from_one_snapshot() {
    const ENTRIES: i64 = 150;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = fixture.db.clone();
    site(&fixture, 3).await;
    let crediting = {
        let db = db.clone();
        tokio::spawn(async move {
            for n in 0..ENTRIES {
                activities::adjust(
                    &db,
                    actor(1),
                    3,
                    1,
                    &identity::random_token(),
                    "snapshot check",
                    NOW + n,
                )
                .await
                .unwrap();
            }
        })
    };
    let mut reads = 0;
    let mut torn = Vec::new();
    while !crediting.is_finished() {
        let page = activities::credit_page(&db, 3, 0, NOW + ENTRIES, 0)
            .await
            .unwrap();
        if let Some(newest) = page.entries.first()
            && newest.available_after != page.available
        {
            torn.push((page.available, newest.available_after));
        }
        reads += 1;
    }
    crediting.await.unwrap();
    assert!(reads > 0);
    assert!(
        torn.is_empty(),
        "{} of {reads} pages mixed two moments (balance, newest entry): {:?}",
        torn.len(),
        &torn[..torn.len().min(5)]
    );
    fixture.cleanup().await.unwrap();
}

/// "My prizes" keeps a prize whose thread the winner can no longer open, but not the thread's current title: a thread
/// moved to a board the winner cannot read and renamed, or withdrawn, shows a placeholder instead.
#[tokio::test]
async fn prize_history_hides_threads_the_winner_can_no_longer_open() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let fid = site(&fixture, 4).await;
    let ring = fixture.config.identity.two_factor.clone();
    let lottery = topic(
        db,
        fid,
        ActivitySpec {
            kind: 3,
            deadline: NOW + 3600,
            prizes: vec![activities::PrizeSpec {
                name: "Signed poster".into(),
                winners: 1,
                kind: "item".into(),
                amount: 0,
                note: "Sent by post".into(),
                codes: vec![],
            }],
            ..Default::default()
        },
    )
    .await;
    reply_with(db, 3, lottery.tid, &identity::random_token()).await;
    activities::process_due(db, NOW + 3601, 100).await.unwrap();
    let subjects = || async {
        let (items, _) = activities::my_prizes(db, &ring, 3, 0).await.unwrap();
        items
            .into_iter()
            .map(|item| (item.prize, item.subject))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        subjects().await,
        [(
            "Signed poster".to_owned(),
            "Activity card thread".to_owned()
        )]
    );
    // Renamed after the draw and its board made private.
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET subject='Renamed behind closed doors' WHERE tid={}",
            lottery.tid
        ),
    )
    .await;
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='private' WHERE fid={fid}"),
    )
    .await;
    let hidden = subjects().await;
    assert_eq!(hidden.len(), 1, "the prize stays listed");
    assert_eq!(hidden[0].0, "Signed poster");
    assert!(
        !hidden[0].1.contains("Renamed"),
        "the new title of a thread the winner cannot read: {hidden:?}"
    );
    // Back in a readable board, but withdrawn.
    execute(
        db,
        &format!("UPDATE gonof_forum_forum SET visibility='public' WHERE fid={fid}"),
    )
    .await;
    assert_eq!(subjects().await[0].1, "Renamed behind closed doors");
    execute(
        db,
        &format!(
            "UPDATE gonof_forum_thread SET deleted_at={} WHERE tid={}",
            NOW + 3700,
            lottery.tid
        ),
    )
    .await;
    assert!(
        !subjects().await[0].1.contains("Renamed"),
        "the title of a withdrawn thread"
    );
    fixture.cleanup().await.unwrap();
}

/// The points name stands for the credit type in the lottery prize format, whose columns are separated by `|`: a new
/// name cannot contain `|` or be a word the format reads as another prize type, while a site that already uses such
/// a name can still save its other settings.
#[tokio::test]
async fn the_points_name_cannot_take_the_place_of_another_prize_type() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    site(&fixture, 2).await;
    let global = &fixture.global;
    let save = |name: &'static str| async move {
        gono_forum_web::runtime::update(db, global, actor(1), "site.settings.save", |settings| {
            settings.site.credit_name = name.into()
        })
        .await
    };
    // "Code" and "Other" are the English words for the code and item types; U+5151 U+6362 U+7801 is the Simplified
    // Chinese word for a redemption code.
    for refused in [
        "item",
        "code",
        "credit|7",
        "Code",
        "Other",
        "\u{5151}\u{6362}\u{7801}",
    ] {
        assert!(save(refused).await.is_err(), "{refused}");
    }
    save("Coins").await.unwrap();
    // What a site saved before the rule: reading and saving other settings keep working.
    gono_forum_web::test_support::set_setting(db, "settings.site.credit_name", "item")
        .await
        .unwrap();
    gono_forum_web::runtime::update(db, global, actor(1), "site.settings.save", |settings| {
        settings.site.description = "Still saves".into()
    })
    .await
    .unwrap();
    fixture.cleanup().await.unwrap();
}
