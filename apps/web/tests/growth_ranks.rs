//! Leaderboards and the growth group: growth points are booked in line with each post's
//! own state, replays and concurrency do not duplicate, hiding and withdrawing take points back and restoring gives
//! them back, admin adjustments and reconciliation; boards count only public content, are filtered by members'
//! current status when read, and snapshots refresh by interval; pages show levels and today's earnings.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities::{self, ActivitySpec},
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{
            ContentWriteError, ContentWriter, NewReply, NewThread, ThreadVisibility, WriteConfig,
        },
    },
    db::{self, Database},
    growth,
    i18n::Lang,
    identity::{self, Actor, Identity, SessionGrant},
    interaction::Interaction,
    ranks::{self, Board, Period},
    router,
    test_support::{self, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: i64 = 7301;
const A: i64 = 7302;
const B: i64 = 7303;
const C: i64 = 7304;
const D: i64 = 7305;
const E: i64 = 7306;
const VIEWER: i64 = 7307;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

struct Fixture {
    services: TestFixture,
    app: Router,
    fid: i64,
    now: i64,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        let now = identity::now();
        for (uid, adminid) in [
            (ADMIN, 1),
            (A, 0),
            (B, 0),
            (C, 0),
            (D, 0),
            (E, 0),
            (VIEWER, 0),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,?)")
                .bind(uid)
                .bind(format!("grower-{uid}"))
                .bind(format!("grower-{uid}@example.test"))
                .bind(adminid)
                .bind(now - 100 * 86_400)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let fid = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        // The fixture's cache handle, so a test can change a setting and clear it.
        let app = router::build_with_global(
            services.config.clone(),
            &services.root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            fid,
            now,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db().executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    async fn growth(&self, uid: i64) -> i64 {
        growth::growth(self.db(), uid).await.unwrap()
    }

    async fn ledger_rows(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM gonof_common_growth_ledger")
            .await
    }

    fn writer(&self) -> ContentWriter {
        ContentWriter::new(self.db().clone(), WriteConfig::default())
    }

    fn editor(&self) -> ContentEditor {
        ContentEditor::new(self.db().clone(), WriteConfig::default(), 86_400)
    }

    async fn thread_in(
        &self,
        uid: i64,
        fid: i64,
        title: &str,
        visibility: ThreadVisibility,
        activity: ActivitySpec,
        at: i64,
    ) -> (i64, i64) {
        let published = self
            .writer()
            .publish_at(
                uid,
                NewThread {
                    visibility,
                    activity,
                    fid,
                    title: title.into(),
                    body: format!("{title} body"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap();
        (published.tid, published.pid)
    }

    async fn thread(&self, uid: i64, title: &str) -> (i64, i64) {
        self.thread_in(
            uid,
            self.fid,
            title,
            ThreadVisibility::Public,
            ActivitySpec::default(),
            self.now,
        )
        .await
    }

    async fn reply_at(&self, uid: i64, tid: i64, at: i64) -> i64 {
        self.writer()
            .reply_at(
                uid,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: format!("reply-{uid}-{}", identity::random_token()),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap()
            .pid
    }

    async fn reply(&self, uid: i64, tid: i64) -> i64 {
        self.reply_at(uid, tid, self.now).await
    }

    async fn like(&self, uid: i64, pid: i64, desired: bool) -> Result<(), ContentWriteError> {
        Interaction::new(self.db().clone())
            .set_reaction(actor(uid), pid, desired, &identity::random_token())
            .await
    }

    /// Authors withdraw and restore without a reason; managers' hide, highlight and so on require one.
    async fn change(&self, uid: i64, pid: i64, action: ContentAction) {
        let editor = self.editor();
        let post = editor.editable_at(actor(uid), pid, self.now).await.unwrap();
        editor
            .change_at(
                actor(uid),
                ChangeRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: if uid == ADMIN {
                        "growth test".into()
                    } else {
                        String::new()
                    },
                    action,
                },
                self.now,
            )
            .await
            .unwrap();
    }

    async fn consistent(&self) {
        let result = growth::reconcile(self.db()).await.unwrap();
        assert!(result.is_consistent(), "{result:?}");
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.47:48323".parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn get(&self, path: &str, session: Option<&SessionGrant>) -> Reply {
        let mut builder = Request::builder().uri(path);
        if let Some(session) = session {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", session.session_token),
            );
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, session: &SessionGrant, fields: &[(&str, String)]) -> Reply {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &session.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", session.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
        )
        .await
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        let identity = Identity::new(
            self.db().clone(),
            self.services.config.identity.clone(),
            self.services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        http_auth::login(&identity, self.db(), uid).await
    }
}

/// Growth points follow each post's own state: posting, liking and unliking, highlight, hiding takes back, restoring
/// gives back, withdraw, accepted answer; request replays, repeated likes and repeated moderation never book an extra
/// entry; switching to members-only has no effect; admin adjustments are idempotent; reconciliation is consistent.
#[tokio::test]
async fn growth_follows_post_state_and_never_double_counts() {
    let f = Fixture::new().await;
    let (tid, first) = f.thread(A, "GROWTH thread").await;
    assert_eq!(f.growth(A).await, 3);
    let reply = f.reply(B, tid).await;
    assert_eq!(f.growth(B).await, 1);

    // Replaying the same request key and liking again after already liking: neither books extra.
    let key = identity::random_token();
    let interaction = Interaction::new(f.db().clone());
    interaction
        .set_reaction(actor(B), first, true, &key)
        .await
        .unwrap();
    assert_eq!(f.growth(A).await, 4);
    let rows = f.ledger_rows().await;
    interaction
        .set_reaction(actor(B), first, true, &key)
        .await
        .unwrap();
    f.like(B, first, true).await.unwrap();
    assert_eq!((f.growth(A).await, f.ledger_rows().await), (4, rows));
    f.like(C, first, true).await.unwrap();
    assert_eq!(f.growth(A).await, 5);
    // A downvote removes the member's own like, and the growth points from the like are taken back with
    // it; liking again removes the downvote and gives them back.
    interaction
        .set_downvote(actor(B), first, true, &identity::random_token())
        .await
        .unwrap();
    assert_eq!(f.growth(A).await, 4);
    f.like(B, first, true).await.unwrap();
    assert_eq!(f.growth(A).await, 5);
    assert!(matches!(
        f.like(A, first, true).await,
        Err(ContentWriteError::Forbidden(_))
    ));

    // Highlight +10; one hide takes everything back, repeated hides book nothing more; unliking while hidden books
    // nothing; unhiding gives back by the like count at that time.
    f.change(ADMIN, first, ContentAction::Feature).await;
    assert_eq!(f.growth(A).await, 15);
    f.change(ADMIN, first, ContentAction::Hide).await;
    assert_eq!(f.growth(A).await, 0);
    assert_eq!(
        f.scalar(&format!(
            "SELECT delta FROM gonof_common_growth_ledger WHERE pid={first} AND kind='revoke'"
        ))
        .await,
        -15
    );
    let rows = f.ledger_rows().await;
    f.change(ADMIN, first, ContentAction::Hide).await;
    f.like(B, first, false).await.unwrap();
    assert_eq!((f.growth(A).await, f.ledger_rows().await), (0, rows));
    f.change(ADMIN, first, ContentAction::Unhide).await;
    assert_eq!(f.growth(A).await, 3 + 1 + 10);
    f.change(ADMIN, first, ContentAction::Unfeature).await;
    assert_eq!(f.growth(A).await, 4);

    // Withdraw takes back, restore gives back.
    f.change(B, reply, ContentAction::Withdraw).await;
    assert_eq!(f.growth(B).await, 0);
    f.change(B, reply, ContentAction::Restore).await;
    assert_eq!(f.growth(B).await, 1);

    // Only the post's own state matters: after the thread switches to members-only it still counts (unliking and liking
    // again are both still booked).
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET visibility='members' WHERE tid={tid}"
    ))
    .await;
    f.like(C, first, false).await.unwrap();
    assert_eq!(f.growth(A).await, 3);
    f.like(C, first, true).await.unwrap();
    assert_eq!(f.growth(A).await, 4);

    // Bounty accepted answer +5; hiding the accepted reply takes back the accepted-answer points too, and unhiding gives
    // them back.
    activities::adjust(
        f.db(),
        actor(ADMIN),
        A,
        100,
        &identity::random_token(),
        "bounty budget",
        f.now,
    )
    .await
    .unwrap();
    let (bounty, _) = f
        .thread_in(
            A,
            f.fid,
            "GROWTH bounty",
            ThreadVisibility::Public,
            ActivitySpec {
                kind: 1,
                budget: 10,
                deadline: f.now + 3600,
                ..Default::default()
            },
            f.now,
        )
        .await;
    assert_eq!(f.growth(A).await, 7);
    let answer = f.reply(D, bounty).await;
    activities::accept(
        f.db(),
        actor(A),
        bounty,
        answer,
        &identity::random_token(),
        f.now + 1,
    )
    .await
    .unwrap();
    assert_eq!(f.growth(D).await, 6);
    f.change(ADMIN, answer, ContentAction::Hide).await;
    assert_eq!(f.growth(D).await, 0);
    f.change(ADMIN, answer, ContentAction::Unhide).await;
    assert_eq!(f.growth(D).await, 6);

    // Concurrent likes on the same post: the growth row is locked before writing the like, so every like is booked,
    // exactly once.
    let likers: Vec<i64> = (0..8).map(|n| 7400 + n).collect();
    for uid in &likers {
        f.execute(&format!("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES({uid},'liker-{uid}','synthetic-hash','liker-{uid}@example.test',1,1)")).await;
    }
    let (_, popular) = f.thread(C, "GROWTH concurrency").await;
    let results =
        futures_util::future::join_all(likers.iter().map(|uid| f.like(*uid, popular, true))).await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(f.growth(C).await, 3 + 8);

    // Admin adjustment: replaying the same key does not duplicate; the same key with different content is rejected;
    // cannot go below 0.
    let key = identity::random_token();
    growth::adjust(f.db(), actor(ADMIN), B, 5, &key, "make-up award", f.now)
        .await
        .unwrap();
    growth::adjust(f.db(), actor(ADMIN), B, 5, &key, "make-up award", f.now)
        .await
        .unwrap();
    assert_eq!(f.growth(B).await, 6);
    assert!(
        growth::adjust(f.db(), actor(ADMIN), B, 6, &key, "make-up award", f.now)
            .await
            .is_err()
    );
    assert!(
        growth::adjust(
            f.db(),
            actor(ADMIN),
            B,
            -100,
            &identity::random_token(),
            "deduction",
            f.now
        )
        .await
        .is_err()
    );
    assert!(
        growth::adjust(
            f.db(),
            actor(B),
            C,
            1,
            &identity::random_token(),
            "not allowed",
            f.now
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='growth.adjust'")
            .await,
        1
    );
    f.consistent().await;

    // Reconciliation finds every per-post growth row that was tampered with.
    f.execute(&format!(
        "UPDATE gonof_common_growth_post SET credited=credited+1 WHERE pid={first}"
    ))
    .await;
    let broken = growth::reconcile(f.db()).await.unwrap();
    assert_eq!(broken.post_ledger, [first]);
    assert_eq!(broken.post_state, [first]);
    f.services.cleanup_ref().await.unwrap();
}

/// Boards count only public content (members-only threads, member forums, hidden and withdrawn posts do not count),
/// with periods by posting and like times; boards are filtered by members' current status when read (frozen, only me,
/// signed-in users only, blocked), with places shifted down; snapshots are not recomputed within the interval and
/// reflect new hides only after the interval.
#[tokio::test]
async fn ranks_count_public_content_and_filter_members_when_read() {
    let f = Fixture::new().await;
    f.execute("INSERT INTO gonof_forum_forum(slug,name,description,guidance,visibility) VALUES('club','Club','','','members')").await;
    let club = f
        .scalar("SELECT fid FROM gonof_forum_forum WHERE slug='club'")
        .await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='members' WHERE uid={C}"
    ))
    .await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='private' WHERE uid={D}"
    ))
    .await;
    f.execute(&format!(
        "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES({VIEWER},{A},1)"
    ))
    .await;

    let (host, host_pid) = f.thread(ADMIN, "RANK public thread").await;
    let (_, a1_pid) = f.thread(A, "RANK A1").await;
    f.thread(A, "RANK A2").await;
    let (hidden_thread, _) = f
        .thread_in(
            A,
            f.fid,
            "RANK members only",
            ThreadVisibility::Members,
            ActivitySpec::default(),
            f.now,
        )
        .await;
    f.thread_in(
        A,
        club,
        "RANK members forum",
        ThreadVisibility::Public,
        ActivitySpec::default(),
        f.now,
    )
    .await;
    let mut a_replies = Vec::new();
    for _ in 0..3 {
        a_replies.push(f.reply(A, host).await);
    }
    let members_only_reply = f.reply(A, hidden_thread).await;
    f.thread(B, "RANK B1").await;
    f.reply(B, host).await;
    let hidden = f.reply(B, host).await;
    f.change(ADMIN, hidden, ContentAction::Hide).await;
    let withdrawn = f.reply(B, host).await;
    f.change(B, withdrawn, ContentAction::Withdraw).await;
    f.reply(C, host).await;
    f.reply(D, host).await;
    f.reply(D, host).await;
    f.reply(E, host).await;
    // Threads and replies from 40 days ago go only into "all time".
    let old_at = f.now - 40 * 86_400;
    let (old, _) = f
        .thread_in(
            B,
            f.fid,
            "RANK old thread",
            ThreadVisibility::Public,
            ActivitySpec::default(),
            old_at,
        )
        .await;
    f.reply_at(B, old, old_at + 1).await;
    f.like(B, a1_pid, true).await.unwrap();
    f.like(C, a1_pid, true).await.unwrap();
    f.like(A, host_pid, true).await.unwrap();
    f.like(C, members_only_reply, true).await.unwrap();

    let now = identity::now();
    assert_eq!(ranks::refresh(f.db(), now, 0).await.unwrap(), 12);
    let replies_month = ranks::compute(f.db(), Board::Replies, Period::Month, now)
        .await
        .unwrap();
    assert_eq!(replies_month, [(A, 3), (D, 2), (B, 1), (C, 1), (E, 1)]);
    let replies_all = ranks::compute(f.db(), Board::Replies, Period::All, now)
        .await
        .unwrap();
    assert_eq!(replies_all, [(A, 3), (B, 2), (D, 2), (C, 1), (E, 1)]);
    let threads_month = ranks::compute(f.db(), Board::Threads, Period::Month, now)
        .await
        .unwrap();
    assert_eq!(threads_month, [(A, 2), (ADMIN, 1), (B, 1)]);
    let threads_all = ranks::compute(f.db(), Board::Threads, Period::All, now)
        .await
        .unwrap();
    assert_eq!(threads_all, [(A, 2), (B, 2), (ADMIN, 1)]);
    let likes_day = ranks::compute(f.db(), Board::Likes, Period::Day, now)
        .await
        .unwrap();
    assert_eq!(likes_day, [(A, 2), (ADMIN, 1)]);
    // The growth points board ranks by growth points: A has a members-only thread and posts in a member forum, which
    // still earn growth points (only the post's own state matters).
    let growth_all = ranks::compute(f.db(), Board::Growth, Period::All, now)
        .await
        .unwrap();
    // 4 threads, 4 replies, 3 likes (including the like on the reply in the members-only thread).
    assert_eq!(growth_all[0], (A, 4 * 3 + 4 + 3));
    // 24 hours by net history gain: B's old thread and old reply are outside the window, and posting points of hidden
    // and withdrawn posts have been taken back.
    assert!(growth_all.contains(&(B, 3 + 1 + 3 + 1)), "{growth_all:?}");
    let growth_day = ranks::compute(f.db(), Board::Growth, Period::Day, now)
        .await
        .unwrap();
    assert!(growth_day.contains(&(B, 3 + 1)), "{growth_day:?}");

    // Board filtering: anonymous visitors do not see signed-in-only and only-me members; a visitor who blocked A does
    // not see A; places shift down.
    let names = |snapshot: &ranks::Snapshot| -> Vec<(usize, i64)> {
        snapshot
            .standings
            .iter()
            .map(|standing| (standing.rank, standing.uid))
            .collect()
    };
    let anonymous = ranks::read(f.db(), Board::Replies, Period::Month, None)
        .await
        .unwrap();
    assert_eq!(names(&anonymous), [(1, A), (2, B), (3, E)]);
    assert_eq!(anonymous.computed_at, Some(now));
    let member = ranks::read(f.db(), Board::Replies, Period::Month, Some(B))
        .await
        .unwrap();
    assert_eq!(names(&member), [(1, A), (2, B), (3, C), (4, E)]);
    let blocker = ranks::read(f.db(), Board::Replies, Period::Month, Some(VIEWER))
        .await
        .unwrap();
    assert_eq!(names(&blocker), [(1, B), (2, C), (3, E)]);
    // Freezes take effect immediately, without waiting for a refresh.
    f.execute(&format!(
        "UPDATE gonof_common_member SET freeze=1 WHERE uid={E}"
    ))
    .await;
    let frozen = ranks::read(f.db(), Board::Replies, Period::Month, None)
        .await
        .unwrap();
    assert_eq!(names(&frozen), [(1, A), (2, B)]);

    // The snapshot is not recomputed within the interval; new hides are reflected only after the interval.
    f.change(ADMIN, a_replies[0], ContentAction::Hide).await;
    assert_eq!(ranks::refresh(f.db(), now + 10, 60).await.unwrap(), 0);
    let stale = ranks::read(f.db(), Board::Replies, Period::Month, None)
        .await
        .unwrap();
    assert_eq!(stale.standings[0].score, 3);
    assert_eq!(ranks::refresh(f.db(), now + 61, 60).await.unwrap(), 12);
    let fresh = ranks::read(f.db(), Board::Replies, Period::Month, None)
        .await
        .unwrap();
    assert_eq!((fresh.standings[0].uid, fresh.standings[0].score), (A, 2));
    assert_eq!(fresh.computed_at, Some(now + 61));

    // Pages: viewable anonymously, unknown boards 404, members with private spaces do not appear.
    let page = f.get("/ranks?board=replies&period=30d", None).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("grower-7302"));
    assert!(!page.body.contains("grower-7305"));
    assert!(page.body.contains("data-rank=\"1\""));
    assert_eq!(f.get("/ranks", None).await.status, StatusCode::OK);
    assert_eq!(
        f.get("/ranks?board=credits", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get("/ranks?period=7d", None).await.status,
        StatusCode::NOT_FOUND
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A board that comes out empty still records when it was computed: its page says nobody is on it rather than that it
/// is being built, and no process computes it again within the interval.
#[tokio::test]
async fn empty_boards_keep_their_computation_time() {
    let f = Fixture::new().await;
    let now = gono_forum_web::identity::now();
    let page = f.get("/ranks?board=likes&period=24h", None).await;
    assert!(
        page.body.contains("The leaderboard is being built"),
        "{}",
        page.body
    );
    assert_eq!(ranks::refresh(f.db(), now, 60).await.unwrap(), 12);
    let likes = ranks::read(f.db(), Board::Likes, Period::Day, None)
        .await
        .unwrap();
    assert!(likes.standings.is_empty());
    assert_eq!(likes.computed_at, Some(now));
    assert_eq!(ranks::refresh(f.db(), now + 10, 60).await.unwrap(), 0);
    let page = f.get("/ranks?board=likes&period=24h", None).await;
    assert!(
        page.body
            .contains("Nobody is on this board for this period yet."),
        "{}",
        page.body
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A refresh that started earlier but stores later gives up instead of overwriting the snapshot a later refresh
/// already stored (#17): the newer figures and time stay.
#[tokio::test]
async fn an_older_refresh_never_overwrites_a_newer_snapshot() {
    let f = Fixture::new().await;
    f.thread(A, "RANK older").await;
    let now = identity::now();
    let older = ranks::prepare(f.db(), Board::Threads, Period::All, now, 0)
        .await
        .unwrap()
        .expect("the board has never been computed");
    f.thread(B, "RANK newer").await;
    f.thread(B, "RANK newer again").await;
    assert!(
        ranks::refresh_board(f.db(), Board::Threads, Period::All, now + 5, 0)
            .await
            .unwrap()
    );
    assert!(!older.store(f.db()).await.unwrap());
    let board = ranks::read(f.db(), Board::Threads, Period::All, None)
        .await
        .unwrap();
    assert_eq!(board.computed_at, Some(now + 5));
    let places: Vec<(i64, i64)> = board
        .standings
        .iter()
        .map(|standing| (standing.uid, standing.score))
        .collect();
    assert_eq!(places, [(B, 2), (A, 1)]);
    // A snapshot time in the future that was there before the refresh read it (the clock went back) is replaced.
    let behind = ranks::prepare(f.db(), Board::Threads, Period::All, now, 30)
        .await
        .unwrap()
        .expect("a snapshot from the future is never fresh");
    assert!(behind.store(f.db()).await.unwrap());
    f.services.cleanup_ref().await.unwrap();
}

/// One failing board does not end the round, and a round with a failure is tried again at the next wake-up instead of
/// a full interval later (#182).
#[tokio::test]
async fn a_failed_board_neither_stops_the_round_nor_waits_an_interval() {
    let f = Fixture::new().await;
    f.thread(A, "RANK fault").await;
    let boards = || f.scalar("SELECT COUNT(*) FROM gonof_rank_board");
    test_support::fail_when(
        f.db(),
        "gonof_test_rank_fault",
        "BEFORE INSERT",
        "gonof_rank_board",
        "NEW.board = 'threads'",
        "injected leaderboard fault",
    )
    .await
    .unwrap();
    let schedule = ranks::Schedule::default();
    let now = identity::now();
    let error = schedule.tick(f.db(), 60, now).await.unwrap_err();
    assert!(error.to_string().contains("3 of 12"), "{error:#}");
    // Growth, replies and likes were refreshed although the threads boards came before them.
    assert_eq!(boards().await, 9);
    test_support::drop_trigger(f.db(), "gonof_test_rank_fault")
        .await
        .unwrap();
    // The next wake-up, well within the 60-second interval, refreshes the three missing boards only.
    schedule.tick(f.db(), 60, now + 10).await.unwrap();
    assert_eq!(boards().await, 12);
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_rank_board WHERE computed_at > 0 AND board = 'threads'"
        )
        .await,
        3
    );
    // A round that refreshed everything is recorded: the following wake-ups wait for the interval.
    f.thread(B, "RANK after").await;
    schedule.tick(f.db(), 60, now + 20).await.unwrap();
    let threads = ranks::read(f.db(), Board::Threads, Period::All, None)
        .await
        .unwrap();
    assert_eq!(threads.computed_at, Some(now + 10));
    f.services.cleanup_ref().await.unwrap();
}

/// Members nobody may see do not take the places of those after them (#160): with the 100 highest growth values held
/// by "Only me" profiles and the next 100 by "Logged-in members" profiles, anonymous visitors still see the public
/// member behind them, and signed-in members see the members-only profiles and then the public one.
#[tokio::test]
async fn hidden_profiles_do_not_crowd_out_the_members_behind_them() {
    let f = Fixture::new().await;
    let first = 20_000;
    let mut members = Vec::new();
    let mut accounts = Vec::new();
    for offset in 0..201 {
        let uid = first + offset;
        let space = match offset {
            0..100 => "private",
            100..200 => "members",
            _ => "public",
        };
        members.push(format!(
            "({uid},'crowd-{uid}','synthetic-hash','crowd-{uid}@example.test',1,0,{},'{space}')",
            f.now - 86_400
        ));
        accounts.push(format!("({uid},{})", 1_000 - offset));
    }
    f.execute(&format!(
        "INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate,space_visibility) VALUES {}",
        members.join(",")
    ))
    .await;
    f.execute(&format!(
        "INSERT INTO gonof_common_growth_account(uid,growth) VALUES {}",
        accounts.join(",")
    ))
    .await;
    let now = identity::now();
    assert!(
        ranks::refresh_board(f.db(), Board::Growth, Period::All, now, 0)
            .await
            .unwrap()
    );
    let uids = |snapshot: &ranks::Snapshot| -> Vec<i64> {
        snapshot
            .standings
            .iter()
            .map(|standing| standing.uid)
            .collect()
    };
    let anonymous = ranks::read(f.db(), Board::Growth, Period::All, None)
        .await
        .unwrap();
    assert_eq!(uids(&anonymous), [first + 200]);
    assert_eq!(anonymous.standings[0].rank, 1);
    let signed_in = ranks::read(f.db(), Board::Growth, Period::All, Some(VIEWER))
        .await
        .unwrap();
    let expected: Vec<i64> = (first + 100..=first + 200).collect();
    assert_eq!(uids(&signed_in), expected);
    // Nobody may see the "Only me" profiles, so the snapshot does not keep them.
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_rank_snapshot WHERE board='growth' AND period='all' AND uid < {}",
            first + 100
        ))
        .await,
        0
    );
    f.services.cleanup_ref().await.unwrap();
}

/// The `members.rank` data source does not keep showing a member who made their profile private until the cached
/// copy's segment ends (#7): the copy is keyed by the content revision the privacy change raised.
#[tokio::test]
async fn data_source_copies_drop_a_member_who_turned_private() {
    let f = Fixture::new().await;
    if !f.services.cache.enabled() {
        return f.services.cleanup_ref().await.unwrap();
    }
    f.thread(A, "RANK cached").await;
    ranks::refresh(f.db(), identity::now(), 0).await.unwrap();
    let path = "/data/v1/members.rank?board=threads&period=all";
    let before = f.get(path, None).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    assert!(before.body.contains("grower-7302"), "{}", before.body);
    let session = f.login(A).await;
    let identity = Identity::new(
        f.db().clone(),
        f.services.config.identity.clone(),
        f.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    identity
        .update_space_visibility(&session.session_token, "private")
        .await
        .unwrap();
    let after = f.get(path, None).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert!(!after.body.contains("grower-7302"), "{}", after.body);
    f.services.cleanup_ref().await.unwrap();
}

/// Pages: "My credits" shows growth points, level and today's earnings; growth history; admin adjustment
/// (administrators only); the level reaches 2 at 20; the profile page and thread floors show levels; credits rules are
/// readable anonymously; "Only me" spaces do not reveal growth points.
#[tokio::test]
async fn pages_show_levels_progress_and_today_gains() {
    let f = Fixture::new().await;
    let author = f.login(A).await;
    let admin = f.login(ADMIN).await;
    let other = f.login(B).await;
    let published = f
        .post(
            "/new",
            &author,
            &[
                ("request_key", identity::random_token()),
                ("fid", f.fid.to_string()),
                ("title", "PAGES growth thread".into()),
                ("body", "Body text".into()),
                ("category", "0".into()),
                ("visibility", "public".into()),
                ("attachment_ids", String::new()),
            ],
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );
    let tid = f
        .scalar("SELECT tid FROM gonof_forum_thread WHERE subject='PAGES growth thread'")
        .await;

    let credits = f.get("/my/credits", Some(&author)).await;
    assert_eq!(credits.status, StatusCode::OK, "{}", credits.body);
    assert!(credits.body.contains("data-growth>3<"), "{}", credits.body);
    assert!(credits.body.contains("One-Star Member"));
    assert!(
        credits.body.contains("<dd>Posted +3</dd>"),
        "{}",
        credits.body
    );
    let ledger = f.get("/my/credits/growth", Some(&author)).await;
    assert_eq!(ledger.status, StatusCode::OK);
    assert!(ledger.body.contains("data-growth-kind=\"publish\""));

    // Only administrators can adjust; 19 is still level 1, 20 reaches level 2.
    let denied = f
        .post(
            "/manage/credits/growth",
            &other,
            &[
                ("request_key", identity::random_token()),
                ("uid", A.to_string()),
                ("amount", "16".into()),
                ("reason", "not allowed".into()),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    for (amount, level, name) in [(16, 1, "One-Star Member"), (1, 2, "Two-Star Member")] {
        let adjusted = f
            .post(
                "/manage/credits/growth",
                &admin,
                &[
                    ("request_key", identity::random_token()),
                    ("uid", A.to_string()),
                    ("amount", amount.to_string()),
                    ("reason", "level boundary test".into()),
                ],
            )
            .await;
        assert_eq!(adjusted.status, StatusCode::SEE_OTHER, "{}", adjusted.body);
        assert_eq!(
            adjusted.headers[header::LOCATION],
            "/manage/credits?done=growth"
        );
        let profile = f.get(&format!("/space/{A}"), Some(&other)).await;
        assert_eq!(profile.status, StatusCode::OK);
        assert!(
            profile.body.contains(&format!("data-level=\"{level}\"")),
            "{}",
            profile.body
        );
        assert!(profile.body.contains(name));
    }
    assert_eq!(f.growth(A).await, 20);
    let profile = f.get(&format!("/space/{A}"), None).await;
    assert!(profile.body.contains("data-stat=\"growth\">20<"));
    assert!(
        profile.body.contains("60 growth to Lv3 Three-Star Member"),
        "{}",
        profile.body
    );
    let topic = f.get(&format!("/thread-{tid}"), None).await;
    assert_eq!(topic.status, StatusCode::OK);
    assert!(topic.body.contains("class=\"tag tag--level\""));
    assert!(topic.body.contains("data-level=\"2\">"));

    // Level thresholds are written the way the language writes numbers: grouped in English.
    let rules = f.get("/credits/rules", None).await;
    assert_eq!(rules.status, StatusCode::OK);
    for (name, floor) in [
        ("Two-Star Member", "20"),
        ("Six-Star Member", "1,200"),
        ("Eight-Star Member", "8,000"),
    ] {
        assert!(
            rules
                .body
                .contains(&format!("<td>{name}</td><td>{floor}</td>")),
            "{name}: {}",
            rules.body
        );
    }
    assert!(rules.body.contains("<td>Off</td>"), "{}", rules.body);

    // "Only me" spaces: others get 403, and the page outputs no growth points or level.
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='private' WHERE uid={A}"
    ))
    .await;
    let private = f.get(&format!("/space/{A}"), Some(&other)).await;
    assert_eq!(private.status, StatusCode::FORBIDDEN);
    assert!(!private.body.contains("data-growth-progress"));

    // A zh-CN site writes the same thresholds without grouping (`meta-number-grouping = never`).
    let zh = Lang::parse("zh-CN").unwrap();
    test_support::set_setting(f.db(), "settings.site.language", "zh-CN")
        .await
        .unwrap();
    f.services.global.clear_local();
    let rules = f.get("/credits/rules", None).await;
    assert_eq!(rules.status, StatusCode::OK);
    for (name, floor) in [("Six-Star Member", 1200), ("Eight-Star Member", 8000)] {
        assert!(
            rules
                .body
                .contains(&format!("<td>{name}</td><td>{}</td>", zh.number(floor))),
            "{name}: {}",
            rules.body
        );
    }
    assert!(!rules.body.contains("8,000"), "{}", rules.body);
    f.services.cleanup_ref().await.unwrap();
}
