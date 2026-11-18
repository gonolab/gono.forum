//! Thread downvotes, thread tips and following members.
//!
//! Writes all go through web form submissions; only scenarios that cannot be clicked through on the web, such as
//! "unverified email" and "concurrency", go through the data layer.
//! Every rule has an assertion that fails once the corresponding source is removed.
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
    activities, content,
    content::writes::{ContentWriteError, ContentWriter, NewReply, NewThread, WriteConfig},
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::{self, social},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: i64 = 9700;
const AUTHOR: i64 = 9701;
const FAN: i64 = 9702;
const OTHER: i64 = 9703;
const UNVERIFIED: i64 = 9704;
const POOR: i64 = 9705;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .map_or("", |value| value.to_str().unwrap())
    }
}

struct Fixture {
    services: TestFixture,
    app: Router,
    writer: ContentWriter,
    admin: SessionGrant,
    author: SessionGrant,
    fan: SessionGrant,
    other: SessionGrant,
    daily: i64,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        for (uid, adminid, verified) in [
            (ADMIN, 1, 1),
            (AUTHOR, 0, 1),
            (FAN, 0, 1),
            (OTHER, 0, 1),
            (UNVERIFIED, 0, 0),
            (POOR, 0, 1),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,?,?,1)")
                .bind(uid).bind(format!("plus-{uid}")).bind(format!("plus-{uid}@example.test")).bind(verified).bind(adminid)
                .execute(db.executor()).await.unwrap();
        }
        let config = services.config.clone();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin = http_auth::login(&identity, &db, ADMIN).await;
        let author = http_auth::login(&identity, &db, AUTHOR).await;
        let fan = http_auth::login(&identity, &db, FAN).await;
        let other = http_auth::login(&identity, &db, OTHER).await;
        let app = router::build(config, &services.root, db.clone());
        let daily = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(db.executor())
            .await
            .unwrap();
        Self {
            writer: ContentWriter::new(db.clone(), WriteConfig::default()),
            services,
            app,
            admin,
            author,
            fan,
            other,
            daily,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    /// `uid` posts a thread in the everyday forum; returns (tid, first post pid).
    async fn thread(&self, uid: i64, title: &str) -> (i64, i64) {
        let published = self
            .writer
            .publish(
                uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid: self.daily,
                    title: title.into(),
                    body: format!("Body of {title}"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
        (published.tid, published.pid)
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.47:48322".parse::<SocketAddr>().unwrap(),
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

    async fn page(&self, path: &str, session: Option<&SessionGrant>) -> String {
        let reply = self.get(path, session).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
        reply.body
    }

    async fn post_as(
        &self,
        session: &SessionGrant,
        path: &str,
        fields: &[(&str, String)],
        htmx: bool,
    ) -> Reply {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &session.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}", session.session_token),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
        if htmx {
            builder = builder.header("HX-Request", "true");
        }
        self.send(builder.body(Body::from(form.finish())).unwrap())
            .await
    }

    async fn downvote(&self, session: &SessionGrant, pid: i64, desired: bool, key: &str) -> Reply {
        self.post_as(
            session,
            &format!("/api/comments/{pid}/downvote"),
            &[
                ("request_key", key.into()),
                ("desired", desired.to_string()),
                ("location", String::new()),
            ],
            false,
        )
        .await
    }

    async fn like(&self, session: &SessionGrant, pid: i64, desired: bool) -> Reply {
        self.post_as(
            session,
            &format!("/api/comments/{pid}/vote"),
            &[
                ("request_key", identity::random_token()),
                ("desired", desired.to_string()),
                ("location", String::new()),
            ],
            false,
        )
        .await
    }

    async fn tip(&self, session: &SessionGrant, tid: i64, amount: i64, key: &str) -> Reply {
        self.post_as(
            session,
            &format!("/api/thread-{tid}/tip"),
            &[("request_key", key.into()), ("amount", amount.to_string())],
            false,
        )
        .await
    }

    async fn follow(&self, session: &SessionGrant, uid: i64, desired: bool, key: &str) -> Reply {
        self.post_as(
            session,
            &format!("/api/users/{uid}/follow"),
            &[
                ("request_key", key.into()),
                ("desired", desired.to_string()),
                ("location", format!("/space/{uid}")),
            ],
            false,
        )
        .await
    }

    async fn block(&self, session: &SessionGrant, uid: i64, desired: bool) -> Reply {
        let reply = self
            .post_as(
                session,
                &format!("/api/users/{uid}/block"),
                &[
                    ("request_key", identity::random_token()),
                    ("desired", desired.to_string()),
                    ("location", format!("/space/{uid}")),
                ],
                false,
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        reply
    }

    async fn credit(&self, uid: i64, amount: i64) {
        activities::adjust(
            self.db(),
            actor(ADMIN),
            uid,
            amount,
            &identity::random_token(),
            "Test grant",
            identity::now(),
        )
        .await
        .unwrap();
    }

    async fn balance(&self, uid: i64) -> i64 {
        self.scalar(&format!(
            "SELECT COALESCE(MAX(available),0) FROM gonof_common_credit_account WHERE uid={uid}"
        ))
        .await
    }

    async fn enable_tips(&self, amounts: &str, daily_limit: i64) {
        let reply = self
            .post_as(
                &self.admin,
                "/manage/credits/tips",
                &[
                    ("enabled", "true".into()),
                    ("amounts", amounts.into()),
                    ("daily_limit", daily_limit.to_string()),
                ],
                false,
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    }

    async fn read_only(&self, on: bool) {
        db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey='settings.site.read_only'")
            .bind(if on { "true" } else { "false" })
            .execute(self.db().executor())
            .await
            .unwrap();
    }
}

fn downvotes(pid: i64, uid: i64) -> String {
    format!("SELECT COUNT(*) FROM gonof_forum_post_downvote WHERE pid={pid} AND uid={uid}")
}

/// One ledger row: uid, available delta, available after, kind, thread, post.
type LedgerRow = (i64, i64, i64, String, Option<i64>, Option<i64>);

fn likes(pid: i64, uid: i64) -> String {
    format!("SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid={pid} AND uid={uid}")
}

#[tokio::test]
async fn downvotes_are_idempotent_exclusive_with_likes_and_counted_only_for_the_author() {
    let f = Fixture::new().await;
    let db = f.db();
    let (tid, pid) = f.thread(AUTHOR, "Thread open to downvotes").await;

    let key = identity::random_token();
    let reply = f.downvote(&f.fan, pid, true, &key).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.location(), format!("/thread-{tid}#floor-1"));
    assert_eq!(f.scalar(&downvotes(pid, FAN)).await, 1);
    // Replaying the same key with the same content: 303 as usual, nothing extra written; same key with different
    // content is 409.
    assert_eq!(
        f.downvote(&f.fan, pid, true, &key).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(f.scalar(&downvotes(pid, FAN)).await, 1);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid={FAN} AND request_key='{key}' AND dateline>0"
        ))
        .await,
        1,
        "the request row has a dateline, so the 30-day cleanup can see it"
    );
    assert_eq!(
        f.downvote(&f.fan, pid, false, &key).await.status,
        StatusCode::CONFLICT
    );

    // Counts are shown only to the author; others only see their own state; anonymous visitors see nothing.
    let fan_page = f.page(&format!("/thread-{tid}"), Some(&f.fan)).await;
    assert!(fan_page.contains("data-downvote=\"true\""), "{fan_page}");
    assert!(fan_page.contains("aria-label=\"Remove downvote\""));
    assert!(!fan_page.contains("data-downvote-count"));
    let author_page = f.page(&format!("/thread-{tid}"), Some(&f.author)).await;
    assert!(
        author_page.contains("data-downvote-count>1</span>"),
        "the author sees the downvote count"
    );
    let anonymous = f.page(&format!("/thread-{tid}"), None).await;
    assert!(!anonymous.contains(&format!("id=\"downvote-{pid}\"")));

    // A like cancels the member's own downvote, and a downvote cancels their own like.
    assert_eq!(
        f.like(&f.fan, pid, true).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(f.scalar(&likes(pid, FAN)).await, 1);
    assert_eq!(
        f.scalar(&downvotes(pid, FAN)).await,
        0,
        "a like removes the downvote"
    );
    let reply = f
        .post_as(
            &f.fan,
            &format!("/api/comments/{pid}/downvote"),
            &[
                ("request_key", identity::random_token()),
                ("desired", "true".into()),
                ("location", format!("/thread-{tid}")),
            ],
            true,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(f.scalar(&downvotes(pid, FAN)).await, 1);
    assert_eq!(
        f.scalar(&likes(pid, FAN)).await,
        0,
        "a downvote removes the like"
    );
    // HTMX swaps the downvote control and carries an out-of-band swap of the first post's like control.
    assert!(reply.body.contains(&format!("id=\"downvote-{pid}\"")));
    assert!(reply.body.contains(&format!("id=\"reaction-{pid}\"")));
    assert!(reply.body.contains("hx-swap-oob=\"true\""));

    // Only applies to threads: downvoting a reply is 404.
    let reply_pid = f
        .writer
        .reply(
            OTHER,
            NewReply {
                tid,
                root_pid: None,
                reply_to_pid: None,
                body: "A reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .pid;
    assert_eq!(
        f.downvote(&f.fan, reply_pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // You cannot downvote your own thread.
    assert_eq!(
        f.downvote(&f.author, pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    // An unverified email cannot downvote (there is no button on the web, so it goes through the data layer).
    let unverified = interaction::Interaction::new(db.clone())
        .set_downvote(actor(UNVERIFIED), pid, true, &identity::random_token())
        .await;
    assert!(
        matches!(unverified, Err(ContentWriteError::Forbidden(_))),
        "{unverified:?}"
    );
    // The author blocked the reader: no new downvote, but an existing downvote can still be cancelled.
    f.block(&f.author, OTHER, true).await;
    assert_eq!(
        f.downvote(&f.other, pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.scalar(&downvotes(pid, OTHER)).await, 0);
    f.block(&f.author, FAN, true).await;
    assert_eq!(
        f.downvote(&f.fan, pid, false, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(f.scalar(&downvotes(pid, FAN)).await, 0);
    f.block(&f.author, FAN, false).await;
    // Blocked in read-only mode.
    f.read_only(true).await;
    assert_eq!(
        f.downvote(&f.fan, pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    f.read_only(false).await;
    assert_eq!(f.scalar(&downvotes(pid, FAN)).await, 0);
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn downvotes_share_the_toggle_rate_limit_with_likes() {
    let f = Fixture::new().await;
    let (_, pid) = f.thread(AUTHOR, "Rate-limited thread").await;
    // 60 toggles per account per minute, shared by likes and downvotes.
    for index in 0..60 {
        let reply = if index % 2 == 0 {
            f.like(&f.fan, pid, index % 4 == 0).await
        } else {
            f.downvote(&f.fan, pid, index % 4 == 1, &identity::random_token())
                .await
        };
        assert_eq!(
            reply.status,
            StatusCode::SEE_OTHER,
            "toggle {index}: {}",
            reply.body
        );
    }
    assert_eq!(
        f.downvote(&f.fan, pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Other members are unaffected.
    assert_eq!(
        f.downvote(&f.other, pid, true, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn tips_move_credits_once_within_the_limits() {
    let f = Fixture::new().await;
    let (tid, pid) = f.thread(AUTHOR, "Thread worth a tip").await;
    let (second, _) = f.thread(AUTHOR, "Second thread").await;
    let (third, _) = f.thread(AUTHOR, "Third thread").await;
    f.credit(FAN, 30).await;
    f.credit(POOR, 3).await;

    // A new database seeds it as on; after the admin area turns it off there is no entry and
    // submissions are refused.
    let reply = f
        .post_as(
            &f.admin,
            "/manage/credits/tips",
            &[
                ("amounts", "1,5,10,20".into()),
                ("daily_limit", "100".into()),
            ],
            false,
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let page = f.page(&format!("/thread-{tid}"), Some(&f.fan)).await;
    assert!(!page.contains("data-tip-open"));
    let reply = f.tip(&f.fan, tid, 5, &identity::random_token()).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains("Tipping is not turned on"),
        "{}",
        reply.body
    );

    // Admin save: the cap cannot be below the largest tier, and tiers must be ascending.
    for (amounts, limit) in [("1,5,10,20", 10), ("5,1", 100)] {
        let reply = f
            .post_as(
                &f.admin,
                "/manage/credits/tips",
                &[
                    ("enabled", "true".into()),
                    ("amounts", amounts.into()),
                    ("daily_limit", limit.to_string()),
                ],
                false,
            )
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{amounts}");
    }
    f.enable_tips("1,5,10,20", 25).await;
    assert!(
        f.page("/manage/credits", Some(&f.admin))
            .await
            .contains("value=\"1,5,10,20\"")
    );
    let page = f.page(&format!("/thread-{tid}"), Some(&f.fan)).await;
    assert!(page.contains("data-tip-open"));
    assert!(page.contains("aria-label=\"Tip\""));
    assert!(page.contains("data-tip-action-count>0</span>"));
    assert!(page.contains("data-tip-amount=\"20\""));
    // There is no button on the author's own thread, and a submission is refused too.
    assert!(
        !f.page(&format!("/thread-{tid}"), Some(&f.author))
            .await
            .contains("data-tip-open")
    );
    let reply = f.tip(&f.author, tid, 5, &identity::random_token()).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("You cannot tip your own thread"));
    // An amount not among the tiers.
    let reply = f.tip(&f.fan, tid, 3, &identity::random_token()).await;
    assert!(
        reply.body.contains("Choose a valid tip amount"),
        "{}",
        reply.body
    );

    let key = identity::random_token();
    let reply = f.tip(&f.fan, tid, 20, &key).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.location(), format!("/thread-{tid}#floor-1"));
    assert_eq!((f.balance(FAN).await, f.balance(AUTHOR).await), (10, 20));
    // Ledger consistency: one out, one in, both carrying the thread and first post, and balances equal each one's
    // latest available_after.
    let rows = db::query("SELECT uid,available_delta,available_after,kind,tid,pid FROM gonof_common_credit_ledger WHERE kind IN ('tip_out','tip_in') ORDER BY id")
        .fetch_all(f.db().executor())
        .await
        .unwrap();
    let rows: Vec<LedgerRow> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("uid").unwrap(),
                row.try_get("available_delta").unwrap(),
                row.try_get("available_after").unwrap(),
                db::text(row, "kind").unwrap(),
                row.try_get("tid").unwrap(),
                row.try_get("pid").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (FAN, -20, 10, "tip_out".into(), Some(tid), Some(pid)),
            (AUTHOR, 20, 20, "tip_in".into(), Some(tid), Some(pid)),
        ]
    );
    // Replaying the same key with the same content records nothing extra; the same key with a different amount is
    // refused; tipping the same thread again with a new key is refused.
    assert_eq!(
        f.tip(&f.fan, tid, 20, &key).await.status,
        StatusCode::SEE_OTHER
    );
    let reply = f.tip(&f.fan, tid, 5, &key).await;
    assert!(
        reply
            .body
            .contains("This request ID was already used for another action"),
        "{}",
        reply.body
    );
    let reply = f.tip(&f.fan, tid, 5, &identity::random_token()).await;
    assert!(
        reply.body.contains("You have already tipped this thread"),
        "{}",
        reply.body
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE kind='tip_out'")
            .await,
        1
    );
    // Daily cap 25: 20 already spent, another 10 exceeds it, 5 fits exactly.
    let reply = f.tip(&f.fan, second, 10, &identity::random_token()).await;
    assert!(reply.body.contains("tipping limit"), "{}", reply.body);
    assert_eq!(
        f.tip(&f.fan, second, 5, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!((f.balance(FAN).await, f.balance(AUTHOR).await), (5, 25));
    // Insufficient balance: nothing of the whole transfer is written, and the request key is not stored either.
    let poor_key = identity::random_token();
    let reply = {
        let identity = Identity::new(
            f.db().clone(),
            f.services.config.identity.clone(),
            f.services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let session = http_auth::login(&identity, f.db(), POOR).await;
        f.tip(&session, tid, 5, &poor_key).await
    };
    assert!(
        reply.body.contains("Not enough Coins available"),
        "{}",
        reply.body
    );
    assert_eq!(f.balance(POOR).await, 3);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE uid={POOR} AND kind='tip_out'"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid={POOR} AND request_key='{poor_key}'"
        ))
        .await,
        0
    );
    // The summary and "Tipped" on the thread page; the wallet records the tip received.
    let page = f.page(&format!("/thread-{tid}"), Some(&f.fan)).await;
    assert!(
        page.contains("data-tip-count=\"1\" data-tip-total=\"20\""),
        "{page}"
    );
    assert!(page.contains("plus-9702"));
    assert!(page.contains("data-tipped"));
    assert!(page.contains("aria-label=\"Tipped\""));
    assert!(page.contains("data-tip-action-count>1</span>"));
    assert!(!page.contains("data-tip-open"));
    assert!(
        f.page(&format!("/thread-{tid}"), None)
            .await
            .contains("1 tip · 20 Coins in total")
    );
    assert!(
        f.page("/my/credits", Some(&f.author))
            .await
            .contains("Received a tip")
    );

    // Thread author frozen: no button, submissions refused, balances untouched.
    f.credit(OTHER, 50).await;
    db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=?")
        .bind(AUTHOR)
        .execute(f.db().executor())
        .await
        .unwrap();
    assert!(
        !f.page(&format!("/thread-{third}"), Some(&f.other))
            .await
            .contains("data-tip-open")
    );
    let reply = f.tip(&f.other, third, 5, &identity::random_token()).await;
    assert!(
        reply.body.contains("author cannot receive tips right now"),
        "{}",
        reply.body
    );
    assert_eq!(f.balance(OTHER).await, 50);
    db::query("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=?")
        .bind(AUTHOR)
        .execute(f.db().executor())
        .await
        .unwrap();
    // The thread author blocked the payer: cannot tip.
    f.block(&f.author, OTHER, true).await;
    let reply = f.tip(&f.other, third, 5, &identity::random_token()).await;
    assert!(
        reply
            .body
            .contains("This thread cannot be tipped right now"),
        "{}",
        reply.body
    );
    f.block(&f.author, OTHER, false).await;
    // Blocked in read-only mode.
    f.read_only(true).await;
    let reply = f.tip(&f.other, third, 5, &identity::random_token()).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    f.read_only(false).await;
    assert_eq!(
        f.tip(&f.other, third, 5, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!((f.balance(OTHER).await, f.balance(AUTHOR).await), (45, 30));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_tips_are_recorded_exactly_once() {
    let f = Fixture::new().await;
    let db = f.db().clone();
    let (tid, _) = f.thread(AUTHOR, "Concurrent tips").await;
    f.enable_tips("1,5,10,20", 100).await;
    for uid in [FAN, OTHER] {
        f.credit(uid, 100).await;
    }
    let now = identity::now();
    let shared = identity::random_token();
    let mut tasks = Vec::new();
    // The same member: 8 requests with the same key and 8 with different keys tip the same thread at once; another
    // member sends 8 requests with the same key.
    for index in 0..24 {
        let db = db.clone();
        let (uid, key) = match index {
            0..8 => (FAN, shared.clone()),
            8..16 => (FAN, identity::random_token()),
            _ => (OTHER, format!("{:0>64}", "abc")),
        };
        tasks.push(tokio::spawn(async move {
            activities::tip(&db, actor(uid), tid, 10, &key, now).await
        }));
    }
    let mut succeeded = 0;
    for task in tasks {
        if task.await.unwrap().is_ok() {
            succeeded += 1;
        }
    }
    assert!(
        succeeded >= 2,
        "each of the two members succeeds at least once (replays included)"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE tid={tid} AND kind='tip_out'"
        ))
        .await,
        2,
        "one ledger row per member"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE tid={tid} AND kind='tip_in'"
        ))
        .await,
        2
    );
    assert_eq!(
        (
            f.balance(FAN).await,
            f.balance(OTHER).await,
            f.balance(AUTHOR).await
        ),
        (90, 90, 20)
    );
    // The thread author's two incoming entries have available_after 10 and 20 in order, with no overlapping overwrite.
    let afters: Vec<i64> = db::query_scalar(&format!(
        "SELECT available_after FROM gonof_common_credit_ledger WHERE uid={AUTHOR} AND kind='tip_in' ORDER BY id"
    ))
    .fetch_all(db.executor())
    .await
    .unwrap();
    assert_eq!(afters, vec![10, 20]);
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_follows_respect_blocks_privacy_and_limits() {
    let f = Fixture::new().await;
    let db = f.db();
    let follows = |uid: i64, followed: i64| {
        format!(
            "SELECT COUNT(*) FROM gonof_home_member_follow WHERE uid={uid} AND followed_uid={followed}"
        )
    };
    // Following, replay and conflict.
    let key = identity::random_token();
    let reply = f.follow(&f.fan, AUTHOR, true, &key).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.location(), format!("/space/{AUTHOR}"));
    assert_eq!(
        f.follow(&f.fan, AUTHOR, true, &key).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.follow(&f.fan, AUTHOR, false, &key).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(f.scalar(&follows(FAN, AUTHOR)).await, 1);
    assert_eq!(
        f.follow(&f.fan, FAN, true, &identity::random_token())
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "cannot follow oneself"
    );
    // Profile page: the button and the two counts.
    let profile = f.page(&format!("/space/{AUTHOR}"), Some(&f.fan)).await;
    assert!(profile.contains(&format!("data-social-target=\"{AUTHOR}\">Unfollow")));
    assert!(
        profile.contains("data-stat=\"followers\">1</span>"),
        "{profile}"
    );
    assert!(
        f.page(&format!("/space/{FAN}"), None)
            .await
            .contains("data-stat=\"following\">1</span>")
    );
    // Only the member themselves can see the lists.
    assert!(
        f.page("/my/followers", Some(&f.author))
            .await
            .contains(&format!("data-follower=\"{FAN}\""))
    );
    assert!(
        f.page("/my/following/members", Some(&f.fan))
            .await
            .contains("plus-9701")
    );
    assert!(
        !f.page("/my/followers", Some(&f.fan))
            .await
            .contains("data-follower")
    );
    assert_eq!(
        f.get("/my/followers", None).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.get(&format!("/space/{AUTHOR}/followers"), Some(&f.fan))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "there is no public follower list"
    );

    // Follow feed: new threads from the last 30 days; older ones do not appear; when the space is set to private the
    // whole person disappears.
    let (fresh, _) = f.thread(AUTHOR, "New thread by a followed member").await;
    let (old, old_pid) = f.thread(AUTHOR, "Thread from long ago").await;
    db::query("UPDATE gonof_forum_post SET dateline=? WHERE pid=?")
        .bind(identity::now() - 31 * 86400)
        .bind(old_pid)
        .execute(db.executor())
        .await
        .unwrap();
    let (stranger, _) = f.thread(OTHER, "Thread by a member not followed").await;
    let feed = f.page("/my/following/feed", Some(&f.fan)).await;
    assert!(
        feed.contains(&format!("data-feed-thread=\"{fresh}\"")),
        "{feed}"
    );
    assert!(!feed.contains(&format!("data-feed-thread=\"{old}\"")));
    assert!(!feed.contains(&format!("data-feed-thread=\"{stranger}\"")));
    db::query("UPDATE gonof_common_member SET space_visibility='private' WHERE uid=?")
        .bind(AUTHOR)
        .execute(db.executor())
        .await
        .unwrap();
    assert!(
        !f.page("/my/following/feed", Some(&f.fan))
            .await
            .contains("data-feed-thread"),
        "a private space does not appear in the feed"
    );
    let gated = f.get(&format!("/space/{AUTHOR}"), Some(&f.fan)).await;
    assert_eq!(gated.status, StatusCode::FORBIDDEN);
    assert!(
        !gated.body.contains("data-stat=\"followers\""),
        "no numbers before the gate"
    );
    db::query("UPDATE gonof_common_member SET space_visibility='public' WHERE uid=?")
        .bind(AUTHOR)
        .execute(db.executor())
        .await
        .unwrap();
    assert_eq!(
        f.get("/my/following/feed?before=bad", Some(&f.fan))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );

    // Blocking: cancels follows in both directions; while blocked neither side can newly follow; unblocking does not
    // restore them.
    assert_eq!(
        f.follow(&f.author, FAN, true, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(f.scalar(&follows(AUTHOR, FAN)).await, 1);
    f.block(&f.author, FAN, true).await;
    assert_eq!(
        f.scalar(&follows(FAN, AUTHOR)).await,
        0,
        "the blocked member's follow of the blocker"
    );
    assert_eq!(
        f.scalar(&follows(AUTHOR, FAN)).await,
        0,
        "the blocker's follow of the blocked member"
    );
    let reply = f
        .follow(&f.fan, AUTHOR, true, &identity::random_token())
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.body.contains("Cannot follow for now"));
    assert!(
        f.page(&format!("/space/{AUTHOR}"), Some(&f.fan))
            .await
            .contains(&format!("data-social-notice=\"{AUTHOR}\""))
    );
    let reply = f
        .follow(&f.author, FAN, true, &identity::random_token())
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        reply
            .body
            .contains("You have blocked this member; unblock them first")
    );
    f.block(&f.author, FAN, false).await;
    assert_eq!(
        f.scalar(&follows(FAN, AUTHOR)).await,
        0,
        "unblocking does not restore follows"
    );
    assert_eq!(
        f.follow(&f.fan, AUTHOR, true, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );

    // Blocked in read-only mode (follower counts are public).
    f.read_only(true).await;
    assert_eq!(
        f.follow(&f.other, AUTHOR, true, &identity::random_token())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    f.read_only(false).await;
    // An unverified email cannot follow.
    let unverified = social::set_follow_member(
        db,
        actor(UNVERIFIED),
        AUTHOR,
        true,
        &identity::random_token(),
    )
    .await;
    assert!(matches!(unverified, Err(ContentWriteError::Forbidden(_))));

    // Cap of 500: seed 500 followed members first; the 501st is refused, and unfollowing still works.
    let mut values = Vec::new();
    for index in 0..social::MEMBER_FOLLOW_LIMIT {
        values.push(format!("({},'bulk-{index}',1)", 20_000 + index));
    }
    db::query(&format!(
        "INSERT INTO gonof_common_member(uid,username,regdate) VALUES{}",
        values.join(",")
    ))
    .execute(db.executor())
    .await
    .unwrap();
    db::query(&format!(
        "INSERT INTO gonof_home_member_follow(uid,followed_uid,dateline) SELECT {OTHER},uid,1 FROM gonof_common_member WHERE uid>=20000"
    ))
    .execute(db.executor())
    .await
    .unwrap();
    let reply = f
        .follow(&f.other, AUTHOR, true, &identity::random_token())
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("You can follow at most 500 members"));
    assert_eq!(
        f.follow(&f.other, 20_000, false, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.follow(&f.other, AUTHOR, true, &identity::random_token())
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    f.services.cleanup().await.unwrap();
}

/// During the cooling period of the author's account deletion or content erasure the thread page offers no tip: the
/// author's account is still active, but the tip transaction refuses every amount. Cancelling the request brings the
/// button back.
#[tokio::test]
async fn tips_are_not_offered_while_the_author_is_leaving() {
    use gono_forum_web::privacy::{self, Access, NewRequest};
    let f = Fixture::new().await;
    let (tid, _) = f.thread(AUTHOR, "Author about to leave").await;
    f.credit(FAN, 30).await;
    f.enable_tips("1,5,10,20", 25).await;
    let path = format!("/thread-{tid}");
    assert!(f.page(&path, Some(&f.fan)).await.contains("data-tip-open"));
    let author = Access::Session(f.author.member.actor());
    let request = privacy::submit(
        f.db(),
        &author,
        NewRequest {
            kind: "erase".into(),
            content_mode: "erase".into(),
            request_key: identity::random_token(),
            // The password `http_auth::login` sets.
            password: "Http-fixture-password-2026!".into(),
        },
        identity::now(),
    )
    .await
    .unwrap();
    assert!(!f.page(&path, Some(&f.fan)).await.contains("data-tip-open"));
    let reply = f.tip(&f.fan, tid, 5, &identity::random_token()).await;
    assert!(
        reply.body.contains("author cannot receive tips right now"),
        "{}",
        reply.body
    );
    privacy::cancel(f.db(), &author, request, identity::now())
        .await
        .unwrap();
    assert!(f.page(&path, Some(&f.fan)).await.contains("data-tip-open"));
    f.services.cleanup().await.unwrap();
}
