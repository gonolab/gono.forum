//! Content restoration requests: a member files a request for their own post
//! that was hidden by moderation, or withdrawn and past the author's time window, and staff with permission for the
//! forum accept or reject it.
//!
//! Eligibility, deduplication, cooldown, limits, permission scope and recusal are all decided on the server; accepting
//! goes through the existing unhide / restore-withdrawal in the same transaction. The rule that authors restore their
//! own withdrawals within the time window is unchanged.
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
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor},
        writes::{ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Identity, SessionGrant},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: i64 = 9901;
const GLOBAL: i64 = 9902;
const MOD: i64 = 9903;
const OTHER_MOD: i64 = 9904;
const AUTHOR: i64 = 9905;
const MOD_AUTHOR: i64 = 9906;
const OTHER: i64 = 9907;
const DAY: i64 = 86_400;

struct Fixture {
    services: TestFixture,
    app: Router,
    sessions: std::collections::BTreeMap<i64, SessionGrant>,
    fid: i64,
    now: i64,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }
}

fn config() -> WriteConfig {
    WriteConfig::default()
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        gono_forum_web::test_support::raise_posting_quotas(&db)
            .await
            .unwrap();
        content::seed_fixture(&db).await.unwrap();
        for (uid, adminid) in [
            (ADMIN, 1),
            (GLOBAL, 2),
            (MOD, 3),
            (OTHER_MOD, 3),
            (AUTHOR, 0),
            (MOD_AUTHOR, 3),
            (OTHER, 0),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid).bind(format!("restore-{uid}")).bind(format!("restore-{uid}@example.test")).bind(adminid)
                .execute(db.executor()).await.unwrap();
        }
        let forums: Vec<i64> = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 2")
            .fetch_all(db.executor())
            .await
            .unwrap();
        let (fid, other_fid) = (forums[0], forums[1]);
        for (uid, forum) in [(MOD, fid), (MOD_AUTHOR, fid), (OTHER_MOD, other_fid)] {
            db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(?,?)")
                .bind(uid)
                .bind(forum)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let config = services.config.clone();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = std::collections::BTreeMap::new();
        for uid in [ADMIN, GLOBAL, MOD, OTHER_MOD, AUTHOR, MOD_AUTHOR, OTHER] {
            sessions.insert(uid, http_auth::login(&identity, &db, uid).await);
        }
        let app = router::build(config, &services.root, db.clone());
        Self {
            services,
            app,
            sessions,
            fid,
            now: identity::now(),
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    fn session(&self, uid: i64) -> &SessionGrant {
        &self.sessions[&uid]
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn text(&self, sql: &str) -> String {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db().executor())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn publish(&self, uid: i64, fid: i64, title: &str, at: i64) -> Published {
        ContentWriter::new(self.db().clone(), config())
            .publish_at(
                uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: format!("{title} body"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap()
    }

    async fn reply(&self, uid: i64, tid: i64, body: &str, at: i64) -> Published {
        ContentWriter::new(self.db().clone(), config())
            .reply_at(
                uid,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap()
    }

    /// Uses the existing state actions: author withdrawal takes no reason, staff hiding requires one.
    async fn change(&self, uid: i64, pid: i64, action: ContentAction, at: i64) {
        let editor = ContentEditor::new(self.db().clone(), config(), 86_400);
        let actor = self.session(uid).member.actor();
        let post = editor.editable_at(actor, pid, at).await.unwrap();
        editor
            .change_at(
                actor,
                ChangeRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: if action == ContentAction::Withdraw || action == ContentAction::Restore
                    {
                        String::new()
                    } else {
                        "Broke the community rules".into()
                    },
                    action,
                },
                at,
            )
            .await
            .unwrap_or_else(|e| panic!("{action:?} {pid}: {e}"));
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.46:48322".parse::<SocketAddr>().unwrap(),
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

    async fn get(&self, path: &str, uid: i64) -> Reply {
        self.send(
            Request::builder()
                .uri(path)
                .header(
                    header::COOKIE,
                    format!("gonof_session={}", self.session(uid).session_token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post(&self, path: &str, uid: i64, fields: &[(&str, String)]) -> Reply {
        let session = self.session(uid);
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
                    format!("gonof_session={}", session.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
        )
        .await
    }

    async fn submit(&self, uid: i64, pid: i64, reason: &str, key: &str) -> Reply {
        self.post(
            "/my/restorations",
            uid,
            &[
                ("pid", pid.to_string()),
                ("reason", reason.into()),
                ("request_key", key.into()),
            ],
        )
        .await
    }

    async fn resolve(&self, uid: i64, id: i64, status: &str, resolution: &str) -> Reply {
        self.post(
            &format!("/manage/restorations/{id}"),
            uid,
            &[("status", status.into()), ("resolution", resolution.into())],
        )
        .await
    }

    async fn requests(&self, pid: i64) -> i64 {
        self.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_restore_request WHERE pid={pid}"
        ))
        .await
    }
}

fn request_id(reply: &Reply) -> i64 {
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    reply
        .location()
        .strip_prefix("/my/restorations/")
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn members_request_restoration_only_for_their_own_hidden_or_expired_withdrawals() {
    let f = Fixture::new().await;
    let now = f.now;
    // A thread hidden by a moderator, with replies from both the author and others.
    let hidden = f
        .publish(AUTHOR, f.fid, "RESTORE-HIDDEN", now - 1_000)
        .await;
    let inherited = f
        .reply(
            AUTHOR,
            hidden.tid,
            "a reply hidden with its thread",
            now - 900,
        )
        .await;
    let others = f
        .reply(OTHER, hidden.tid, "A reply from another member", now - 800)
        .await;
    f.change(MOD, hidden.pid, ContentAction::Hide, now - 500)
        .await;
    // A thread posted three days ago and withdrawn right then: past the author's time window.
    let expired = f
        .publish(AUTHOR, f.fid, "RESTORE-EXPIRED", now - 3 * DAY)
        .await;
    f.change(
        AUTHOR,
        expired.pid,
        ContentAction::Withdraw,
        now - 3 * DAY + 10,
    )
    .await;
    // A reply withdrawn just now: still within the time window, so the author restores it themselves.
    let public = f
        .publish(AUTHOR, f.fid, "RESTORE-PUBLIC", now - 2_000)
        .await;
    let recent = f
        .reply(AUTHOR, public.tid, "a reply withdrawn just now", now - 100)
        .await;
    f.change(AUTHOR, recent.pid, ContentAction::Withdraw, now - 50)
        .await;

    // The member's own list: requestable items get an entry, withdrawals within the window point back to the original
    // post, and public and collaterally hidden items get none.
    let threads = f.get("/my/threads", AUTHOR).await;
    assert_eq!(threads.status, StatusCode::OK);
    for pid in [hidden.pid, expired.pid] {
        assert!(
            threads.body.contains(&format!(
                "href=\"/my/restorations/new?pid={pid}\">Request restoration</a>"
            )),
            "{pid}: {}",
            threads.body
        );
    }
    assert!(
        !threads
            .body
            .contains(&format!("/my/restorations/new?pid={}", public.pid))
    );
    let posts = f.get("/my/posts", AUTHOR).await;
    assert!(
        posts.body.contains("Restore it yourself on the post"),
        "{}",
        posts.body
    );
    assert!(
        !posts
            .body
            .contains(&format!("/my/restorations/new?pid={}", inherited.pid))
    );
    assert!(
        !posts
            .body
            .contains(&format!("/my/restorations/new?pid={}", recent.pid))
    );

    // Withdrawal within the window: the form only shows a hint and submission is not accepted; the existing
    // self-restoration by the author is unaffected.
    let form = f
        .get(&format!("/my/restorations/new?pid={}", recent.pid), AUTHOR)
        .await;
    assert_eq!(form.status, StatusCode::OK);
    assert!(
        form.body.contains("restore it yourself on the thread page"),
        "{}",
        form.body
    );
    assert!(!form.body.contains("action=\"/my/restorations\""));
    let refused = f
        .submit(
            AUTHOR,
            recent.pid,
            "please restore",
            &identity::random_token(),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains("restore it yourself on the thread page")
    );
    // Public and collaterally hidden items are not accepted; others' posts give 404, the same as nonexistent ones.
    for (pid, status) in [
        (public.pid, StatusCode::UNPROCESSABLE_ENTITY),
        (inherited.pid, StatusCode::UNPROCESSABLE_ENTITY),
        (others.pid, StatusCode::NOT_FOUND),
        (i64::from(i32::MAX), StatusCode::NOT_FOUND),
    ] {
        let reply = f
            .submit(AUTHOR, pid, "please restore", &identity::random_token())
            .await;
        assert_eq!(reply.status, status, "{pid}: {}", reply.body);
    }
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_restore_request")
            .await,
        0
    );
    assert!(
        !f.get(&format!("/my/restorations/new?pid={}", others.pid), AUTHOR)
            .await
            .body
            .contains("A reply from another member")
    );

    // Submission: records the state at submission time and who did the hiding.
    let key = identity::random_token();
    let first = f
        .submit(
            AUTHOR,
            hidden.pid,
            "The reason for hiding does not match the facts",
            &key,
        )
        .await;
    let id = request_id(&first);
    assert_eq!(
        format!(
            "{}/{}/{}",
            f.text(&format!(
                "SELECT state FROM gonof_forum_restore_request WHERE id={id}"
            ))
            .await,
            f.text(&format!(
                "SELECT status FROM gonof_forum_restore_request WHERE id={id}"
            ))
            .await,
            f.scalar(&format!(
                "SELECT hidden_by FROM gonof_forum_restore_request WHERE id={id}"
            ))
            .await
        ),
        format!("hidden/pending/{MOD}")
    );
    // Replaying the same key returns the original request; the same key with changed content is 409; resubmitting with
    // a new key returns the same pending request.
    assert_eq!(
        request_id(
            &f.submit(
                AUTHOR,
                hidden.pid,
                "The reason for hiding does not match the facts",
                &key
            )
            .await
        ),
        id
    );
    assert_eq!(
        f.submit(AUTHOR, hidden.pid, "changed reason", &key)
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request_id(
            &f.submit(
                AUTHOR,
                hidden.pid,
                "submitted again",
                &identity::random_token()
            )
            .await
        ),
        id
    );
    assert_eq!(f.requests(hidden.pid).await, 1);
    // The list and details are only shown to the member.
    let list = f.get("/my/threads", AUTHOR).await;
    assert!(list.body.contains(&format!(
        "href=\"/my/restorations/{id}\">Restoration request pending</a>"
    )));
    let mine = f.get("/my/restorations", AUTHOR).await;
    assert!(
        mine.body
            .contains(&format!("#{id} · Hidden by moderators · Pending")),
        "{}",
        mine.body
    );
    let detail = f.get(&format!("/my/restorations/{id}"), AUTHOR).await;
    assert_eq!(detail.status, StatusCode::OK);
    assert!(
        detail
            .body
            .contains("The reason for hiding does not match the facts")
    );
    assert_eq!(
        f.get(&format!("/my/restorations/{id}"), OTHER).await.status,
        StatusCode::NOT_FOUND
    );

    // Authors restoring their own withdrawals within the time window still works, without going through a request.
    f.change(AUTHOR, recent.pid, ContentAction::Restore, now)
        .await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT deleted_at FROM gonof_forum_post WHERE pid={}",
            recent.pid
        ))
        .await,
        0
    );

    // At most 5 per person per 24 hours: record 4 more, the 6th is rejected; once moved out of the 24 hours it is
    // allowed.
    for _ in 0..4 {
        f.execute(&format!(
            "INSERT INTO gonof_forum_restore_request(uid,pid,state,reason,status,handler_uid,resolution,handled_at,dateline,request_key) VALUES({AUTHOR},{},'hidden','old request','accepted',{ADMIN},'handled',{now},{},'{}')",
            public.pid,
            now - 100,
            identity::random_token()
        ))
        .await;
    }
    let limited = f
        .submit(
            AUTHOR,
            expired.pid,
            "withdrawn past the window",
            &identity::random_token(),
        )
        .await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.body
    );
    assert_eq!(f.requests(expired.pid).await, 0);
    f.execute(&format!(
        "UPDATE gonof_forum_restore_request SET dateline={} WHERE reason='old request'",
        now - 2 * DAY
    ))
    .await;
    let accepted = f
        .submit(
            AUTHOR,
            expired.pid,
            "withdrawn past the window",
            &identity::random_token(),
        )
        .await;
    let expired_id = request_id(&accepted);
    assert_eq!(
        f.text(&format!(
            "SELECT state FROM gonof_forum_restore_request WHERE id={expired_id}"
        ))
        .await,
        "withdrawn"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_restore_request WHERE id={expired_id} AND hidden_by IS NULL"
        ))
        .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn staff_handle_restorations_in_scope_with_recusal_and_restore_atomically() {
    let f = Fixture::new().await;
    let now = f.now;
    let hidden = f
        .publish(AUTHOR, f.fid, "RESTORE-STAFF-HIDDEN", now - 1_000)
        .await;
    f.change(MOD, hidden.pid, ContentAction::Hide, now - 500)
        .await;
    let expired = f
        .publish(AUTHOR, f.fid, "RESTORE-STAFF-EXPIRED", now - 3 * DAY)
        .await;
    f.change(
        AUTHOR,
        expired.pid,
        ContentAction::Withdraw,
        now - 3 * DAY + 10,
    )
    .await;
    // The author is also a moderator of this forum: after the content was hidden by another moderator, they can only
    // request, not handle it themselves.
    let moderator_own = f
        .publish(MOD_AUTHOR, f.fid, "RESTORE-MOD-OWN", now - 1_000)
        .await;
    f.change(MOD, moderator_own.pid, ContentAction::Hide, now - 400)
        .await;

    let hidden_id = request_id(
        &f.submit(
            AUTHOR,
            hidden.pid,
            "please review the hiding",
            &identity::random_token(),
        )
        .await,
    );
    let expired_id = request_id(
        &f.submit(
            AUTHOR,
            expired.pid,
            "the withdrawal was a mistake",
            &identity::random_token(),
        )
        .await,
    );
    let own_id = request_id(
        &f.submit(
            MOD_AUTHOR,
            moderator_own.pid,
            "please review",
            &identity::random_token(),
        )
        .await,
    );

    // Moderators of other forums cannot see, open or handle it.
    let queue = f.get("/manage/restorations", OTHER_MOD).await;
    assert_eq!(queue.status, StatusCode::OK);
    assert!(
        !queue
            .body
            .contains(&format!("Restoration request #{hidden_id}"))
    );
    assert_eq!(
        f.get(&format!("/manage/restorations/{hidden_id}"), OTHER_MOD)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.resolve(OTHER_MOD, hidden_id, "accepted", "out of scope")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Ordinary members cannot enter the admin queue.
    assert_eq!(
        f.get("/manage/restorations", OTHER).await.status,
        StatusCode::FORBIDDEN
    );
    // This forum's moderators can see the queue; the admin content detail page also notes the pending request.
    let queue = f.get("/manage/restorations", MOD).await;
    assert!(
        queue
            .body
            .contains(&format!("Restoration request #{hidden_id}")),
        "{}",
        queue.body
    );
    let content = f.get(&format!("/manage/content/{}", hidden.pid), MOD).await;
    assert!(
        content
            .body
            .contains(&format!("href=\"/manage/restorations/{hidden_id}\""))
    );

    // The moderator who did the hiding recuses: the details only show a notice, submission is rejected, and both the
    // content and the request stay unchanged.
    let review = f
        .get(&format!("/manage/restorations/{hidden_id}"), MOD)
        .await;
    assert!(
        review
            .body
            .contains("You made this request or hid the content")
    );
    assert!(!review.body.contains("Confirm and notify the author"));
    assert_eq!(
        f.resolve(MOD, hidden_id, "accepted", "my own review")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    // A requester who is a moderator also recuses.
    assert_eq!(
        f.resolve(MOD_AUTHOR, own_id, "accepted", "restore my own")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT invisible FROM gonof_forum_post WHERE pid={}",
            hidden.pid
        ))
        .await,
        -1
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_restore_request WHERE status<>'pending'")
            .await,
        0
    );

    // A global moderator accepts: unhides in the same transaction, and the author receives only one "Content unhidden"
    // notification.
    let done = f
        .resolve(GLOBAL, hidden_id, "accepted", "no violation after review")
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);
    assert_eq!(
        f.scalar(&format!(
            "SELECT invisible FROM gonof_forum_post WHERE pid={}",
            hidden.pid
        ))
        .await,
        0
    );
    assert_eq!(
        format!(
            "{}/{}/{}",
            f.text(&format!(
                "SELECT status FROM gonof_forum_restore_request WHERE id={hidden_id}"
            ))
            .await,
            f.scalar(&format!(
                "SELECT handler_uid FROM gonof_forum_restore_request WHERE id={hidden_id}"
            ))
            .await,
            f.text(&format!(
                "SELECT resolution FROM gonof_forum_restore_request WHERE id={hidden_id}"
            ))
            .await
        ),
        format!("accepted/{GLOBAL}/no violation after review")
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid={AUTHOR} AND type='moderation' AND pid={}",
            hidden.pid
        ))
        .await,
        2,
        "one for hiding, one for unhiding"
    );
    assert_eq!(
        f.text(&format!(
            "SELECT reason FROM gonof_home_notification WHERE uid={AUTHOR} AND pid={} AND event_key LIKE 'moderation.%.uh' ORDER BY id DESC LIMIT 1",
            hidden.pid
        ))
        .await,
        // The row keeps only the reason; "Content unhidden: " comes from the action code at the end of the event key
        // when the list is shown (docs/architecture.md, "Languages").
        "no violation after review"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.unhide' AND operationuid={GLOBAL}"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='restore.resolve' AND operationuid={GLOBAL} AND uid={AUTHOR}"
        ))
        .await,
        1
    );
    // Replaying the same decision succeeds; changing the decision is 409.
    assert_eq!(
        f.resolve(GLOBAL, hidden_id, "accepted", "no violation after review")
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.resolve(GLOBAL, hidden_id, "rejected", "changed my mind")
            .await
            .status,
        StatusCode::CONFLICT
    );

    // This forum's moderator rejects a request for a withdrawal past the window (no one did any hiding, so no recusal
    // is needed): the author receives the decision.
    let rejected = f
        .resolve(
            MOD,
            expired_id,
            "rejected",
            "Withdrawn past the window, kept as is",
        )
        .await;
    assert_eq!(rejected.status, StatusCode::SEE_OTHER, "{}", rejected.body);
    assert_eq!(
        f.text(&format!(
            "SELECT reason FROM gonof_home_notification WHERE uid={AUTHOR} AND event_key='restore.{expired_id}.result'"
        ))
        .await,
        // "Your restoration request was not accepted: " comes from the request's status when the list is shown.
        "Withdrawn past the window, kept as is"
    );
    // Within 30 days after a rejection no new request can be made; after the cooldown it can, and once an
    // administrator accepts, the withdrawal is restored.
    let cooling = f
        .submit(
            AUTHOR,
            expired.pid,
            "request again",
            &identity::random_token(),
        )
        .await;
    assert_eq!(cooling.status, StatusCode::FORBIDDEN, "{}", cooling.body);
    let posts = f.get("/my/threads", AUTHOR).await;
    assert!(posts.body.contains(&format!(
        "href=\"/my/restorations/{expired_id}\">Restoration request not accepted</a>"
    )));
    f.execute(&format!(
        "UPDATE gonof_forum_restore_request SET handled_at={}, dateline={} WHERE id={expired_id}",
        now - 31 * DAY,
        now - 32 * DAY
    ))
    .await;
    let again = request_id(
        &f.submit(
            AUTHOR,
            expired.pid,
            "request again",
            &identity::random_token(),
        )
        .await,
    );
    assert_ne!(again, expired_id);
    let restored = f
        .resolve(ADMIN, again, "accepted", "restoration approved")
        .await;
    assert_eq!(restored.status, StatusCode::SEE_OTHER, "{}", restored.body);
    assert_eq!(
        f.scalar(&format!(
            "SELECT p.deleted_at+t.deleted_at FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid={}",
            expired.pid
        ))
        .await,
        0
    );
    assert_eq!(
        f.text(&format!(
            "SELECT reason FROM gonof_home_notification WHERE uid={AUTHOR} AND pid={} AND event_key LIKE 'moderation.%.r' ORDER BY id DESC LIMIT 1",
            expired.pid
        ))
        .await,
        "restoration approved"
    );

    // A moderator's own request is handled by a global moderator; when the content was already restored by someone
    // else, accepting only records the decision and sends a separate decision notification.
    f.change(GLOBAL, moderator_own.pid, ContentAction::Unhide, now)
        .await;
    let late = f
        .resolve(GLOBAL, own_id, "accepted", "already restored")
        .await;
    assert_eq!(late.status, StatusCode::SEE_OTHER, "{}", late.body);
    assert_eq!(
        f.text(&format!(
            "SELECT reason FROM gonof_home_notification WHERE uid={MOD_AUTHOR} AND event_key='restore.{own_id}.result'"
        ))
        .await,
        "already restored"
    );

    // The log page filters restoration request events by type.
    let logs = f.get("/manage/logs?kind=restore", ADMIN).await;
    assert_eq!(logs.status, StatusCode::OK);
    assert!(logs.body.contains("Restoration request handled"));
    assert!(logs.body.contains("Restoration request submitted"));
    f.services.cleanup().await.unwrap();
}
