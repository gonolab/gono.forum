//! The per-member leaderboard totals (`gonof_rank_member`, `ranks::counts`, #189) follow every write path that changes
//! what the all-time threads, replies and likes boards count. After each step the table is compared with a count made
//! from the posts and like rows (both backends), and on MySQL the content consistency check runs too. A MySQL case
//! has many members write at once, so deadlocks between their transactions show up as failures unless retried.
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use gono_forum_web::{
    content::{
        edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
        writes::{
            ContentWriteError, ContentWriter, NewReply, NewThread, Published, ThreadVisibility,
            WriteConfig,
        },
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::Interaction,
    media::Media,
    organization::{self, ForumInput},
    privacy::{self, Access, NewRequest},
    ranks::{self, Board, Period},
    test_support::{self, TestFixture},
};

const PASSWORD: &str = "Rank-counts-password-2026!";

/// (threads, replies, likes) by member.
type Totals = BTreeMap<i64, (i64, i64, i64)>;

struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
    media: Media,
    writer: ContentWriter,
    editor: ContentEditor,
    admin: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        test_support::raise_posting_quotas(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            test_support::detached_global(),
        );
        let admin_password = test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        Self {
            media: Media::new(
                db.clone(),
                services.config.media.clone(),
                services.root.clone(),
            ),
            writer: ContentWriter::new(db.clone(), WriteConfig::default()),
            editor: ContentEditor::new(
                db.clone(),
                services.config.content.clone(),
                services.config.media.orphan_seconds,
            ),
            services,
            db,
            identity,
            admin,
        }
    }

    fn admin(&self) -> Actor {
        self.admin.member.actor()
    }

    /// A member registered by invitation with a verified e-mail, registered long ago so no new-member quota applies.
    async fn member(&self, username: &str) -> SessionGrant {
        let invite = self
            .identity
            .generate_invite(self.admin.member.uid, &identity::random_token())
            .await
            .unwrap();
        let member = self
            .identity
            .register(
                username,
                &format!("{username}@example.test"),
                PASSWORD,
                &invite.code,
            )
            .await
            .unwrap();
        self.execute(&format!(
            "UPDATE gonof_common_member SET emailstatus=1,regdate=1 WHERE uid={}",
            member.member.uid
        ))
        .await;
        member
    }

    async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db.executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    async fn forum(&self, slug: &str, visibility: &str, parent: i64) -> i64 {
        organization::save_forum(
            &self.db,
            self.admin(),
            &ForumInput {
                name: slug.into(),
                slug: slug.into(),
                visibility: visibility.into(),
                status: 1,
                parent_fid: parent,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    /// Reads the board's current values, changes some and saves through the admin entry point.
    async fn edit_forum(&self, fid: i64, change: impl FnOnce(&mut ForumInput)) {
        let row = db::query("SELECT name,slug,description,guidance,displayorder,status,visibility,parent_fid,icon_aid,version,color,glyph FROM gonof_forum_forum WHERE fid=?")
            .bind(fid)
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        let mut input = ForumInput {
            fid,
            parent_fid: row
                .try_get::<Option<i64>, _>("parent_fid")
                .unwrap()
                .unwrap_or(0),
            icon_aid: row
                .try_get::<Option<i64>, _>("icon_aid")
                .unwrap()
                .unwrap_or(0),
            name: db::text(&row, "name").unwrap(),
            slug: db::text(&row, "slug").unwrap(),
            description: db::text(&row, "description").unwrap(),
            guidance: db::text(&row, "guidance").unwrap(),
            displayorder: row.try_get("displayorder").unwrap(),
            status: row.try_get("status").unwrap(),
            visibility: db::text(&row, "visibility").unwrap(),
            version: row.try_get("version").unwrap(),
            color: db::text(&row, "color").unwrap(),
            glyph: db::text(&row, "glyph").unwrap(),
        };
        change(&mut input);
        organization::save_forum(&self.db, self.admin(), &input)
            .await
            .unwrap();
    }

    async fn publish(&self, author: &SessionGrant, fid: i64, title: &str) -> Published {
        self.publish_as(author.member.uid, fid, title).await
    }

    async fn publish_as(&self, uid: i64, fid: i64, title: &str) -> Published {
        self.writer
            .publish(
                uid,
                NewThread {
                    visibility: ThreadVisibility::Public,
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: format!("{title} body"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap()
    }

    async fn reply_to(&self, uid: i64, tid: i64, root: Option<i64>) -> Published {
        self.writer
            .reply(
                uid,
                NewReply {
                    tid,
                    root_pid: root,
                    reply_to_pid: root,
                    body: format!("reply by {uid} {}", identity::random_token()),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap()
    }

    async fn change(&self, actor: Actor, pid: i64, action: ContentAction) {
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
                    reason: if actor.uid == self.admin.member.uid {
                        "Rank counts moderation".into()
                    } else {
                        String::new()
                    },
                },
            )
            .await
            .unwrap();
    }

    async fn visibility(&self, actor: Actor, pid: i64, visibility: ThreadVisibility) {
        let post = self.editor.editable(actor, pid).await.unwrap();
        self.editor
            .edit(
                actor,
                EditRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    title: None,
                    body: None,
                    request_key: identity::random_token(),
                    reason: String::new(),
                    visibility: Some(visibility),
                },
            )
            .await
            .unwrap();
    }

    async fn like(&self, actor: Actor, pid: i64, desired: bool) {
        Interaction::new(self.db.clone())
            .set_reaction(actor, pid, desired, &identity::random_token())
            .await
            .unwrap();
    }

    async fn move_to(&self, tid: i64, fid: i64) {
        let version: i64 = db::query_scalar("SELECT version FROM gonof_forum_thread WHERE tid=?")
            .bind(tid)
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        organization::move_thread(
            &self.db,
            self.admin(),
            tid,
            fid,
            version,
            &identity::random_token(),
            "Rank counts move",
        )
        .await
        .unwrap();
    }

    /// The totals as the boards define them, counted from the posts and the like rows.
    async fn expected(&self) -> Totals {
        let rows = db::query(
            "SELECT p.authorid AS uid,
                    CAST(SUM(CASE WHEN p.first = 1 THEN 1 ELSE 0 END) AS SIGNED) AS threads,
                    CAST(SUM(CASE WHEN p.first = 0 THEN 1 ELSE 0 END) AS SIGNED) AS replies,
                    CAST(SUM(COALESCE(l.likes, 0)) AS SIGNED) AS likes
               FROM gonof_forum_post p
               JOIN gonof_forum_thread t ON t.tid = p.tid
               JOIN gonof_forum_forum f ON f.fid = t.fid
               LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid
               LEFT JOIN (SELECT r.pid, COUNT(*) AS likes FROM gonof_forum_post_reaction r
                            JOIN gonof_forum_post rp ON rp.pid = r.pid
                           WHERE r.uid <> rp.authorid GROUP BY r.pid) l ON l.pid = p.pid
              WHERE p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0 AND p.erased_at = 0
                AND p.root_invisible = 0 AND t.displayorder >= 0 AND t.visibility = 'public'
                AND f.status = 1 AND f.visibility = 'public'
                AND (pf.fid IS NULL OR (pf.status = 1 AND pf.visibility = 'public'))
              GROUP BY p.authorid",
        )
        .fetch_all(self.db.executor())
        .await
        .unwrap();
        rows.iter()
            .map(|row| {
                (
                    row.try_get("uid").unwrap(),
                    (
                        row.try_get("threads").unwrap(),
                        row.try_get("replies").unwrap(),
                        row.try_get("likes").unwrap(),
                    ),
                )
            })
            .filter(|(_, totals)| *totals != (0, 0, 0))
            .collect()
    }

    async fn stored(&self) -> Totals {
        db::query("SELECT uid,threads,replies,likes FROM gonof_rank_member")
            .fetch_all(self.db.executor())
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.try_get("uid").unwrap(),
                    (
                        row.try_get("threads").unwrap(),
                        row.try_get("replies").unwrap(),
                        row.try_get("likes").unwrap(),
                    ),
                )
            })
            .filter(|(_, totals)| *totals != (0, 0, 0))
            .collect()
    }

    async fn totals(&self, member: &SessionGrant) -> (i64, i64, i64) {
        self.stored()
            .await
            .get(&member.member.uid)
            .copied()
            .unwrap_or_default()
    }

    /// The table agrees with the posts after `step`.
    async fn agree(&self, step: &str) {
        assert_eq!(self.stored().await, self.expected().await, "after {step}");
        let mut connection = self.db.pool.acquire().await.unwrap();
        let drift = test_support::content_drift(&mut *connection).await.unwrap();
        let bad: Vec<_> = drift.iter().filter(|(_, amount)| *amount != 0).collect();
        assert!(bad.is_empty(), "after {step}: {bad:?}");
    }
}

/// Posting, replying, liking, moderation (a reply, a floor with nested replies, a first post), withdrawal, thread
/// visibility, moves, board visibility, enabled state and parent, and erasure all keep the totals equal to what the
/// boards count; the all-time boards read them.
#[tokio::test]
async fn totals_follow_every_write_path() {
    let f = Fixture::new().await;
    let open = f.forum("rank-open", "public", 0).await;
    let club = f.forum("rank-club", "members", 0).await;
    let outer = f.forum("rank-outer", "public", 0).await;
    let inner = f.forum("rank-inner", "public", outer).await;
    let a = f.member("rank-author").await;
    let b = f.member("rank-replier").await;
    let c = f.member("rank-third").await;

    let thread = f.publish(&a, open, "Ranked thread").await;
    let first = f.reply_to(b.member.uid, thread.tid, None).await;
    let floor = f.reply_to(c.member.uid, thread.tid, None).await;
    let nested = f.reply_to(b.member.uid, thread.tid, Some(floor.pid)).await;
    f.publish(&b, club, "Members-only board").await;
    f.agree("posting").await;
    assert_eq!(f.totals(&a).await, (1, 0, 0));
    assert_eq!(f.totals(&b).await, (0, 2, 0));

    f.like(c.member.actor(), thread.pid, true).await;
    f.like(b.member.actor(), floor.pid, true).await;
    f.like(a.member.actor(), floor.pid, true).await;
    f.like(a.member.actor(), nested.pid, true).await;
    f.agree("likes").await;
    assert_eq!(f.totals(&a).await, (1, 0, 1));
    assert_eq!(f.totals(&c).await, (0, 1, 2));
    f.like(a.member.actor(), floor.pid, false).await;
    f.like(a.member.actor(), floor.pid, false).await;
    f.agree("unlike").await;
    assert_eq!(f.totals(&c).await, (0, 1, 1));

    // A top-level floor hidden takes its nested replies with it.
    f.change(f.admin(), floor.pid, ContentAction::Hide).await;
    f.agree("hiding a floor").await;
    assert_eq!(f.totals(&c).await, (0, 0, 0));
    assert_eq!(f.totals(&b).await, (0, 1, 0));
    f.change(f.admin(), floor.pid, ContentAction::Unhide).await;
    f.agree("unhiding a floor").await;
    f.change(f.admin(), nested.pid, ContentAction::Hide).await;
    f.agree("hiding a nested reply").await;
    f.change(f.admin(), nested.pid, ContentAction::Unhide).await;
    f.change(b.member.actor(), first.pid, ContentAction::Withdraw)
        .await;
    f.agree("withdrawing").await;
    assert_eq!(f.totals(&b).await, (0, 1, 1));
    f.change(b.member.actor(), first.pid, ContentAction::Restore)
        .await;
    f.agree("restoring").await;

    // Hiding the first post unlists the thread: every reply in it leaves the totals.
    f.change(f.admin(), thread.pid, ContentAction::Hide).await;
    f.agree("hiding a first post").await;
    assert_eq!(f.stored().await.get(&a.member.uid), None);
    assert_eq!(f.totals(&c).await, (0, 0, 0));
    f.change(f.admin(), thread.pid, ContentAction::Unhide).await;
    f.agree("unhiding a first post").await;
    assert_eq!(f.totals(&a).await, (1, 0, 1));

    f.visibility(a.member.actor(), thread.pid, ThreadVisibility::Members)
        .await;
    f.agree("members-only thread").await;
    assert_eq!(f.totals(&c).await, (0, 0, 0));
    f.visibility(a.member.actor(), thread.pid, ThreadVisibility::Public)
        .await;
    f.agree("public thread").await;
    assert_eq!(f.totals(&c).await, (0, 1, 1));

    f.move_to(thread.tid, club).await;
    f.agree("moving to a members board").await;
    assert_eq!(f.totals(&a).await, (0, 0, 0));
    f.move_to(thread.tid, inner).await;
    f.agree("moving to a public child board").await;
    assert_eq!(f.totals(&a).await, (1, 0, 1));

    // The parent's visibility and enabled state are part of the child's.
    f.edit_forum(outer, |input| input.visibility = "members".into())
        .await;
    f.agree("a members-only parent").await;
    assert_eq!(f.totals(&a).await, (0, 0, 0));
    f.edit_forum(outer, |input| input.visibility = "public".into())
        .await;
    f.agree("a public parent").await;
    f.edit_forum(outer, |input| input.status = 0).await;
    f.agree("a disabled parent").await;
    assert_eq!(f.totals(&a).await, (0, 0, 0));
    f.edit_forum(outer, |input| input.status = 1).await;
    f.edit_forum(inner, |input| input.status = 0).await;
    f.agree("a disabled board").await;
    f.edit_forum(inner, |input| input.status = 1).await;
    f.agree("an enabled board").await;
    assert_eq!(f.totals(&a).await, (1, 0, 1));
    // Moving the board under the members-only one, then back under a public parent.
    f.edit_forum(inner, |input| input.parent_fid = club).await;
    f.agree("a board moved under a members board").await;
    assert_eq!(f.totals(&a).await, (0, 0, 0));
    f.edit_forum(inner, |input| input.parent_fid = outer).await;
    f.agree("a board moved back").await;
    assert_eq!(f.totals(&a).await, (1, 0, 1));
    f.edit_forum(club, |input| input.visibility = "public".into())
        .await;
    f.agree("a members board made public").await;
    assert_eq!(f.totals(&b).await, (1, 2, 1));

    // The all-time boards read the totals.
    let now = identity::now();
    assert_eq!(
        ranks::compute(&f.db, Board::Replies, Period::All, now)
            .await
            .unwrap(),
        [(b.member.uid, 2), (c.member.uid, 1)]
    );
    assert_eq!(
        ranks::compute(&f.db, Board::Likes, Period::All, now)
            .await
            .unwrap(),
        [(a.member.uid, 1), (b.member.uid, 1), (c.member.uid, 1)]
    );

    // Erasure takes the member's posts out with the likes they had; the likes the member gave stay.
    privacy::submit(
        &f.db,
        &Access::Session(b.member.actor()),
        NewRequest {
            kind: "erase".into(),
            content_mode: "erase".into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        },
        now,
    )
    .await
    .unwrap();
    for round in 0..20 {
        privacy::process_pending(
            &f.db,
            &f.services.global,
            &f.media,
            &f.services.root,
            &f.services.config.identity.local_directory,
            90,
            now + privacy::COOLING_SECONDS + round,
        )
        .await
        .unwrap();
    }
    let left: i64 = db::query_scalar(&format!(
        "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={} AND erased_at=0",
        b.member.uid
    ))
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(left, 0, "the erasure finished");
    f.agree("erasure").await;
    assert_eq!(f.totals(&b).await, (0, 0, 0));
    assert_eq!(f.totals(&c).await, (0, 1, 1));
    f.services.cleanup().await.unwrap();
}

/// Runs `operation` again while it fails with a MySQL deadlock (1213) the write path's own three attempts did not
/// get past, at most 30 times, counting those in `exhausted`. A reply or like refused because the thread's first post
/// is hidden at that moment counts as done.
async fn persist<F, Fut>(exhausted: &AtomicUsize, mut operation: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), ContentWriteError>>,
{
    for _ in 0..30 {
        match operation().await {
            Ok(()) | Err(ContentWriteError::NotFound) => return Ok(()),
            Err(error) if error.to_string().contains("1213") => {
                exhausted.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("deadlocked 30 times in a row".into())
}

/// Many members reply and like across each other's threads while the administrator hides and unhides the threads'
/// first posts, which recounts whole threads (MySQL row locks). The write paths retry deadlocks themselves; their
/// totals agree with the posts afterwards. Deadlocks that outlast the three built-in attempts are retried here and
/// printed: they come from the growth-account upsert in likes (`growth::lock_accounts`, whose foreign-key check
/// share-locks the author's member row after the board lock), which the same workload hits with the totals switched
/// off.
#[tokio::test]
async fn concurrent_writes_across_members_keep_the_totals() {
    gono_forum_web::mysql_only!();
    let f = Fixture::new().await;
    let board = f.forum("rank-busy", "public", 0).await;
    // Members written directly: the administrator's daily invite codes would run out.
    let members: Vec<Actor> = (0..8)
        .map(|index| Actor {
            uid: 60_000 + index,
            credentials_version: 1,
            api_key: None,
        })
        .collect();
    for actor in &members {
        f.execute(&format!(
            "INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES({0},'rank-busy-{0}','synthetic-hash','rank-busy-{0}@example.test',1,1)",
            actor.uid
        ))
        .await;
    }
    let mut threads = Vec::new();
    for (index, actor) in members.iter().enumerate() {
        let thread = f
            .publish_as(actor.uid, board, &format!("Busy thread {index}"))
            .await;
        threads.push((thread.tid, thread.pid));
    }
    let exhausted = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for (index, actor) in members.iter().enumerate() {
        let actor = *actor;
        let writer = f.writer.clone();
        let db = f.db.clone();
        let threads = threads.clone();
        let exhausted = exhausted.clone();
        tasks.spawn(async move {
            for round in 0..4 {
                for (offset, (tid, pid)) in threads.iter().enumerate() {
                    if offset == index {
                        continue;
                    }
                    persist(&exhausted, || {
                        let writer = writer.clone();
                        let request = NewReply {
                            tid: *tid,
                            root_pid: None,
                            reply_to_pid: None,
                            body: format!("busy {round} {}", identity::random_token()),
                            attachment_ids: vec![],
                            request_key: identity::random_token(),
                        };
                        async move { writer.reply(actor.uid, request).await.map(drop) }
                    })
                    .await
                    .map_err(|error| format!("reply: {error}"))?;
                    persist(&exhausted, || {
                        let interaction = Interaction::new(db.clone());
                        async move {
                            interaction
                                .set_reaction(
                                    actor,
                                    *pid,
                                    round % 2 == 0,
                                    &identity::random_token(),
                                )
                                .await
                        }
                    })
                    .await
                    .map_err(|error| format!("like: {error}"))?;
                }
            }
            Ok::<(), String>(())
        });
    }
    {
        let editor = f.editor.clone();
        let db = f.db.clone();
        let admin = f.admin();
        let threads = threads.clone();
        let exhausted = exhausted.clone();
        tasks.spawn(async move {
            for round in 0..3 {
                for (tid, pid) in &threads {
                    for action in [ContentAction::Hide, ContentAction::Unhide] {
                        persist(&exhausted, || {
                            let (editor, db) = (editor.clone(), db.clone());
                            async move {
                                // The versions are read directly: the edit form's read (`editable`) does not retry.
                                let versions = db::query(
                                    "SELECT p.version AS post_version,t.version AS thread_version FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=? AND t.tid=?",
                                )
                                .bind(*pid)
                                .bind(*tid)
                                .fetch_one(db.executor())
                                .await
                                .unwrap();
                                editor
                                    .change(
                                        admin,
                                        ChangeRequest {
                                            pid: *pid,
                                            action,
                                            expected_post_version: versions
                                                .try_get("post_version")
                                                .unwrap(),
                                            expected_thread_version: versions
                                                .try_get("thread_version")
                                                .unwrap(),
                                            request_key: identity::random_token(),
                                            reason: format!("busy moderation {round}"),
                                        },
                                    )
                                    .await
                                    .map(drop)
                            }
                        })
                        .await
                        .map_err(|error| format!("moderation: {error}"))?;
                    }
                }
            }
            Ok(())
        });
    }
    while let Some(joined) = tasks.join_next().await {
        joined.unwrap().unwrap();
    }
    eprintln!(
        "deadlocks past the write paths' own retries: {}",
        exhausted.load(Ordering::Relaxed)
    );
    f.agree("concurrent writes").await;
    let replies: i64 = f.stored().await.values().map(|totals| totals.1).sum();
    assert!(replies > 0);
    f.services.cleanup().await.unwrap();
}

/// The all-time boards read their first places from the totals' `(total, uid)` indexes, without a sort, on both
/// backends: the two statements `ranks::top_by_index` runs for each (the backward read of the top, and the forward
/// read of the members tied at the last score).
#[tokio::test]
async fn all_time_boards_read_the_total_indexes() {
    let services = TestFixture::migrated().await.unwrap();
    let db = &services.db;
    let members: Vec<String> = (1..=2_000)
        .map(|uid| format!("({uid},'plan-{uid}','plan-{uid}@example.test',1)"))
        .collect();
    let totals: Vec<String> = (1..=2_000)
        .map(|uid| format!("({uid},{},{},{})", uid % 7, uid % 11, uid % 13))
        .collect();
    let growth: Vec<String> = (1..=2_000)
        .map(|uid| format!("({uid},{})", uid % 17))
        .collect();
    for sql in [
        format!(
            "INSERT INTO gonof_common_member(uid,username,email,regdate) VALUES {}",
            members.join(",")
        ),
        format!(
            "INSERT INTO gonof_rank_member(uid,threads,replies,likes) VALUES {}",
            totals.join(",")
        ),
        format!(
            "INSERT INTO gonof_common_growth_account(uid,growth) VALUES {}",
            growth.join(",")
        ),
    ] {
        db::query(&sql).execute(db.executor()).await.unwrap();
    }
    if db.backend() == db::Backend::MySql {
        for table in ["gonof_rank_member", "gonof_common_growth_account"] {
            db::query(&format!("ANALYZE TABLE {table}"))
                .execute(db.executor())
                .await
                .unwrap();
        }
    }
    for (table, column, index) in [
        ("gonof_rank_member", "threads", "gonof_rank_member_threads"),
        ("gonof_rank_member", "replies", "gonof_rank_member_replies"),
        ("gonof_rank_member", "likes", "gonof_rank_member_likes"),
        ("gonof_common_growth_account", "growth", "gonof_growth_rank"),
    ] {
        for sql in [
            format!(
                "SELECT uid, {column} AS score FROM {table} WHERE {column} > 0 ORDER BY {column} DESC, uid DESC LIMIT 400"
            ),
            format!(
                "SELECT uid, {column} AS score FROM {table} WHERE {column} = 5 ORDER BY uid LIMIT 400"
            ),
        ] {
            match db.backend() {
                db::Backend::MySql => {
                    let plan: String = db::query_scalar(&format!("EXPLAIN FORMAT=JSON {sql}"))
                        .fetch_one(db.executor())
                        .await
                        .unwrap();
                    let plan: String = plan.split_whitespace().collect();
                    assert!(
                        plan.contains(&format!("\"key\":\"{index}\"")),
                        "{sql}\n{plan}"
                    );
                    assert!(!plan.contains("\"using_filesort\":true"), "{sql}\n{plan}");
                }
                db::Backend::Sqlite => {
                    let plan = test_support::sqlite_plan(db, &sql).await.unwrap();
                    assert!(
                        plan.iter()
                            .any(|step| step.contains(&format!("INDEX {table}__{index}"))),
                        "{sql}\n{plan:?}"
                    );
                    assert!(
                        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                        "{sql}\n{plan:?}"
                    );
                }
            }
        }
    }
    // The tie at the last place read is broken by uid, as the aggregated boards do.
    let mut expected: Vec<(i64, i64)> = (1..=2_000_i64).map(|uid| (uid, uid % 11)).collect();
    expected.sort_by(|(a_uid, a), (b_uid, b)| b.cmp(a).then(a_uid.cmp(b_uid)));
    expected.truncate(100);
    assert_eq!(
        ranks::compute(db, Board::Replies, Period::All, identity::now())
            .await
            .unwrap(),
        expected
    );
    services.cleanup().await.unwrap();
}
