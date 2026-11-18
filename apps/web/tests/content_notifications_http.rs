#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content::{
        self,
        writes::{ContentWriter, NewThread},
    },
    db::Database,
    identity, notifications, router, urls,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    db: Database,
    app: Router,
    sessions: Vec<String>,
    csrf_tokens: Vec<String>,
    first_pid: i64,
}
impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        let identity = identity::Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = vec![];
        let mut csrf_tokens = Vec::new();
        for uid in 9001_i64..=9003 {
            gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,'synthetic-hash',?,1,1)")
                .bind(uid).bind(format!("notice-http-{uid}")).bind(format!("notice-{uid}@example.test")).execute(db.executor()).await.unwrap();
            let grant = http_auth::login(&identity, &db, uid).await;
            csrf_tokens.push(grant.csrf_token);
            sessions.push(grant.session_token);
        }
        let first_pid = gono_forum_web::db::query_scalar(
            "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        gono_forum_web::db::query("UPDATE gonof_forum_post SET authorid=9001 WHERE pid=?")
            .bind(first_pid)
            .execute(db.executor())
            .await
            .unwrap();
        gono_forum_web::db::query("UPDATE gonof_forum_thread SET authorid=9001 WHERE tid=103")
            .execute(db.executor())
            .await
            .unwrap();
        // The author copy is maintained only by code: after changing the author, the same code rewrites it.
        gono_forum_web::test_support::sync_member_copies(&db, 9001)
            .await
            .unwrap();
        let app = router::build(config, &root, db.clone());
        Self {
            _services: services,
            db,
            app,
            sessions,
            csrf_tokens,
            first_pid,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        user: Option<usize>,
        fields: &[(&str, String)],
    ) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(user) = user {
            req = req.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.sessions[user]),
            );
        }
        if method == "POST" {
            req = req
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter())
            .finish();
        let response = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, headers, html)
    }
    async fn get(&self, path: &str, user: Option<usize>) -> (StatusCode, HeaderMap, String) {
        self.call("GET", path, user, &[]).await
    }
    async fn boosted_get(&self, path: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("HX-Request", "true")
                    .header("HX-Boosted", "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    fn csrf(&self, user: usize) -> Vec<(&'static str, String)> {
        vec![
            ("_csrf", self.csrf_tokens[user].clone()),
            ("request_key", identity::random_token()),
        ]
    }
    async fn execute(&self, sql: &str) {
        gono_forum_web::db::raw_sql(sql)
            .execute(self.db.executor())
            .await
            .unwrap();
    }
    /// After raw SQL changes a thread or post, synchronizes the copies and invalidates the cache the way the
    /// moderation and edit entry points do.
    async fn execute_in(&self, tid: i64, sql: &str) {
        self.execute(sql).await;
        gono_forum_web::test_support::resync_thread(&self.db, tid)
            .await
            .unwrap();
    }
    async fn assert_tombstone_permalink(
        &self,
        pid: i64,
        location: &str,
        marker: &str,
        secret: &str,
    ) {
        let path = format!("/posts/{pid}");
        let response = self.get(&path, None).await;
        assert_eq!(response.0, StatusCode::FOUND);
        assert_eq!(response.1[header::LOCATION], location);
        let boosted = self.boosted_get(&path).await;
        assert_eq!(boosted.status(), StatusCode::OK);
        assert_eq!(boosted.headers()["HX-Redirect"], location);
        assert!(!boosted.headers().contains_key(header::LOCATION));

        let (page, anchor) = location.split_once('#').unwrap();
        let response = self.get(page, None).await;
        assert_eq!(response.0, StatusCode::OK);
        assert!(response.2.contains(&format!("id=\"{anchor}\"")));
        assert!(response.2.contains(marker));
        assert!(!response.2.contains(secret));
    }
}

#[tokio::test]
async fn source_permalink_uses_real_pagination_and_rechecks_current_public_visibility() {
    let f = Fixture::new().await;
    let thread = gono_forum_web::test_support::thread_with_all_pages(&f.db, 103)
        .await
        .unwrap()
        .unwrap();
    let mut pids = vec![thread.first_pid];
    pids.extend(
        thread
            .posts
            .iter()
            .filter(|p| !p.hidden && !p.withdrawn)
            .map(|p| p.pid as i64),
    );
    for pid in pids.iter().step_by(20).chain(pids.last()) {
        let response = f.get(&format!("/posts/{pid}"), None).await;
        assert_eq!(response.0, StatusCode::FOUND);
        assert_eq!(
            response.1[header::LOCATION],
            urls::post_location(
                &thread,
                *pid,
                false,
                gono_forum_web::test_support::SEEDED_PAGING
            )
            .unwrap()
        );
        let boosted = f.boosted_get(&format!("/posts/{pid}")).await;
        assert_eq!(boosted.status(), StatusCode::OK);
        assert_eq!(
            boosted.headers()["HX-Redirect"],
            urls::post_location(
                &thread,
                *pid,
                false,
                gono_forum_web::test_support::SEEDED_PAGING
            )
            .unwrap()
        );
        assert!(!boosted.headers().contains_key(header::LOCATION));
    }
    let target = thread.posts.iter().find(|p| p.root_pid.is_none()).unwrap();
    let target_location = urls::post_location(
        &thread,
        target.pid as i64,
        false,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .unwrap();
    const SECRET: &str = "hidden reply body the permalink must not leak";
    f.execute_in(
        103,
        &format!(
            "UPDATE gonof_forum_post SET message='{SECRET}' WHERE pid={}",
            target.pid
        ),
    )
    .await;
    for (mutation, marker) in [
        (
            format!(
                "UPDATE gonof_forum_post SET invisible=1 WHERE pid={}",
                target.pid
            ),
            "This reply is hidden.",
        ),
        (
            format!(
                "UPDATE gonof_forum_post SET invisible=0,deleted_at=1 WHERE pid={}",
                target.pid
            ),
            "This reply was withdrawn. The discussion stays.",
        ),
    ] {
        f.execute_in(103, &mutation).await;
        f.assert_tombstone_permalink(target.pid as i64, &target_location, marker, SECRET)
            .await;
    }
    let child = thread
        .posts
        .iter()
        .find(|p| p.root_pid == Some(target.pid))
        .unwrap();
    f.execute_in(
        103,
        &format!(
            "UPDATE gonof_forum_post SET invisible=1,deleted_at=0 WHERE pid={}",
            target.pid
        ),
    )
    .await;
    const CHILD_SECRET: &str = "nested reply body that must not leak once its parent is hidden";
    f.execute_in(
        103,
        &format!(
            "UPDATE gonof_forum_post SET message='{CHILD_SECRET}' WHERE pid={}",
            child.pid
        ),
    )
    .await;
    f.assert_tombstone_permalink(
        child.pid as i64,
        &urls::post_location(
            &thread,
            child.pid as i64,
            false,
            gono_forum_web::test_support::SEEDED_PAGING,
        )
        .unwrap(),
        "This reply is hidden.",
        CHILD_SECRET,
    )
    .await;
    for mutation in [
        "UPDATE gonof_forum_thread SET readperm=1 WHERE tid=103",
        "UPDATE gonof_forum_thread SET readperm=0,visibility='private' WHERE tid=103",
    ] {
        f.execute_in(103, mutation).await;
        assert_eq!(
            f.get(&format!("/posts/{}", f.first_pid), Some(0)).await.0,
            StatusCode::NOT_FOUND
        );
    }
    const OPENING_SECRET: &str = "first post body of a withdrawn thread that must not leak";
    f.execute(&format!(
        "UPDATE gonof_forum_post SET message='{OPENING_SECRET}' WHERE pid={}",
        f.first_pid
    ))
    .await;
    f.execute_in(
        103,
        "UPDATE gonof_forum_thread SET visibility='public',deleted_at=1 WHERE tid=103",
    )
    .await;
    f.assert_tombstone_permalink(
        f.first_pid,
        &urls::post_location(
            &thread,
            f.first_pid,
            false,
            gono_forum_web::test_support::SEEDED_PAGING,
        )
        .unwrap(),
        "The author withdrew this thread. The replies stay.",
        OPENING_SECRET,
    )
    .await;
    assert_eq!(
        f.get("/posts/9223372036854775807", None).await.0,
        StatusCode::NOT_FOUND
    );
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn quote_entry_prefills_native_editor_without_reply_target_and_preserves_preview() {
    let f = Fixture::new().await;
    let path = format!("/thread-103?quote={}", f.first_pid);
    let response = f.get(&path, Some(1)).await;
    assert_eq!(response.0, StatusCode::OK);
    assert!(
        response
            .2
            .contains(&format!("[Quote](post:{})\n\n</textarea>", f.first_pid))
    );
    assert!(response.2.contains("name=\"reply_to_pid\" value=\"0\""));
    assert!(
        response
            .2
            .contains(&format!("data-draft=\"reply-103-0-quote-{}\"", f.first_pid))
    );
    assert!(response.2.contains("Quoting the selected post"));
    let anonymous = f.get(&path, None).await;
    assert_eq!(anonymous.0, StatusCode::OK);
    assert!(anonymous.2.contains(&format!(
        "return_to=%2Fthread-103%3Fquote%3D{}%23reply-editor",
        f.first_pid
    )));
    let mut fields = f.csrf(1);
    fields.extend([
        ("tid", "103".into()),
        ("quote_pid", f.first_pid.to_string()),
        ("location", "/thread-103".into()),
        (
            "body",
            format!("[old source text](post:{} \"old author\")", f.first_pid),
        ),
    ]);
    let preview = f.call("POST", "/api/preview", Some(1), &fields).await;
    assert_eq!(preview.0, StatusCode::OK);
    assert!(
        preview
            .2
            .contains(&format!("href=\"/posts/{}\"", f.first_pid))
    );
    assert!(preview.2.contains(">Quoted post</a>"));
    assert!(
        preview
            .2
            .contains(&format!("data-draft=\"reply-103-0-quote-{}\"", f.first_pid))
    );
    fields.iter_mut().find(|(key, _)| *key == "body").unwrap().1 = "[bad source](post:0)".into();
    assert_eq!(
        f.call("POST", "/api/preview", Some(1), &fields).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    fields
        .iter_mut()
        .find(|(key, _)| *key == "_csrf")
        .unwrap()
        .1 = "bad".into();
    assert_eq!(
        f.call("POST", "/api/preview", Some(1), &fields).await.0,
        StatusCode::FORBIDDEN
    );
    let foreign: i64 =
        gono_forum_web::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE tid<>103 LIMIT 1")
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(
        f.get(&format!("/thread-103?quote={foreign}"), Some(1))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    f.execute(&format!(
        "UPDATE gonof_forum_post SET deleted_at=1 WHERE pid={}",
        f.first_pid
    ))
    .await;
    assert_eq!(f.get(&path, Some(1)).await.0, StatusCode::NOT_FOUND);
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn mention_and_quote_notices_render_current_kind_and_first_post_open_checks_ownership() {
    let f = Fixture::new().await;
    let fid = gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=103")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), Default::default());
    let mut published = vec![];
    for (index, body) in [
        "[@someone](member:9001)".to_owned(),
        format!("[quote](post:{}) [@same member](member:9001)", f.first_pid),
    ]
    .into_iter()
    .enumerate()
    {
        let result = writer
            .publish(
                9002,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: format!("Mention notice thread {index}"),
                    body,
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
        published.push(result);
    }
    let delivered = notifications::deliver_pending(&f.db, identity::now())
        .await
        .unwrap();
    assert_eq!(delivered.failed, 0);
    let response = f.get("/my/notifications", Some(0)).await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1[header::CACHE_CONTROL], "private, no-store");
    assert!(response.2.contains("data-notification-kind=\"mention\""));
    assert!(response.2.contains("data-notification-kind=\"quote\""));
    assert!(response.2.contains("mentioned you"));
    assert!(response.2.contains("quoted your post"));
    assert_eq!(response.2.matches(">View content</button>").count(), 2);
    // The "Latest threads" card in the right column lists the public threads published in this run, so only the body half is
    // checked; otherwise it would be mistaken for a notification leak.
    let other_member = f.get("/my/notifications", Some(2)).await.2;
    let panel = other_member.split("<aside").next().unwrap();
    assert!(!panel.contains("Mention notice thread"));
    assert_eq!(
        f.get("/my/notifications", None).await.0,
        StatusCode::SEE_OTHER
    );
    let id: i64 = gono_forum_web::db::query_scalar(
        "SELECT id FROM gonof_home_notification WHERE pid=? AND uid=9001",
    )
    .bind(published[0].pid)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    let path = format!("/my/notifications/{id}/open");
    let mut invalid_csrf = f.csrf(0);
    invalid_csrf[0].1 = "invalid".into();
    assert_eq!(
        f.call("POST", &path, Some(0), &invalid_csrf).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call("POST", &path, Some(2), &f.csrf(2)).await.0,
        StatusCode::NOT_FOUND
    );
    let opened = f.call("POST", &path, Some(0), &f.csrf(0)).await;
    assert_eq!(opened.0, StatusCode::SEE_OTHER);
    assert_eq!(
        opened.1[header::LOCATION],
        format!("/thread-{}#floor-1", published[0].tid)
    );
    f.execute_in(
        published[0].tid,
        &format!(
            "UPDATE gonof_forum_post SET invisible=1 WHERE pid={}",
            published[0].pid
        ),
    )
    .await;
    let hidden = f.get("/my/notifications", Some(0)).await;
    assert!(!hidden.2.contains("Mention notice thread 0"));
    assert!(hidden.2.contains("Content not available"));
    assert_eq!(
        f.call("POST", &path, Some(0), &f.csrf(0)).await.0,
        StatusCode::NOT_FOUND
    );
    f._services.cleanup_ref().await.unwrap();
}
