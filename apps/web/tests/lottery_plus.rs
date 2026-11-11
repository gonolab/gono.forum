//! One-click lottery sign-up, author review, early draw, multiple prizes and redemption codes.
use std::sync::{Arc, Mutex, OnceLock};

use gono_forum_web::{
    activities::{self, ActivitySpec, PrizeSpec},
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, TwoFactorConfig},
    test_support::TestFixture,
};

const NOW: i64 = 1_800_000_000;
const SECRET: &str = "SECRET-CODE-7Q2X";

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}
fn writer(db: &Database, ring: &TwoFactorConfig) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default()).with_codes(ring.clone())
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn text(db: &Database, sql: &str) -> String {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
async fn execute(db: &Database, sql: &str) {
    gono_forum_web::db::raw_sql(sql)
        .execute(&db.pool)
        .await
        .unwrap();
}
fn prize(name: &str, winners: i64, kind: &str, amount: i64, codes: &[&str]) -> PrizeSpec {
    PrizeSpec {
        name: name.into(),
        winners,
        kind: kind.into(),
        amount,
        note: String::new(),
        codes: codes.iter().map(|c| (*c).to_owned()).collect(),
    }
}
fn lottery(entry_mode: &str, prizes: Vec<PrizeSpec>) -> ActivitySpec {
    ActivitySpec {
        kind: 3,
        deadline: NOW + 3600,
        entry_mode: entry_mode.into(),
        prizes,
        ..Default::default()
    }
}

struct World {
    fixture: TestFixture,
    ring: TwoFactorConfig,
    fid: i64,
}
impl World {
    /// Administrator 1, author 2 (1000 credits), ordinary members 3..=last, all with verified emails.
    async fn new(last: i64) -> Self {
        let fixture = TestFixture::migrated().await.unwrap();
        let db = &fixture.db;
        content::seed_forums(db).await.unwrap();
        gono_forum_web::test_support::raise_posting_quotas(db)
            .await
            .unwrap();
        for uid in 1..=last {
            db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate,adminid) VALUES(?,?,?,'test-password',1,?,?)")
                .bind(uid).bind(format!("lottery-{uid}")).bind(format!("lottery-{uid}@example.test")).bind(NOW - 172800).bind(if uid == 1 { 1 } else { 0 })
                .execute(db.executor()).await.unwrap();
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
        let fid = scalar(db, "SELECT MIN(fid) FROM gonof_forum_forum").await;
        let ring = fixture.config.identity.two_factor.clone();
        Self { fixture, ring, fid }
    }
    fn db(&self) -> &Database {
        &self.fixture.db
    }
    async fn publish(&self, spec: ActivitySpec) -> Published {
        writer(self.db(), &self.ring)
            .publish_at(
                2,
                NewThread {
                    visibility: Default::default(),
                    activity: spec,
                    fid: self.fid,
                    title: "Lottery extension thread".into(),
                    body: "Lottery extension body".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                NOW,
            )
            .await
            .unwrap()
    }
    async fn reply(&self, uid: i64, tid: i64) -> Published {
        writer(self.db(), &self.ring)
            .reply_at(
                uid,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: "Entering the lottery".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                NOW + 1,
            )
            .await
            .unwrap()
    }
    async fn withdraw_first(&self, pid: i64, at: i64) {
        let editor = ContentEditor::new(self.db().clone(), Default::default(), 86400 * 30);
        let post = editor.editable_at(actor(2), pid, at).await.unwrap();
        editor
            .change_at(
                actor(2),
                ChangeRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: String::new(),
                    action: ContentAction::Withdraw,
                },
                at,
            )
            .await
            .unwrap();
    }
    async fn frozen(&self, uid: i64) -> i64 {
        scalar(
            self.db(),
            &format!("SELECT frozen FROM gonof_common_credit_account WHERE uid={uid}"),
        )
        .await
    }
    async fn available(&self, uid: i64) -> i64 {
        db::query_scalar("SELECT available FROM gonof_common_credit_account WHERE uid=?")
            .bind(uid)
            .fetch_optional(self.db().executor())
            .await
            .unwrap()
            .unwrap_or(0)
    }
    async fn activity(&self, tid: i64) -> (String, i64, i64) {
        let row =
            db::query("SELECT state,remaining,deadline FROM gonof_forum_activity WHERE tid=?")
                .bind(tid)
                .fetch_one(self.db().executor())
                .await
                .unwrap();
        (
            db::text(&row, "state").unwrap(),
            row.try_get("remaining").unwrap(),
            row.try_get("deadline").unwrap(),
        )
    }
    async fn winners(&self, tid: i64) -> (Vec<i64>, Vec<i64>) {
        let row = db::query("SELECT eligible,winners FROM gonof_forum_activity_draw WHERE tid=?")
            .bind(tid)
            .fetch_one(self.db().executor())
            .await
            .unwrap();
        (
            serde_json::from_str(&db::text(&row, "eligible").unwrap()).unwrap(),
            serde_json::from_str(&db::text(&row, "winners").unwrap()).unwrap(),
        )
    }
}

fn message(error: anyhow::Error) -> String {
    format!("{error:#}")
}

/// One-click sign-up, item-by-item confirmation, author removal and its reversal, early draw; mixed prizes are
/// assigned in list order and the books balance.
#[tokio::test]
async fn mysql_click_entry_review_and_early_draw_with_mixed_prizes() {
    let w = World::new(9).await;
    let db = w.db();
    let mut spec = lottery(
        "click",
        vec![
            prize("First prize", 1, "code", 0, &[SECRET]),
            prize("Second prize", 2, "credit", 50, &[]),
            prize("Souvenir", 1, "item", 0, &[]),
        ],
    );
    spec.requirements = vec!["Follow the author".into(), "Share this thread".into()];
    spec.review = true;
    spec.early_draw = true;
    let thread = w.publish(spec).await;
    let tid = thread.tid;
    assert_eq!(w.frozen(2).await, 100, "only point prizes are frozen");
    assert_eq!(w.available(2).await, 900);
    assert_eq!(
        scalar(
            db,
            &format!("SELECT shares FROM gonof_forum_lottery WHERE tid={tid}")
        )
        .await,
        4
    );
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_forum_lottery_award WHERE tid={tid} AND code_ciphertext IS NOT NULL")).await,
        1
    );
    let stored = text(
        db,
        &format!(
            "SELECT code_ciphertext FROM gonof_forum_lottery_award WHERE tid={tid} AND prize_no=1"
        ),
    )
    .await;
    assert!(
        stored.starts_with("v1.") && !stored.contains(SECRET),
        "codes are stored encrypted"
    );

    // Item-by-item confirmation: one item too few or too many is refused; the author cannot sign up; replies do not
    // sign up.
    for confirm in [&[][..], &[1][..], &[1, 2, 3][..]] {
        let error = activities::enter(db, actor(3), tid, confirm, NOW + 5)
            .await
            .unwrap_err();
        assert!(message(error).contains("Please confirm every entry requirement"));
    }
    assert!(
        message(
            activities::enter(db, actor(2), tid, &[1, 2], NOW + 5)
                .await
                .unwrap_err()
        )
        .contains("Authors cannot enter their own lottery")
    );
    w.reply(4, tid).await;
    assert_eq!(
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_forum_lottery_entry WHERE tid={tid}")
        )
        .await,
        0,
        "replying does not enter a one-click lottery"
    );
    assert!(
        activities::enter(db, actor(3), tid, &[2, 1], NOW + 5)
            .await
            .unwrap()
    );
    assert!(
        !activities::enter(db, actor(3), tid, &[1, 2], NOW + 6)
            .await
            .unwrap(),
        "a repeated entry returns the first result"
    );
    for uid in 4..=8 {
        assert!(
            activities::enter(db, actor(uid), tid, &[1, 2], NOW + 6)
                .await
                .unwrap()
        );
    }

    // Removal: only the author, a reason is required, and the other side must have signed up; after reversal they are
    // back on the list.
    assert!(
        message(
            activities::exclude(db, actor(3), tid, 4, "not eligible", NOW + 7)
                .await
                .unwrap_err()
        )
        .contains("Only the lottery's author")
    );
    assert!(
        message(
            activities::exclude(db, actor(2), tid, 4, "  ", NOW + 7)
                .await
                .unwrap_err()
        )
        .contains("reason of 1 to 200 characters for the removal")
    );
    assert!(
        message(
            activities::exclude(db, actor(2), tid, 9, "never entered", NOW + 7)
                .await
                .unwrap_err()
        )
        .contains("has not entered")
    );
    activities::exclude(db, actor(2), tid, 4, "did not share", NOW + 7)
        .await
        .unwrap();
    activities::exclude(db, actor(2), tid, 4, "another reason", NOW + 7)
        .await
        .unwrap();
    activities::exclude(db, actor(2), tid, 5, "removed by mistake", NOW + 7)
        .await
        .unwrap();
    activities::restore(db, actor(2), tid, 5, NOW + 8)
        .await
        .unwrap();
    activities::restore(db, actor(2), tid, 5, NOW + 8)
        .await
        .unwrap();
    let card = activities::card(db, tid, Some(4)).await.unwrap().unwrap();
    assert_eq!((card.entries, card.excluded), (6, 1));
    assert_eq!(
        card.my_excluded, "did not share",
        "removing again keeps the first reason"
    );
    assert_eq!(card.requirements.len(), 2);
    assert!(card.review && card.early_draw && card.entry_mode == "click");
    let other = activities::card(db, tid, Some(5)).await.unwrap().unwrap();
    assert!(other.my_excluded.is_empty() && other.my_entry && !other.can_enter);
    let anonymous = activities::card(db, tid, None).await.unwrap().unwrap();
    assert!(anonymous.my_excluded.is_empty() && !anonymous.can_draw);
    assert!(
        activities::card(db, tid, Some(9))
            .await
            .unwrap()
            .unwrap()
            .can_enter
    );
    assert!(
        activities::card(db, tid, Some(2))
            .await
            .unwrap()
            .unwrap()
            .can_draw
    );
    let audit = scalar(
        db,
        &format!("SELECT COUNT(*) FROM gonof_common_log WHERE type IN ('activity.lottery.exclude','activity.lottery.restore') AND data LIKE '%\"tid\":{tid}%'"),
    )
    .await;
    assert_eq!(audit, 3);

    // Member 8 is frozen after signing up: not eligible at the draw.
    execute(db, "UPDATE gonof_common_member SET `freeze`=1 WHERE uid=8").await;

    // Early draw: only the author; afterwards sign-up, removal and replacing the whole set of codes are all refused.
    assert!(
        message(
            activities::draw_now(db, actor(3), tid, NOW + 10)
                .await
                .unwrap_err()
        )
        .contains("Only the lottery's author")
    );
    assert!(
        activities::draw_now(db, actor(2), tid, NOW + 10)
            .await
            .unwrap()
    );
    assert!(
        !activities::draw_now(db, actor(2), tid, NOW + 10)
            .await
            .unwrap()
    );
    assert_eq!(w.activity(tid).await, ("drawing".into(), 100, NOW + 11));
    assert_eq!(
        scalar(
            db,
            &format!("SELECT planned_deadline FROM gonof_forum_lottery WHERE tid={tid}")
        )
        .await,
        NOW + 3600
    );
    assert!(
        message(
            activities::enter(db, actor(9), tid, &[1, 2], NOW + 10)
                .await
                .unwrap_err()
        )
        .contains("The lottery has closed")
    );
    assert!(
        message(
            activities::exclude(db, actor(2), tid, 3, "too late", NOW + 10)
                .await
                .unwrap_err()
        )
        .contains("the entries are locked")
    );
    assert!(
        message(
            activities::replace_codes(db, &w.ring, actor(2), tid, 1, &["NEW".into()], NOW + 10)
                .await
                .unwrap_err()
        )
        .contains("The draw has started")
    );
    assert_eq!(activities::process_due(db, NOW + 11, 20).await.unwrap(), 1);

    let (eligible, winners) = w.winners(tid).await;
    assert_eq!(
        eligible,
        [3, 5, 6, 7],
        "removed member 4 and frozen member 8 do not count"
    );
    assert_eq!(winners.len(), 4);
    // Assigned in list order: person 1 gets the first prize (a code), persons 2 and 3 the second prize (50 credits
    // each), person 4 the souvenir.
    let code_winner = scalar(
        db,
        &format!(
            "SELECT uid FROM gonof_forum_lottery_award WHERE tid={tid} AND prize_no=1 AND slot_no=1"
        ),
    )
    .await;
    assert_eq!(code_winner, winners[0]);
    for uid in &winners[1..3] {
        assert_eq!(
            scalar(
                db,
                &format!(
                    "SELECT amount FROM gonof_forum_lottery_signup WHERE tid={tid} AND uid={uid}"
                )
            )
            .await,
            50
        );
        assert_eq!(w.available(*uid).await, 50);
    }
    assert_eq!(
        scalar(
            db,
            &format!("SELECT uid FROM gonof_forum_lottery_award WHERE tid={tid} AND prize_no=3")
        )
        .await,
        winners[3]
    );
    assert_eq!(w.activity(tid).await.0, "settled");
    assert_eq!(w.frozen(2).await, 0);
    assert_eq!(
        w.available(2).await,
        900,
        "all points paid out, nothing refunded"
    );
    assert_eq!(
        scalar(
            db,
            &format!("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE tid={tid}")
        )
        .await,
        5,
        "one freeze row, and one row out and one in for each of the two point winners"
    );

    let card = activities::card(db, tid, Some(winners[0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (card.my_prize.as_str(), card.my_prize_kind.as_str()),
        ("First prize", "code")
    );
    assert_eq!(card.planned_deadline, NOW + 3600);
    assert_eq!(card.winners.len(), 4);
    assert_eq!(
        card.winners
            .iter()
            .map(|w| (w.uid, w.prize_no))
            .collect::<Vec<_>>(),
        winners
            .iter()
            .copied()
            .zip([1, 2, 2, 3])
            .collect::<Vec<_>>()
    );
    let json = serde_json::to_string(&card).unwrap();
    assert!(
        !json.contains(SECRET) && !json.contains(&stored),
        "the card holds no code"
    );
    let (mine, next) = activities::my_prizes(db, &w.ring, winners[0], 0)
        .await
        .unwrap();
    assert!(next.is_none());
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].code.as_deref(), Some(SECRET));
    let (theirs, _) = activities::my_prizes(db, &w.ring, winners[3], 0)
        .await
        .unwrap();
    assert_eq!(
        (
            theirs.len(),
            theirs[0].kind.as_str(),
            theirs[0].code.is_none()
        ),
        (1, "item", true)
    );
    assert!(
        activities::my_prizes(db, &w.ring, winners[1], 0)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    w.fixture.cleanup().await.unwrap();
}

/// With vacant slots the unpaid credits are refunded and codes stay with the author; hiding the first post before the
/// draw refunds in full and assigns no codes.
#[tokio::test]
async fn mysql_vacant_prizes_refund_and_cancelling_before_the_draw_refunds_everything() {
    let w = World::new(6).await;
    let db = w.db();
    let vacant = w
        .publish(lottery(
            "reply",
            vec![
                prize("Coins", 3, "credit", 10, &[]),
                prize("Codes", 2, "code", 0, &["X-1", "X-2"]),
            ],
        ))
        .await;
    let cancelled = w
        .publish(lottery(
            "reply",
            vec![
                prize("Grand prize", 1, "credit", 200, &[]),
                prize("Code", 1, "code", 0, &["Y-1"]),
            ],
        ))
        .await;
    assert_eq!(w.frozen(2).await, 230);
    for uid in [3, 4] {
        w.reply(uid, vacant.tid).await;
    }
    w.reply(5, cancelled.tid).await;

    w.withdraw_first(cancelled.pid, NOW + 100).await;
    assert_eq!(
        w.activity(cancelled.tid).await,
        ("cancelled".into(), 0, NOW + 3600)
    );
    assert_eq!(
        w.frozen(2).await,
        30,
        "withdrawing the first post refunds everything frozen"
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_lottery_award WHERE tid={} AND uid IS NOT NULL",
                cancelled.tid
            )
        )
        .await,
        0
    );
    let board = activities::code_board(db, &w.ring, 2, false, cancelled.tid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        board.prizes[0].slots[0].code.as_deref(),
        Some("Y-1"),
        "the author still sees the codes not given out"
    );
    assert!(!board.can_replace);

    assert_eq!(
        activities::process_due(db, NOW + 3600, 20).await.unwrap(),
        1
    );
    let (_, winners) = w.winners(vacant.tid).await;
    assert_eq!(winners.len(), 2, "only two members entered");
    assert_eq!(w.activity(vacant.tid).await.0, "settled");
    assert_eq!(w.frozen(2).await, 0);
    assert_eq!(
        w.available(2).await,
        1000 - 20,
        "the 10 points of the vacant slot are refunded"
    );
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE tid={} AND kind='activity_refund'", vacant.tid)).await,
        1
    );
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM gonof_forum_lottery_award WHERE tid={} AND uid IS NOT NULL",
                vacant.tid
            )
        )
        .await,
        0,
        "both won points, and no code was given out"
    );
    let board = activities::code_board(db, &w.ring, 2, false, vacant.tid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        board.prizes[0]
            .slots
            .iter()
            .map(|s| s.code.clone().unwrap())
            .collect::<Vec<_>>(),
        ["X-1", "X-2"]
    );
    // Only administrators see the distribution; other members cannot.
    let admin = activities::code_board(db, &w.ring, 1, true, vacant.tid)
        .await
        .unwrap()
        .unwrap();
    assert!(
        admin.prizes[0]
            .slots
            .iter()
            .all(|s| s.code.is_none() && !s.unreadable)
    );
    assert!(
        activities::code_board(db, &w.ring, 3, false, vacant.tid)
            .await
            .unwrap()
            .is_none()
    );
    w.fixture.cleanup().await.unwrap();
}

/// Once the list is fixed and awarding is half done when the first post is withdrawn, the rest is still paid out,
/// with no refund and no cancellation.
/// Removing the check in `terminate` makes this fail at the "still awarding" check.
#[tokio::test]
async fn mysql_withdrawing_after_the_list_is_decided_still_pays_everyone() {
    let w = World::new(6).await;
    let db = w.db();
    let thread = w
        .publish(lottery(
            "reply",
            vec![
                prize("First prize", 1, "code", 0, &["Z-1"]),
                prize("Second prize", 3, "credit", 20, &[]),
            ],
        ))
        .await;
    let tid = thread.tid;
    for uid in 3..=6 {
        w.reply(uid, tid).await;
    }
    // The first credit payout batch fails: the list and codes are already fixed and committed, and not a single credit
    // was paid.
    gono_forum_web::test_support::fail_when(
        db,
        "fail_lottery_payout",
        "BEFORE UPDATE",
        "gonof_forum_lottery_entry",
        &format!("NEW.tid={tid} AND NEW.amount>0"),
        "payout contract failure",
    )
    .await
    .unwrap();
    let backoff = activities::DueBackoff::default();
    assert_eq!(
        activities::process_due_with(db, NOW + 3600, 20, &backoff)
            .await
            .unwrap(),
        0
    );
    let (_, winners) = w.winners(tid).await;
    assert_eq!(winners.len(), 4);
    assert_eq!(w.activity(tid).await.1, 60);
    assert_eq!(
        scalar(
            db,
            &format!("SELECT uid FROM gonof_forum_lottery_award WHERE tid={tid}")
        )
        .await,
        winners[0]
    );

    w.withdraw_first(thread.pid, NOW + 3601).await;
    assert_eq!(
        w.activity(tid).await.0,
        "drawing",
        "withdrawing the first post after the list is fixed does not cancel"
    );
    assert_eq!(w.frozen(2).await, 60, "and does not refund");

    gono_forum_web::test_support::drop_trigger(db, "fail_lottery_payout")
        .await
        .unwrap();
    assert_eq!(
        activities::process_due_with(db, NOW + 3601 + 600, 20, &backoff)
            .await
            .unwrap(),
        1
    );
    assert_eq!(w.activity(tid).await.0, "settled");
    for uid in &winners[1..] {
        assert_eq!(w.available(*uid).await, 20);
    }
    assert_eq!(w.frozen(2).await, 0);
    assert_eq!(w.available(2).await, 940);
    assert_eq!(
        scalar(
            db,
            &format!("SELECT uid FROM gonof_forum_lottery_award WHERE tid={tid}")
        )
        .await,
        winners[0],
        "the code is not taken back"
    );
    // Withdrawal before the list is fixed still cancels and refunds.
    let other = w
        .publish(lottery("reply", vec![prize("Coins", 1, "credit", 5, &[])]))
        .await;
    w.reply(3, other.tid).await;
    w.withdraw_first(other.pid, NOW + 100).await;
    assert_eq!(w.activity(other.tid).await.0, "cancelled");
    w.fixture.cleanup().await.unwrap();
}

/// Sign-up, removal, early draw and the background draw run at the same time: the list only contains sign-ups
/// submitted before step 0 and not removed, all later sign-ups and removals are refused, and two background processes
/// settling at once still pay out only once.
#[tokio::test]
async fn mysql_entries_reviews_and_draws_race_without_changing_the_decided_list() {
    const LAST: i64 = 40;
    let w = World::new(LAST).await;
    let db = w.db();
    let mut spec = lottery(
        "click",
        vec![
            prize("Coins", 10, "credit", 3, &[]),
            prize("Code", 2, "code", 0, &["R-1", "R-2"]),
        ],
    );
    spec.review = true;
    spec.early_draw = true;
    let tid = w.publish(spec).await.tid;
    for uid in 3..=12 {
        activities::enter_live(db, actor(uid), tid, &[])
            .await
            .unwrap();
    }
    let db_clone = db.clone();
    let mut tasks = tokio::task::JoinSet::new();
    for uid in 13..=LAST {
        let db = db_clone.clone();
        tasks.spawn(async move {
            (
                uid,
                activities::enter_live(&db, actor(uid), tid, &[])
                    .await
                    .map(|_| ()),
            )
        });
    }
    for uid in (3..=LAST).step_by(3) {
        let db = db_clone.clone();
        tasks.spawn(async move {
            (
                -uid,
                activities::exclude_live(&db, actor(2), tid, uid, "concurrent removal").await,
            )
        });
    }
    {
        let db = db_clone.clone();
        tasks.spawn(async move {
            (
                0,
                activities::draw_now_live(&db, actor(2), tid)
                    .await
                    .map(|_| ()),
            )
        });
    }
    let mut drew = false;
    while let Some(joined) = tasks.join_next().await {
        let (who, result) = joined.unwrap();
        match (who, result) {
            (0, result) => drew = result.is_ok(),
            (_, Ok(())) => {}
            (_, Err(error)) => {
                let text = message(error);
                assert!(
                    text.contains("has closed")
                        || text.contains("are locked")
                        || text.contains("has not entered"),
                    "{text}"
                );
            }
        }
    }
    assert!(drew);
    let (state, _, deadline) = w.activity(tid).await;
    assert_eq!(state, "drawing");
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_forum_lottery_signup WHERE tid={tid} AND dateline>={deadline}")).await,
        0,
        "no new entries after step 0"
    );
    let later = identity::now().max(deadline) + 5;
    let (a, b) = tokio::join!(
        activities::process_due(db, later, 20),
        activities::process_due(db, later, 20)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1, "settled only once");
    let (eligible, winners) = w.winners(tid).await;
    let signups: Vec<i64> = db::query_scalar(&format!("SELECT s.uid FROM gonof_forum_lottery_signup s WHERE s.tid={tid} AND NOT EXISTS(SELECT 1 FROM gonof_forum_lottery_exclusion x WHERE x.tid=s.tid AND x.uid=s.uid) ORDER BY s.uid"))
        .fetch_all(db.executor()).await.unwrap();
    assert_eq!(
        eligible, signups,
        "the list equals all entries that were not removed"
    );
    let excluded: Vec<i64> = db::query_scalar(&format!(
        "SELECT uid FROM gonof_forum_lottery_exclusion WHERE tid={tid}"
    ))
    .fetch_all(db.executor())
    .await
    .unwrap();
    assert!(excluded.iter().all(|uid| !eligible.contains(uid)));
    assert_eq!(winners.len(), eligible.len().min(12));
    let paid = winners.len().min(10) as i64;
    assert_eq!(
        scalar(db, &format!("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE tid={tid} AND kind='activity_reward'")).await,
        paid
    );
    assert_eq!(w.frozen(2).await, 0);
    assert_eq!(w.available(2).await, 1000 - 3 * paid);
    w.fixture.cleanup().await.unwrap();
}

/// An in-process log buffer installed as the global subscriber, collecting logs of every level in this test process.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn captured_logs() -> Captured {
    static LOGS: OnceLock<Captured> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Captured::default();
        let sink = logs.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .init();
        logs
    })
    .clone()
}

/// Codes are only for the author and winners: the author can correct an issued code and winners see the correction
/// time; logs, audit, mail queue and cards never hold the plaintext; after a key change `rekey` moves old ciphertext
/// to the new key.
#[tokio::test]
async fn mysql_codes_stay_private_can_be_corrected_and_survive_rekey() {
    let logs = captured_logs();
    let w = World::new(5).await;
    let db = w.db();
    let tid = w
        .publish(lottery(
            "reply",
            vec![prize("Membership", 2, "code", 0, &[SECRET, "SECOND-CODE"])],
        ))
        .await
        .tid;
    // Replacing the whole set before the draw: the count must match and no duplicates.
    assert!(
        message(
            activities::replace_codes(db, &w.ring, actor(2), tid, 1, &["ONLY".into()], NOW + 2)
                .await
                .unwrap_err()
        )
        .contains("The number of codes must equal the winners")
    );
    assert!(
        message(
            activities::replace_codes(
                db,
                &w.ring,
                actor(3),
                tid,
                1,
                &["A".into(), "B".into()],
                NOW + 2
            )
            .await
            .unwrap_err()
        )
        .contains("Only the lottery's author")
    );
    activities::replace_codes(
        db,
        &w.ring,
        actor(2),
        tid,
        1,
        &[SECRET.into(), "REPLACED-CODE".into()],
        NOW + 2,
    )
    .await
    .unwrap();
    for uid in [3, 4] {
        w.reply(uid, tid).await;
    }
    assert_eq!(
        activities::process_due(db, NOW + 3600, 20).await.unwrap(),
        1
    );
    let (_, winners) = w.winners(tid).await;
    let first = winners[0];
    let second = winners[1];
    assert_eq!(
        activities::my_prizes(db, &w.ring, first, 0)
            .await
            .unwrap()
            .0[0]
            .code
            .as_deref(),
        Some(SECRET)
    );
    assert_eq!(
        activities::my_prizes(db, &w.ring, second, 0)
            .await
            .unwrap()
            .0[0]
            .code
            .as_deref(),
        Some("REPLACED-CODE")
    );
    assert!(
        activities::my_prizes(db, &w.ring, 5, 0)
            .await
            .unwrap()
            .0
            .is_empty()
    );

    // Correcting one at a time after the draw: a wrong version or a duplicate of another code is refused.
    assert!(
        message(
            activities::correct_code(db, &w.ring, actor(2), tid, 1, 2, SECRET, 0, NOW + 3700)
                .await
                .unwrap_err()
        )
        .contains("Codes cannot repeat")
    );
    activities::correct_code(
        db,
        &w.ring,
        actor(2),
        tid,
        1,
        2,
        "FIXED-CODE",
        0,
        NOW + 3700,
    )
    .await
    .unwrap();
    assert!(
        message(
            activities::correct_code(db, &w.ring, actor(2), tid, 1, 2, "AGAIN", 0, NOW + 3701)
                .await
                .unwrap_err()
        )
        .contains("The code was just corrected")
    );
    let corrected = &activities::my_prizes(db, &w.ring, second, 0)
        .await
        .unwrap()
        .0[0];
    assert_eq!(
        (corrected.code.as_deref(), corrected.revised_at),
        (Some("FIXED-CODE"), NOW + 3700)
    );

    // The plaintext is nowhere else: logs, audit, mail queue, activity card.
    for uid in [None, Some(first), Some(2)] {
        let card = activities::card(db, tid, uid).await.unwrap().unwrap();
        assert!(!serde_json::to_string(&card).unwrap().contains(SECRET));
    }
    let audit: Vec<String> = db::query_scalar("SELECT data FROM gonof_common_log")
        .fetch_all(db.executor())
        .await
        .unwrap();
    assert!(audit.iter().any(|d| d.contains(&format!("\"tid\":{tid}"))));
    assert!(
        audit
            .iter()
            .all(|d| !d.contains(SECRET) && !d.contains("FIXED-CODE"))
    );
    // Query with bound parameters so the test's own SQL text does not write the plaintext into the log.
    let mailed: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE payload LIKE ?")
            .bind(format!("%{SECRET}%"))
            .fetch_one(db.executor())
            .await
            .unwrap();
    assert_eq!(mailed, 0);
    let captured = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(
        captured.contains("gonof_forum_lottery_award"),
        "the log did capture this test's SQL"
    );
    assert!(!captured.contains(SECRET) && !captured.contains("FIXED-CODE"));

    // Key change: the old key moves into previous_keys, and after rekey keeping only the new key still decrypts.
    let rotated = TwoFactorConfig {
        active_key: "ab".repeat(32),
        previous_keys: vec![w.ring.active_key.clone()],
    };
    let report = activities::rekey_lottery_codes(db, &rotated).await.unwrap();
    assert_eq!(
        (report.rekeyed, report.current, report.unreadable.len()),
        (2, 0, 0)
    );
    let again = activities::rekey_lottery_codes(db, &rotated).await.unwrap();
    assert_eq!((again.rekeyed, again.current), (0, 2));
    let only_new = TwoFactorConfig {
        active_key: rotated.active_key.clone(),
        previous_keys: vec![],
    };
    assert_eq!(
        activities::my_prizes(db, &only_new, first, 0)
            .await
            .unwrap()
            .0[0]
            .code
            .as_deref(),
        Some(SECRET)
    );
    // With the key lost it cannot be decrypted; the page shows "This code cannot be decrypted right now" without an error.
    let lost = TwoFactorConfig {
        active_key: "cd".repeat(32),
        previous_keys: vec![],
    };
    let unreadable = &activities::my_prizes(db, &lost, first, 0).await.unwrap().0[0];
    assert!(unreadable.code.is_none() && unreadable.unreadable);
    // Ciphertext moved to another slot cannot be decrypted (the AAD contains tid, prize number and slot number).
    let first_code: Option<String> = db::query_scalar(
        "SELECT code_ciphertext FROM gonof_forum_lottery_award WHERE tid=? AND slot_no=1",
    )
    .bind(tid)
    .fetch_one(db.executor())
    .await
    .unwrap();
    db::query("UPDATE gonof_forum_lottery_award SET code_ciphertext=? WHERE tid=? AND slot_no=2")
        .bind(first_code)
        .bind(tid)
        .execute(db.executor())
        .await
        .unwrap();
    assert!(
        activities::my_prizes(db, &only_new, second, 0)
            .await
            .unwrap()
            .0[0]
            .unreadable
    );
    w.fixture.cleanup().await.unwrap();
}

/// The sign-up list page: the author and administrators can see it, others cannot; only the author can remove entries
/// before the draw.
#[tokio::test]
async fn mysql_review_page_is_for_the_author_and_admins() {
    let w = World::new(5).await;
    let db = w.db();
    let mut spec = lottery("reply", vec![prize("Coins", 1, "credit", 5, &[])]);
    spec.review = true;
    let tid = w.publish(spec).await.tid;
    let pid = w.reply(3, tid).await.pid;
    w.reply(4, tid).await;
    activities::exclude(db, actor(2), tid, 4, "sockpuppet", NOW + 2)
        .await
        .unwrap();
    let page = activities::review_page(db, 2, false, tid, 0, NOW + 3)
        .await
        .unwrap()
        .unwrap();
    assert!(page.can_edit);
    assert_eq!((page.entries_total, page.excluded_total), (2, 1));
    assert_eq!(page.entries[0].source_pid, Some(pid));
    assert_eq!(page.entries[1].excluded.as_deref(), Some("sockpuppet"));
    let admin = activities::review_page(db, 1, true, tid, 0, NOW + 3)
        .await
        .unwrap()
        .unwrap();
    assert!(!admin.can_edit);
    assert!(
        activities::review_page(db, 3, false, tid, 0, NOW + 3)
            .await
            .unwrap()
            .is_none()
    );
    // A lottery without review enabled cannot remove entries.
    let plain = w
        .publish(lottery("reply", vec![prize("Coins", 1, "credit", 5, &[])]))
        .await
        .tid;
    w.reply(3, plain).await;
    assert!(
        message(
            activities::exclude(db, actor(2), plain, 3, "reason", NOW + 2)
                .await
                .unwrap_err()
        )
        .contains("does not use author review")
    );
    assert!(
        message(
            activities::draw_now(db, actor(2), plain, NOW + 2)
                .await
                .unwrap_err()
        )
        .contains("cannot be drawn early")
    );
    w.fixture.cleanup().await.unwrap();
}
