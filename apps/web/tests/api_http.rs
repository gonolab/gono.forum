//! HTTP contract of the member JSON API: Bearer authentication, idempotency, rate limiting, settings page and
//! admin revocation.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities,
    config::Config,
    content::{
        self,
        edits::{ContentEditor, EditRequest},
        writes::{ContentWriteError, ContentWriter, NewReply, WriteConfig},
    },
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    i18n::Lang,
    identity::{self, Identity, SessionGrant},
    interaction::Interaction,
    router, runtime,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const PASSWORD: &str = "Api-http-fixture-password-2026!";
const ADMIN: i64 = 9200;
const MEMBER: i64 = 9201;
const OTHER: i64 = 9202;
const UNVERIFIED: i64 = 9203;

struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
    app: Router,
    first_pid: i64,
}

fn password_hash() -> String {
    let salt = SaltString::encode_b64(b"api-http-fixture-salt").unwrap();
    Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

impl Fixture {
    /// `settings` are the site settings to change (key, value). It changes the database directly, and the app reads
    /// it on its first request.
    async fn new(settings: &[(&str, &str)]) -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        for (key, value) in settings {
            gono_forum_web::test_support::set_setting(&db, key, value)
                .await
                .unwrap();
        }
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let regdate = identity::now() - 30 * 86_400;
        for (uid, verified, role) in [
            (ADMIN, 1_i64, 1_i64),
            (MEMBER, 1, 0),
            (OTHER, 1, 0),
            (UNVERIFIED, 0, 0),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,?,?,?,?,?)")
                .bind(uid)
                .bind(format!("api-http-{uid}"))
                .bind(password_hash())
                .bind(format!("api-http-{uid}@example.test"))
                .bind(verified)
                .bind(role)
                .bind(regdate)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let first_pid: i64 =
            db::query_scalar("SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1")
                .fetch_one(db.executor())
                .await
                .unwrap();
        db::query("UPDATE gonof_forum_post SET authorid=? WHERE pid=?")
            .bind(OTHER)
            .bind(first_pid)
            .execute(db.executor())
            .await
            .unwrap();
        db::query("UPDATE gonof_forum_thread SET authorid=? WHERE tid=103")
            .bind(OTHER)
            .execute(db.executor())
            .await
            .unwrap();
        let app = router::build_with_global(
            config,
            &root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            db,
            identity,
            app,
            first_pid,
        }
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        self.identity
            .login(&format!("api-http-{uid}"), PASSWORD)
            .await
            .unwrap()
    }

    async fn key(&self, uid: i64) -> (SessionGrant, String) {
        let grant = self.login(uid).await;
        let secret = self
            .identity
            .create_api_key(&grant.session_token, PASSWORD, 86_400)
            .await
            .unwrap()
            .secret;
        (grant, secret)
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }

    async fn execute(&self, sql: &str) {
        db::query(sql).execute(self.db.executor()).await.unwrap();
    }

    async fn actor(&self, uid: i64) -> identity::Actor {
        identity::Actor {
            uid,
            credentials_version: self
                .scalar(&format!(
                    "SELECT credentials_version FROM gonof_common_member WHERE uid={uid}"
                ))
                .await,
            api_key: None,
        }
    }

    /// Change settings: shares the same global cache handle as the app, so the app's next request sees the new value
    /// after saving.
    async fn settings(&self, change: impl FnOnce(&mut runtime::SiteSettings)) {
        runtime::update(
            &self.db,
            &self.services.global,
            self.actor(ADMIN).await,
            "site.settings.save",
            change,
        )
        .await
        .unwrap();
    }

    /// JSON request; when `headers` gives a content-type, the default application/json is not added.
    async fn api(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        let explicit_type = headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
        if body.is_some() && !explicit_type {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .app
            .clone()
            .oneshot(
                builder
                    .body(Body::from(body.unwrap_or_default().to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(text(&bytes)));
        (status, headers, value)
    }

    async fn page(
        &self,
        path: &str,
        grant: Option<&SessionGrant>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut builder = Request::builder().uri(path).header("HX-Request", "false");
        if let Some(grant) = grant {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", grant.session_token),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, text(&bytes))
    }

    /// Open a page with the session and another cookie (the one-time display cookie issued after generating a
    /// token).
    async fn page_with(
        &self,
        path: &str,
        grant: &SessionGrant,
        cookie: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let request = Request::builder()
            .uri(path)
            .header("HX-Request", "false")
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}; {cookie}", grant.session_token),
            )
            .body(Body::empty())
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, text(&bytes))
    }

    async fn form(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter())
            .finish();
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("HX-Request", "false")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}", grant.session_token),
            )
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, text(&bytes))
    }
}

async fn thread_subject(db: &Database, tid: i64) -> String {
    db::query_scalar("SELECT subject FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn post_message(db: &Database, pid: i64) -> String {
    db::query_scalar("SELECT message FROM gonof_forum_post WHERE pid=?")
        .bind(pid)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

/// The one-time display cookie (`name=value`) in the response that generates a token; checks that it is sent only
/// to /my/api, is HttpOnly and lasts 10 minutes.
fn reveal_cookie(headers: &HeaderMap) -> String {
    let line = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with("gonof_api_reveal__port58833="))
        .expect("creating a token must set the one-time display cookie");
    assert!(line.contains("; Path=/my/api;"), "{line}");
    assert!(
        line.contains("HttpOnly") && line.contains("SameSite=Lax"),
        "{line}"
    );
    assert!(line.contains("Max-Age=600"), "{line}");
    line.split(';').next().unwrap().to_owned()
}

fn secret_in(html: &str) -> String {
    let start = html
        .find("data-api-secret>")
        .expect("the page must show the token")
        + "data-api-secret>".len();
    html[start..].split('<').next().unwrap().to_owned()
}

fn header(headers: &HeaderMap, name: header::HeaderName) -> String {
    headers
        .get(name)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn bearer_tokens_are_the_only_credential_and_failures_are_throttled() {
    // The hard lower bound for authentication failures is 5 (`settings.api.auth_failures_per_5min`).
    let f = Fixture::new(&[("settings.api.auth_failures_per_5min", "5")]).await;
    let (grant, secret) = f.key(MEMBER).await;

    let (status, headers, body) = f.api("GET", "/api/v1/me", None, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        header(&headers, header::WWW_AUTHENTICATE),
        "Bearer realm=\"gonof\""
    );
    assert_eq!(header(&headers, header::CACHE_CONTROL), "no-store");
    assert!(header(&headers, header::CONTENT_TYPE).starts_with("application/json"));
    assert_eq!(body["code"], "invalid_token");
    // The error is two fields: the stable code and the text for people (docs/api.md, "Responses").
    assert_eq!(body.as_object().unwrap().len(), 2, "{body}");
    assert!(
        body["error"].as_str().is_some_and(|text| !text.is_empty()),
        "{body}"
    );
    assert!(headers.get(header::SET_COOKIE).is_none());

    let cookie = format!("gonof_session__port58833={}", grant.session_token);
    let (status, _, body) = f
        .api(
            "GET",
            "/api/v1/me",
            None,
            None,
            &[("cookie", cookie.as_str())],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a cookie session cannot call the JSON API"
    );
    assert_eq!(body["code"], "invalid_token");

    let (status, _, body) = f
        .api("GET", "/api/v1/me", Some("not-a-token"), None, &[])
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "invalid_token");

    let (status, headers, body) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, header::CACHE_CONTROL), "no-store");
    assert!(headers.get(header::SET_COOKIE).is_none());
    assert_eq!(body["uid"], MEMBER);
    assert_eq!(body["username"], format!("api-http-{MEMBER}"));
    assert_eq!(body["email_verified"], true);
    assert_eq!(body["can_post"], true);
    assert_eq!(body["profile_url"], format!("{ORIGIN}/space/{MEMBER}"));
    assert_eq!(body["key"]["hint"], secret[..14]);
    assert!(body["key"]["expires_at"].as_i64().unwrap() > identity::now());

    assert!(
        f.identity
            .revoke_api_key(&grant.session_token)
            .await
            .unwrap()
    );
    let (status, _, body) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token stops working as soon as the API is turned off"
    );
    assert_eq!(body["code"], "invalid_token");

    // A disabled token counts against the per-IP failure quota just like a random token; once over the limit, every
    // request from the same IP gets 429.
    let bogus = format!("{}{}", identity::API_KEY_PREFIX, identity::random_token());
    let attempts = (0..12).map(|_| f.api("GET", "/api/v1/me", Some(&bogus), None, &[]));
    let statuses = futures_util::future::join_all(attempts).await;
    assert_eq!(
        statuses
            .iter()
            .filter(|(status, ..)| *status == StatusCode::UNAUTHORIZED)
            .count(),
        4
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|(status, ..)| *status == StatusCode::TOO_MANY_REQUESTS)
            .count(),
        8
    );
    let (_, fresh) = f.key(MEMBER).await;
    let (status, headers, body) = f.api("GET", "/api/v1/me", Some(&fresh), None, &[]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    assert_eq!(header(&headers, header::RETRY_AFTER), "300");
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn threads_and_replies_publish_through_the_shared_transaction() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;

    let (status, _, forums) = f
        .api("GET", "/api/v1/forums", Some(&secret), None, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let daily = forums["forums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|forum| forum["slug"] == "daily")
        .expect("the Daily forum is visible")
        .clone();
    assert!(daily["fid"].as_i64().unwrap() > 0);
    assert_eq!(daily["name"], "Daily");
    // `parent_fid` is the only added field; it is null for top-level forums.
    assert!(daily["parent_fid"].is_null(), "{daily}");

    let input = r#"{"forum":"daily","title":"API thread","body":"Body **Markdown**"}"#;
    let key = [("idempotency-key", "thread-1")];
    let (status, headers, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(input), &key)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(header(&headers, header::CACHE_CONTROL), "no-store");
    let tid = created["tid"].as_i64().unwrap();
    let first = created["pid"].as_i64().unwrap();
    assert!(tid > 0 && first > 0);
    assert_eq!(created["position"], 1);
    assert_eq!(created["url"], format!("{ORIGIN}/thread-{tid}"));
    assert_eq!(
        f.scalar(&format!(
            "SELECT authorid FROM gonof_forum_thread WHERE tid={tid}"
        ))
        .await,
        MEMBER
    );
    let source: String = db::query_scalar("SELECT source FROM gonof_common_log WHERE type='content.thread.publish' AND uid=? ORDER BY id DESC LIMIT 1")
        .bind(MEMBER)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(source, "api");

    let (status, _, replay) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(input), &key)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the same key with the same content returns the original result"
    );
    assert_eq!(replay["pid"], first);
    let changed = input.replace("API thread", "Another title");
    let (status, _, conflict) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&changed),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "conflict");
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_thread WHERE authorid={MEMBER}"
        ))
        .await,
        1
    );

    let (status, _, reply) = f
        .api(
            "POST",
            &format!("/api/v1/threads/{tid}/replies"),
            Some(&secret),
            Some(r#"{"body":"Reply to the first post"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    assert_eq!(reply["tid"], tid);
    assert_eq!(reply["position"], 2);
    assert!(reply.get("root_pid").is_none());
    let url = reply["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("{ORIGIN}/thread-{tid}")) && url.contains("#floor-2"));
    let root = reply["pid"].as_i64().unwrap();

    let (status, _, sub) = f
        .api(
            "POST",
            &format!("/api/v1/posts/{root}/replies"),
            Some(&secret),
            Some(r#"{"body":"Nested reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sub}");
    assert_eq!(sub["root_pid"], root);
    assert_eq!(sub["position"], 3);
    let sub_pid = sub["pid"].as_i64().unwrap();

    let (status, _, nested) = f
        .api(
            "POST",
            &format!("/api/v1/posts/{sub_pid}/replies"),
            Some(&secret),
            Some(r#"{"body":"Answer to the nested reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{nested}");
    assert_eq!(
        nested["root_pid"], root,
        "an answer to a nested reply stays under the same parent floor"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT reply_to_pid FROM gonof_forum_post WHERE pid={}",
            nested["pid"]
        ))
        .await,
        sub_pid
    );

    let (status, _, body) = f
        .api(
            "POST",
            &format!("/api/v1/posts/{first}/replies"),
            Some(&secret),
            Some(r#"{"body":"x"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "validation");

    let missing = [
        (
            "POST",
            "/api/v1/posts/999999/replies".to_owned(),
            r#"{"body":"x"}"#,
        ),
        (
            "POST",
            "/api/v1/threads/999999/replies".to_owned(),
            r#"{"body":"x"}"#,
        ),
        (
            "POST",
            "/api/v1/threads".to_owned(),
            r#"{"forum":"nope","title":"x","body":"y"}"#,
        ),
    ];
    for (method, path, body) in missing {
        let (status, _, value) = f.api(method, &path, Some(&secret), Some(body), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {value}");
        assert_eq!(value["code"], "not_found");
    }

    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"x","body":"y","tags":[]}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some("forum=daily"),
            &[("content-type", "text/plain")],
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["code"], "unsupported_media_type");
    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"  ","body":"y"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "validation");
    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"x","body":"y"}"#),
            &[("idempotency-key", "bad key with spaces")],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");

    let by_id = format!(
        r#"{{"forum":{},"title":"By forum number","body":"Body"}}"#,
        daily["fid"]
    );
    let (status, _, body) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(&by_id), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn authors_edit_their_threads_and_replies_with_version_checks() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"Original title","body":"Original body"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let tid = created["tid"].as_i64().unwrap();
    let first = created["pid"].as_i64().unwrap();
    let (status, _, reply) = f
        .api(
            "POST",
            &format!("/api/v1/threads/{tid}/replies"),
            Some(&secret),
            Some(r#"{"body":"Original reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let reply_pid = reply["pid"].as_i64().unwrap();

    let thread_path = format!("/api/v1/threads/{tid}");
    let key = [("idempotency-key", "edit-1")];
    let (status, _, edited) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"title":"New title"}"#),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["pid"], first);
    assert_eq!(edited["version"], 2);
    assert_eq!(edited["url"], format!("{ORIGIN}/thread-{tid}"));
    assert_eq!(thread_subject(&f.db, tid).await, "New title");
    assert_eq!(
        post_message(&f.db, first).await,
        "Original body",
        "an omitted body keeps the current value"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_revision WHERE pid={first} AND version=1 AND message='Original body'"
        ))
        .await,
        1,
        "the content before the edit goes into an internal revision"
    );
    let source: String = db::query_scalar("SELECT source FROM gonof_common_log WHERE type='content.edit' AND operationuid=? ORDER BY id DESC LIMIT 1")
        .bind(MEMBER)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(source, "api");

    let (status, _, replay) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"title":"New title"}"#),
            &key,
        )
        .await;
    assert_eq!(
        (status, replay["version"].as_i64()),
        (StatusCode::OK, Some(2)),
        "a replay does not bump the version again"
    );
    let (status, _, conflict) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"title":"Another title"}"#),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "conflict");

    let (status, _, edited) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"body":"New body","expected_version":2}"#),
            &[("idempotency-key", "edit-body")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["version"], 3);
    assert_eq!(post_message(&f.db, first).await, "New body");
    assert_eq!(
        thread_subject(&f.db, tid).await,
        "New title",
        "an omitted title keeps the current value"
    );
    // Omitted fields are not part of the request content. If the body or title changes later, retrying the original
    // PATCH must still hit the original record.
    let (status, _, replay) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"title":"New title"}"#),
            &key,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a replay of a title-only edit ignores later body changes: {replay}"
    );
    assert_eq!(
        replay["version"], 2,
        "a replay returns the version that edit wrote, not the later body edit's"
    );
    assert_eq!(post_message(&f.db, first).await, "New body");
    let (status, _, edited) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"title":"Later title"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let (status, _, replay) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"body":"New body","expected_version":2}"#),
            &[("idempotency-key", "edit-body")],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a replay of a body-only edit ignores later title changes: {replay}"
    );
    assert_eq!(replay["version"], 3);
    assert_eq!(thread_subject(&f.db, tid).await, "Later title");
    // Every write other than an edit records no version; neither did edits before migration 0007. Replaying such an
    // edit record falls back to the post's current version, read inside the edit transaction.
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE uid={MEMBER} AND pid={first} AND version IS NULL"
        ))
        .await,
        1,
        "publishing the thread records no version"
    );
    db::query("UPDATE gonof_forum_post_request SET version=NULL WHERE uid=? AND version=3")
        .bind(MEMBER)
        .execute(f.db.executor())
        .await
        .unwrap();
    let (status, _, replay) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"body":"New body","expected_version":2}"#),
            &[("idempotency-key", "edit-body")],
        )
        .await;
    assert_eq!(
        (status, replay["version"].as_i64()),
        (StatusCode::OK, Some(4)),
        "an edit record without a version replays with the current version: {replay}"
    );
    let (status, _, stale) = f
        .api(
            "PATCH",
            &thread_path,
            Some(&secret),
            Some(r#"{"body":"One more edit","expected_version":2}"#),
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a stale version cannot overwrite: {stale}"
    );
    assert_eq!(post_message(&f.db, first).await, "New body");

    for (body, status) in [
        ("{}", StatusCode::UNPROCESSABLE_ENTITY),
        (r#"{"title":"x","extra":1}"#, StatusCode::BAD_REQUEST),
        (r#"{"body":"   "}"#, StatusCode::UNPROCESSABLE_ENTITY),
        (
            r#"{"body":"x","expected_version":0}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (actual, _, value) = f
            .api("PATCH", &thread_path, Some(&secret), Some(body), &[])
            .await;
        assert_eq!(actual, status, "{body}: {value}");
    }

    let (status, _, edited) = f
        .api(
            "PATCH",
            &format!("/api/v1/posts/{reply_pid}"),
            Some(&secret),
            Some(r#"{"body":"New reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["version"], 2);
    assert!(edited["url"].as_str().unwrap().contains("#floor-2"));
    assert_eq!(post_message(&f.db, reply_pid).await, "New reply");
    let (status, _, body) = f
        .api(
            "PATCH",
            &format!("/api/v1/posts/{first}"),
            Some(&secret),
            Some(r#"{"body":"x"}"#),
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the first post is edited through the thread endpoint: {body}"
    );

    let foreign_reply = f
        .scalar(
            "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=0 ORDER BY position LIMIT 1",
        )
        .await;
    let (status, _, body) = f
        .api(
            "PATCH",
            &format!("/api/v1/posts/{foreign_reply}"),
            Some(&secret),
            Some(r#"{"body":"Tampered"}"#),
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "someone else's content can neither be edited nor revealed: {body}"
    );
    assert_eq!(body["code"], "not_found");
    let (status, ..) = f
        .api(
            "PATCH",
            "/api/v1/posts/999999",
            Some(&secret),
            Some(r#"{"body":"x"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    f.execute(&format!(
        "UPDATE gonof_forum_post SET dateline=dateline-2*86400 WHERE pid={reply_pid}"
    ))
    .await;
    let (status, _, body) = f
        .api(
            "PATCH",
            &format!("/api/v1/posts/{reply_pid}"),
            Some(&secret),
            Some(r#"{"body":"Late edit"}"#),
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "past the edit window: {body}"
    );
    assert_eq!(body["code"], "forbidden");
    assert_eq!(post_message(&f.db, reply_pid).await, "New reply");
    f.services.cleanup_ref().await.unwrap();
}

/// Posting limits answer `429 quota` with `Retry-After`, the seconds until the limit lets the member post again.
#[tokio::test]
async fn posting_limits_say_how_long_to_wait() {
    let f = Fixture::new(&[
        ("settings.posting.daily_threads", "1"),
        ("settings.posting.reply_interval_seconds", "600"),
    ])
    .await;
    let (_, secret) = f.key(MEMBER).await;
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"The only one today","body":"Body"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, headers, refused) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"One more today","body":"Body"}"#),
            &[],
        )
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("quota")),
        "{refused}"
    );
    // The daily allowance starts again at midnight in the site's time zone (UTC+8 in the fixture).
    let now = identity::now();
    let offset = 8 * 3600;
    let midnight = (now + offset).div_euclid(86_400) * 86_400 + 86_400 - offset;
    let wait: i64 = header(&headers, header::RETRY_AFTER)
        .parse()
        .expect("a posting limit carries Retry-After");
    assert!((midnight - now - wait).abs() <= 5, "{wait}");

    let replies = format!("/api/v1/threads/{}/replies", created["tid"]);
    let (status, _, reply) = f
        .api(
            "POST",
            &replies,
            Some(&secret),
            Some(r#"{"body":"First reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let (status, headers, refused) = f
        .api(
            "POST",
            &replies,
            Some(&secret),
            Some(r#"{"body":"Second reply"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{refused}");
    let wait: i64 = header(&headers, header::RETRY_AFTER).parse().unwrap();
    assert!(
        (590..=600).contains(&wait),
        "the reply interval has the rest of its 600 seconds to run: {wait}"
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A retry of a published thread replays even when the board's slug or the site's time zone changed since: the
/// transaction finds the request by its key, so the API takes the board from the record when the slug finds none, and a
/// deadline typed as a local time enters the digest as the text sent, not as the moment it was read as.
#[tokio::test]
async fn publication_retries_replay_after_the_slug_or_the_time_zone_changes() {
    let f = Fixture::new(&[("settings.posting.daily_threads", "20")]).await;
    let (_, secret) = f.key(MEMBER).await;
    let thread = r#"{"forum":"daily","title":"Before the rename","body":"Body"}"#;
    let key = [("idempotency-key", "renamed-board")];
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(thread), &key)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    f.execute("UPDATE gonof_forum_forum SET slug='daily-renamed' WHERE slug='daily'")
        .await;
    f.services.global.clear_local();
    let (status, _, replayed) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(thread), &key)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a retry with the old slug replays: {replayed}"
    );
    assert_eq!(
        (&replayed["tid"], &replayed["pid"]),
        (&created["tid"], &created["pid"])
    );
    let other = r#"{"forum":"daily","title":"Another title","body":"Body"}"#;
    let (status, _, conflict) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(other), &key)
        .await;
    assert_eq!(
        (status, conflict["code"].as_str()),
        (StatusCode::CONFLICT, Some("conflict")),
        "another request under the recorded key is still a conflict: {conflict}"
    );
    let (status, _, missing) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(thread),
            &[("idempotency-key", "renamed-board-new")],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a new request with the old slug finds no board: {missing}"
    );

    // Deadlines typed as site time (UTC+8 in the fixture), one day ahead on a whole minute.
    let at = (identity::now() + 86_400) / 60 * 60;
    let typed = (chrono::DateTime::from_timestamp(at, 0).unwrap() + chrono::Duration::hours(8))
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    let poll = |deadline: &str| {
        format!(
            r#"{{"forum":"daily-renamed","title":"Poll","body":"Pick one","category":4,"options":["Alpha","Beta"],"deadline":{deadline}}}"#
        )
    };
    // Records written before kept the moment in the digest, the same as a deadline sent in seconds: a retry with the
    // text that reads as that moment still replays against them.
    let by_moment = poll(&at.to_string());
    let by_text = poll(&format!("\"{typed}\""));
    let moment_key = [("idempotency-key", "poll-by-moment")];
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&by_moment),
            &moment_key,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, _, replayed) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&by_text),
            &moment_key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed["tid"], created["tid"]);

    let text_key = [("idempotency-key", "poll-by-text")];
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&by_text),
            &text_key,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    gono_forum_web::test_support::set_setting(&f.db, "settings.time.offset_minutes", "0")
        .await
        .unwrap();
    f.services.global.clear_local();
    let (status, _, replayed) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&by_text),
            &text_key,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the same text replays after the time zone changed: {replayed}"
    );
    assert_eq!(
        (&replayed["tid"], &replayed["pid"]),
        (&created["tid"], &created["pid"])
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A retry of a thread whose activity has ended since still replays with the original result while its key is
/// remembered; a new request with the same ended deadline is refused.
#[tokio::test]
async fn ended_activities_still_replay_their_publication() {
    let f = Fixture::new(&[("settings.posting.daily_threads", "20")]).await;
    let admin = f.actor(ADMIN).await;
    let now = identity::now();
    gono_forum_web::activities::adjust(
        &f.db,
        admin,
        MEMBER,
        100,
        &identity::random_token(),
        "Test budget",
        now,
    )
    .await
    .unwrap();
    let (_, secret) = f.key(MEMBER).await;
    // The deadline is taken right before the first request, with a few seconds to spare, so that a slow runner still
    // publishes before it passes; the setup above can take longer than that on SQLite in CI.
    let deadline = identity::now() + 5;
    let bounty = format!(
        r#"{{"forum":"daily","title":"Short bounty","body":"Quick question","category":1,"budget":10,"deadline":{deadline}}}"#
    );
    let key = [("idempotency-key", "short-bounty")];
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&bounty),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let wait = (deadline + 1 - identity::now()).max(0) as u64;
    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;

    let (status, _, replayed) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&bounty),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(
        (&replayed["tid"], &replayed["pid"]),
        (&created["tid"], &created["pid"])
    );
    // The key with different content is a reused key whether or not the activity has ended: 409, not the ended
    // deadline's 422.
    let changed = format!(
        r#"{{"forum":"daily","title":"Another bounty","body":"Quick question","category":1,"budget":10,"deadline":{deadline}}}"#
    );
    let (status, _, conflict) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&changed),
            &key,
        )
        .await;
    assert_eq!(
        (status, conflict["code"].as_str()),
        (StatusCode::CONFLICT, Some("conflict")),
        "{conflict}"
    );
    let (status, _, refused) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&bounty),
            &[("idempotency-key", "short-bounty-again")],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["error"], Lang::EN.activity_error_deadline());
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn activities_publish_and_participate_through_the_api() {
    // The five thread types plus validation cases exceed the default quota of 5 threads per day; the quota itself is
    // covered by other tests.
    let f = Fixture::new(&[
        ("settings.api.requests_per_minute", "200"),
        ("settings.api.writes_per_minute", "60"),
        ("settings.posting.daily_threads", "20"),
    ])
    .await;
    let admin = f.actor(ADMIN).await;
    let now = identity::now();
    gono_forum_web::activities::adjust(
        &f.db,
        admin,
        MEMBER,
        500,
        &identity::random_token(),
        "Test budget",
        now,
    )
    .await
    .unwrap();
    let (_, secret) = f.key(MEMBER).await;
    let (_, other) = f.key(OTHER).await;
    let deadline = now + 3600;
    let available =
        |uid: i64| format!("SELECT available FROM gonof_common_credit_account WHERE uid={uid}");
    let frozen =
        |uid: i64| format!("SELECT frozen FROM gonof_common_credit_account WHERE uid={uid}");

    let (status, _, credits) = f
        .api("GET", "/api/v1/credits", Some(&secret), None, &[])
        .await;
    assert_eq!(status, StatusCode::OK, "{credits}");
    assert_eq!(
        (credits["available"].as_i64(), credits["frozen"].as_i64()),
        (Some(500), Some(0))
    );

    // Bounty: publishing freezes the budget, and once the author accepts, the full amount goes to the replier.
    let bounty = format!(
        r#"{{"forum":"daily","title":"Bounty question","body":"Who can answer","category":1,"budget":100,"deadline":{deadline}}}"#
    );
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(&bounty), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let bounty_tid = created["tid"].as_i64().unwrap();
    assert_eq!(f.scalar(&frozen(MEMBER)).await, 100);
    assert_eq!(
        f.scalar(&format!(
            "SELECT category FROM gonof_forum_thread WHERE tid={bounty_tid}"
        ))
        .await,
        1
    );
    let (status, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{bounty_tid}/activity"),
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    assert_eq!(card["kind"], 1);
    assert_eq!(card["label"], "Bounty");
    assert_eq!(card["budget"], 100);
    assert_eq!(card["state"], "open");
    assert_eq!(card["owner_uid"], MEMBER);
    let (status, _, body) = f
        .api(
            "GET",
            "/api/v1/threads/103/activity",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an ordinary thread has no activity: {body}"
    );

    let (status, _, reply) = f
        .api(
            "POST",
            &format!("/api/v1/threads/{bounty_tid}/replies"),
            Some(&other),
            Some(r#"{"body":"Here is my answer"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let answer = reply["pid"].as_i64().unwrap();
    let accept_path = format!("/api/v1/threads/{bounty_tid}/accept");
    let accept_body = format!(r#"{{"pid":{answer}}}"#);
    let (status, _, denied) = f
        .api("POST", &accept_path, Some(&other), Some(&accept_body), &[])
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{denied}");
    assert_eq!(denied["code"], "activity_rejected");
    let key = [("idempotency-key", "accept-1")];
    let (status, _, accepted) = f
        .api(
            "POST",
            &accept_path,
            Some(&secret),
            Some(&accept_body),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(
        accepted,
        serde_json::json!({"tid": bounty_tid, "accepted_pid": answer})
    );
    assert_eq!(f.scalar(&available(OTHER)).await, 100);
    assert_eq!(f.scalar(&frozen(MEMBER)).await, 0);
    let (_, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{bounty_tid}/activity"),
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (card["state"].as_str(), card["accepted_pid"].as_i64()),
        (Some("settled"), Some(answer))
    );
    let (status, ..) = f
        .api(
            "POST",
            &accept_path,
            Some(&secret),
            Some(&accept_body),
            &key,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a replay with the same key returns the original result"
    );
    let (status, _, closed) = f
        .api("POST", &accept_path, Some(&secret), Some(&accept_body), &[])
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{closed}");

    // Poll: the deadline uses the same text as the web form, and option numbers come from the activity card.
    let text_deadline = (chrono::DateTime::from_timestamp(now + 86_400, 0).unwrap()
        + chrono::Duration::hours(8))
    .format("%Y-%m-%dT%H:%M")
    .to_string();
    let poll = format!(
        r#"{{"forum":"daily","title":"Poll","body":"Pick one","category":4,"options":["Alpha","Beta","Gamma"],"max_choices":2,"deadline":"{text_deadline}"}}"#
    );
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(&poll), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let poll_tid = created["tid"].as_i64().unwrap();
    let activity_path = format!("/api/v1/threads/{poll_tid}/activity");
    let (_, _, card) = f.api("GET", &activity_path, Some(&other), None, &[]).await;
    let numbers: Vec<i64> = card["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [1, 2, 3]);
    assert_eq!(card["options"][1]["label"], "Beta");
    assert_eq!(
        (card["max_choices"].as_i64(), card["has_voted"].as_bool()),
        (Some(2), Some(false))
    );
    assert!(card["deadline"].as_i64().unwrap() > now + 80_000);
    let vote_path = format!("/api/v1/threads/{poll_tid}/vote");
    let vote_key = [("idempotency-key", "vote-1")];
    let (status, _, voted) = f
        .api(
            "POST",
            &vote_path,
            Some(&other),
            Some(r#"{"choices":[1,3]}"#),
            &vote_key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{voted}");
    assert_eq!(voted["choices"], serde_json::json!([1, 3]));
    let (_, _, card) = f.api("GET", &activity_path, Some(&other), None, &[]).await;
    assert_eq!(card["has_voted"], true);
    assert_eq!(card["voters"], 1);
    assert_eq!(
        (
            card["options"][0]["votes"].as_i64(),
            card["options"][1]["votes"].as_i64(),
            card["options"][2]["votes"].as_i64()
        ),
        (Some(1), Some(0), Some(1))
    );
    let (status, ..) = f
        .api(
            "POST",
            &vote_path,
            Some(&other),
            Some(r#"{"choices":[1,3]}"#),
            &vote_key,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a replay with the same key and choices"
    );
    let (status, _, again) = f
        .api(
            "POST",
            &vote_path,
            Some(&other),
            Some(r#"{"choices":[2]}"#),
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a vote cannot be changed: {again}"
    );
    // The same key in another poll is another request, not a retry: refused, and nothing is counted there.
    let second_poll = poll.replace("\"title\":\"Poll\"", "\"title\":\"Second poll\"");
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&second_poll),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let second_tid = created["tid"].as_i64().unwrap();
    let (status, _, reused) = f
        .api(
            "POST",
            &format!("/api/v1/threads/{second_tid}/vote"),
            Some(&other),
            Some(r#"{"choices":[1,3]}"#),
            &vote_key,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reused}");
    assert_eq!(reused["code"], "activity_rejected");
    assert_eq!(reused["error"], Lang::EN.activity_error_request_reused());
    let (_, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{second_tid}/activity"),
            Some(&other),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (card["voters"].as_i64(), card["has_voted"].as_bool()),
        (Some(0), Some(false))
    );
    for body in [
        r#"{"choices":[9]}"#,
        r#"{"choices":[]}"#,
        r#"{"choices":[1,2,3]}"#,
    ] {
        let (status, _, value) = f
            .api("POST", &vote_path, Some(&secret), Some(body), &[])
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {value}");
        assert_eq!(value["code"], "activity_rejected");
    }

    // Red packet: a reply automatically claims one share.
    let redpacket = format!(
        r#"{{"forum":"daily","title":"Red packet","body":"Come and claim","category":2,"budget":40,"shares":4,"distribution":"equal","deadline":{deadline}}}"#
    );
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&redpacket),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let redpacket_tid = created["tid"].as_i64().unwrap();
    assert_eq!(f.scalar(&frozen(MEMBER)).await, 40);
    let (status, _, reply) = f
        .api(
            "POST",
            &format!("/api/v1/threads/{redpacket_tid}/replies"),
            Some(&other),
            Some(r#"{"body":"Claimed"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    assert_eq!(f.scalar(&available(OTHER)).await, 110);
    let (_, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{redpacket_tid}/activity"),
            Some(&other),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (card["remaining_shares"].as_i64(), card["entries"].as_i64()),
        (Some(3), Some(1))
    );
    assert_eq!(
        (card["my_entry"].as_bool(), card["my_amount"].as_i64()),
        (Some(true), Some(10))
    );

    // Lottery and news.
    let lottery = format!(
        r#"{{"forum":"daily","title":"Lottery","body":"Reply to enter","category":3,"budget":30,"shares":3,"deadline":{deadline}}}"#
    );
    let (status, _, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&lottery),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(f.scalar(&frozen(MEMBER)).await, 30 + 30);
    let news = r#"{"forum":"daily","title":"News","body":"Summary","category":5,"source_url":"https://example.com/news","source_name":"Example site","source_author":"Reporter"}"#;
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(news), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (_, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{}/activity", created["tid"]),
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (card["kind"].as_i64(), card["source_url"].as_str()),
        (Some(5), Some("https://example.com/news"))
    );

    // Validation is done by the publish transaction and returns 422.
    for (body, fragment) in [
        (format!(r#"{{"forum":"daily","title":"x","body":"y","category":1,"deadline":{deadline}}}"#), "activity budget"),
        (r#"{"forum":"daily","title":"x","body":"y","category":9}"#.to_owned(), "valid thread type"),
        (format!(r#"{{"forum":"daily","title":"x","body":"y","category":2,"budget":10,"shares":3,"deadline":{deadline}}}"#), "divide evenly"),
        (format!(r#"{{"forum":"daily","title":"x","body":"y","category":1,"budget":100000,"deadline":{deadline}}}"#), "Not enough Coins"),
        (format!(r#"{{"forum":"daily","title":"x","body":"y","category":4,"options":["Only one"],"deadline":{deadline}}}"#), "2 to 20 options"),
        (r#"{"forum":"daily","title":"x","body":"y","category":1,"budget":10,"deadline":"2000-01-01T00:00"}"#.to_owned(), "deadline must be in the future"),
    ] {
        let (status, _, value) = f
            .api("POST", "/api/v1/threads", Some(&secret), Some(&body), &[])
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {value}");
        assert_eq!(value["code"], "validation");
        assert!(value["error"].as_str().unwrap().contains(fragment), "{value}");
    }
    assert_eq!(
        f.scalar(&frozen(MEMBER)).await,
        60,
        "a failed publish freezes no budget"
    );

    // Check-in: rejected while the rule is off; once on, once per day.
    let (status, _, body) = f
        .api("POST", "/api/v1/check-in", Some(&secret), None, &[])
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let signin = gono_forum_web::activities::rules(&f.db)
        .await
        .unwrap()
        .into_iter()
        .find(|rule| rule.key == "signin")
        .unwrap();
    gono_forum_web::activities::update_rule(
        &f.db,
        &f.services.global,
        admin,
        "signin",
        true,
        5,
        100,
        signin.version,
        now,
    )
    .await
    .unwrap();
    let before = f.scalar(&available(MEMBER)).await;
    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/check-in",
            Some(&secret),
            None,
            &[("idempotency-key", "checkin-1")],
        )
        .await;
    assert_eq!(
        (status, body["credited"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{body}"
    );
    // Check-in is idempotent per member and Shanghai calendar day, not replayed by request key: sending the same
    // Idempotency-Key again also only gets credited=false (check-in stores no request keys).
    let (status, _, body) = f
        .api(
            "POST",
            "/api/v1/check-in",
            Some(&secret),
            None,
            &[("idempotency-key", "checkin-1")],
        )
        .await;
    assert_eq!(
        (status, body["credited"].as_bool()),
        (StatusCode::OK, Some(false)),
        "credited only once per day: {body}"
    );
    let (_, _, credits) = f
        .api("GET", "/api/v1/credits", Some(&secret), None, &[])
        .await;
    assert_eq!(credits["checked_in"], true);
    assert_eq!(credits["available"].as_i64(), Some(before + 5));
    assert_eq!(credits["frozen"], 60);
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn favorites_and_likes_set_explicit_states() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;

    let (status, _, body) = f
        .api(
            "PUT",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, serde_json::json!({"tid": 103, "favorited": true}));
    let (status, _, body) = f
        .api(
            "PUT",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (status, body["favorited"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid={MEMBER} AND tid=103"
        ))
        .await,
        1
    );
    let (status, _, body) = f
        .api(
            "DELETE",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (status, body["favorited"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid={MEMBER} AND tid=103"
        ))
        .await,
        0
    );
    let (status, _, body) = f
        .api(
            "PUT",
            "/api/v1/threads/999999/favorite",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");

    let path = format!("/api/v1/posts/{}/like", f.first_pid);
    let (status, _, body) = f.api("PUT", &path, Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        serde_json::json!({"pid": f.first_pid, "liked": true, "count": 1})
    );
    let (_, _, body) = f.api("PUT", &path, Some(&secret), None, &[]).await;
    assert_eq!(body["count"], 1, "setting it again does not count twice");
    let (_, _, body) = f.api("DELETE", &path, Some(&secret), None, &[]).await;
    assert_eq!(
        body,
        serde_json::json!({"pid": f.first_pid, "liked": false, "count": 0})
    );

    let (status, _, own) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(r#"{"forum":"daily","title":"My own thread","body":"Body"}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{own}");
    let (status, _, body) = f
        .api(
            "PUT",
            &format!("/api/v1/posts/{}/like", own["pid"]),
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "members cannot like their own posts"
    );
    assert_eq!(body["code"], "forbidden");

    let key = [("idempotency-key", "favorite-1")];
    let (status, ..) = f
        .api(
            "PUT",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = f
        .api(
            "DELETE",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &key,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "one idempotency key cannot be used for the opposite action"
    );
    assert_eq!(body["code"], "conflict");
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn account_state_site_switch_read_only_and_body_limit_are_enforced() {
    let f = Fixture::new(&[("settings.api.max_body_kib", "2")]).await;
    let (_, secret) = f.key(MEMBER).await;
    let input = r#"{"forum":"daily","title":"State check","body":"Body"}"#;

    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=0 WHERE uid={MEMBER}"
    ))
    .await;
    let (status, _, me) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(
        (status, me["can_post"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    let (status, _, body) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(input), &[])
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a member without a verified email cannot post: {body}"
    );
    assert_eq!(body["code"], "forbidden");
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={MEMBER}"
    ))
    .await;

    f.execute(&format!(
        "UPDATE gonof_common_member SET freeze=1 WHERE uid={MEMBER}"
    ))
    .await;
    let (status, _, body) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "account_restricted");
    f.execute(&format!(
        "UPDATE gonof_common_member SET freeze=0 WHERE uid={MEMBER}"
    ))
    .await;

    f.settings(|settings| settings.site.read_only = true).await;
    let (status, _, body) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(input), &[])
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "read_only");
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reading still works while read-only"
    );
    f.settings(|settings| {
        settings.site.read_only = false;
        settings.api.enabled = false;
    })
    .await;
    let (status, _, body) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "api_disabled");
    f.settings(|settings| settings.api.enabled = true).await;

    let huge = format!(
        r#"{{"forum":"daily","title":"Large body","body":"{}"}}"#,
        "x".repeat(3000)
    );
    let (status, _, body) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(&huge), &[])
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "payload_too_large");

    let (status, _, body) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(input), &[])
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "publishing works again afterwards: {body}"
    );
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn per_key_limits_cover_all_requests_and_write_requests_separately() {
    let f = Fixture::new(&[
        ("settings.api.requests_per_minute", "5"),
        ("settings.api.writes_per_minute", "2"),
    ])
    .await;
    let (_, secret) = f.key(MEMBER).await;
    for _ in 0..2 {
        let (status, ..) = f
            .api(
                "PUT",
                "/api/v1/threads/103/favorite",
                Some(&secret),
                None,
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, headers, body) = f
        .api(
            "PUT",
            "/api/v1/threads/103/favorite",
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    assert_eq!(header(&headers, header::RETRY_AFTER), "60");
    for _ in 0..2 {
        let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _, body) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn settings_page_generates_rotates_and_disables_tokens() {
    let f = Fixture::new(&[]).await;
    let (status, headers, _) = f.page("/my/api", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        header(&headers, header::LOCATION),
        "/login?return_to=%2Fmy%2Fapi"
    );

    let grant = f.login(MEMBER).await;
    let (status, headers, html) = f.page("/my/api", Some(&grant)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, header::CACHE_CONTROL), "private, no-store");
    assert!(html.contains("The API is off."));
    assert!(html.contains(&format!("{ORIGIN}/api/v1")));
    assert!(html.contains("action=\"/my/api/enable\""));
    assert!(!html.contains("data-api-secret"));

    let (status, _, html) = f
        .form(
            "/my/api/enable",
            &grant,
            &[("_csrf", &grant.csrf_token), ("current_password", "wrong")],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        html.contains(Lang::EN.account_error_wrong_password()),
        "{html}"
    );
    let (status, ..) = f
        .form("/my/api/enable", &grant, &[("current_password", PASSWORD)])
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the CSRF token is missing");
    assert!(f.identity.api_key(MEMBER).await.unwrap().is_none());

    // A successful generation does not render the raw token directly: the raw token goes into a one-time display
    // cookie and a 303 redirects to GET this page. So a refresh shows the GET page and does not resubmit the
    // generate form (otherwise a refresh would quietly generate another token, invalidating the one just saved).
    let (status, headers, html) = f
        .form(
            "/my/api/enable",
            &grant,
            &[("_csrf", &grant.csrf_token), ("current_password", PASSWORD)],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, header::LOCATION), "/my/api");
    assert!(!html.contains("data-api-secret"));
    let reveal = reveal_cookie(&headers);
    // A display page that fails (here its token query) keeps the cookie, so a refresh can still show the token.
    let rename = |from: &str, to: &str| format!("ALTER TABLE {from} RENAME TO {to}");
    db::raw_sql(&rename(
        "gonof_common_member_api_key",
        "gonof_common_member_api_key_away",
    ))
    .execute(f.db.executor())
    .await
    .unwrap();
    let (status, headers, _) = f.page_with("/my/api", &grant, &reveal).await;
    db::raw_sql(&rename(
        "gonof_common_member_api_key_away",
        "gonof_common_member_api_key",
    ))
    .execute(f.db.executor())
    .await
    .unwrap();
    assert!(status.is_server_error(), "{status}");
    assert!(
        !headers.get_all(header::SET_COOKIE).iter().any(|value| value
            .to_str()
            .unwrap()
            .starts_with("gonof_api_reveal__port58833=")),
        "a failed display page keeps the cookie"
    );
    let (status, headers, html) = f.page_with("/my/api", &grant, &reveal).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("shown only this once"));
    let secret = secret_in(&html);
    assert!(identity::valid_api_secret(&secret));
    assert!(!reveal.contains(&secret), "the cookie holds ciphertext");
    assert!(
        headers.get_all(header::SET_COOKIE).iter().any(|value| value
            .to_str()
            .unwrap()
            .starts_with("gonof_api_reveal__port58833=;")
            && value.to_str().unwrap().contains("Max-Age=0")),
        "the display page clears the cookie once read"
    );
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    // Another member who gets this cookie cannot see it either.
    let other = f.login(UNVERIFIED).await;
    let (_, _, html) = f.page_with("/my/api", &other, &reveal).await;
    assert!(!html.contains(&secret));

    let (_, _, html) = f.page("/my/api", Some(&grant)).await;
    assert!(
        !html.contains(&secret),
        "a refresh no longer shows the token"
    );
    assert!(html.contains("The API is on."));
    assert!(html.contains(&secret[..14]));
    assert!(html.contains("action=\"/my/api/rotate\""));
    assert!(html.contains("action=\"/my/api/disable\""));

    let (status, headers, _) = f
        .form(
            "/my/api/rotate",
            &grant,
            &[("_csrf", &grant.csrf_token), ("current_password", PASSWORD)],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let rotated_reveal = reveal_cookie(&headers);
    let (_, _, html) = f.page_with("/my/api", &grant, &rotated_reveal).await;
    let rotated = secret_in(&html);
    assert_ne!(rotated, secret);
    // Even if the old display cookie is still there, it no longer shows the token that has been replaced.
    let (_, _, html) = f.page_with("/my/api", &grant, &reveal).await;
    assert!(!html.contains(&secret) && !html.contains("data-api-secret"));
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old token stops working at once"
    );
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&rotated), None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    let (status, headers, _) = f
        .form("/my/api/disable", &grant, &[("_csrf", &grant.csrf_token)])
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, header::LOCATION), "/my/api?disabled=true");
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&rotated), None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, _, html) = f.page("/my/api?disabled=true", Some(&grant)).await;
    assert!(
        html.contains("The API is off, and the old token stopped working.")
            && html.contains("The API is off.")
    );

    let unverified = f.login(UNVERIFIED).await;
    let (_, _, html) = f.page("/my/api", Some(&unverified)).await;
    assert!(html.contains("Verify your email to turn on the API."));
    assert!(!html.contains("action=\"/my/api/enable\""));
    let (status, ..) = f
        .form(
            "/my/api/enable",
            &unverified,
            &[
                ("_csrf", &unverified.csrf_token),
                ("current_password", PASSWORD),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    f.settings(|settings| settings.api.enabled = false).await;
    let (_, _, html) = f.page("/my/api", Some(&grant)).await;
    assert!(html.contains("This site has turned the API off"));
    let (status, ..) = f
        .form(
            "/my/api/enable",
            &grant,
            &[("_csrf", &grant.csrf_token), ("current_password", PASSWORD)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    f.settings(|settings| settings.api.enabled = true).await;

    let (_, _, home) = f.page("/", Some(&grant)).await;
    assert!(
        home.contains("href=\"/my/account\""),
        "the account menu reaches settings through account security"
    );
    let (_, _, account) = f.page("/my/account", Some(&grant)).await;
    assert!(account.contains("href=\"/my/api\""));
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn admins_see_key_status_and_revoke_with_a_reason() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    let admin = f.login(ADMIN).await;

    let (status, _, html) = f
        .page(&format!("/manage/users/{MEMBER}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<h3>API token</h3>") && html.contains("<p>Active · "));
    assert!(html.contains(&secret[..14]));
    assert!(html.contains(&format!("action=\"/manage/users/{MEMBER}/api/revoke\"")));
    let (_, _, list) = f.page("/manage/users", Some(&admin)).await;
    assert!(
        !list.contains("data-api-key"),
        "the list page does not load the token state"
    );

    let (status, ..) = f
        .form(
            &format!("/manage/users/{MEMBER}/api/revoke"),
            &admin,
            &[("_csrf", &admin.csrf_token), ("reason", "  ")],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a reason is required"
    );
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    let member = f.login(OTHER).await;
    let (status, ..) = f
        .form(
            &format!("/manage/users/{MEMBER}/api/revoke"),
            &member,
            &[("_csrf", &member.csrf_token), ("reason", "Not allowed")],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, headers, _) = f
        .form(
            &format!("/manage/users/{MEMBER}/api/revoke"),
            &admin,
            &[("_csrf", &admin.csrf_token), ("reason", "Test revocation")],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        header(&headers, header::LOCATION),
        format!("/manage/users/{MEMBER}")
    );
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, _, html) = f
        .page(&format!("/manage/users/{MEMBER}"), Some(&admin))
        .await;
    assert!(html.contains("This member has no API token."));
    assert_eq!(
        f.scalar(&format!("SELECT COUNT(*) FROM gonof_common_log WHERE type='api.key.revoke' AND uid={MEMBER} AND operationuid={ADMIN}")).await,
        1
    );
    let (_, _, logs) = f.page("/manage/logs", Some(&admin)).await;
    assert!(logs.contains("API token revoked"));

    // Opening the member API is on the "Registration & permissions" tab (settings are saved per tab).
    let (_, _, settings) = f.page("/manage/settings/registration", Some(&admin)).await;
    assert!(settings.contains("name=\"api_enabled\"") && settings.contains("checked"));
    let (status, ..) = f
        .form(
            "/manage/settings/registration",
            &admin,
            &[
                ("_csrf", &admin.csrf_token),
                ("registration", "invite"),
                ("invitations_enabled", "true"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        !runtime::read(&f.db).await.unwrap().api.enabled,
        "an unchecked box turns it off"
    );
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn json_api_paths_never_reach_html_handlers() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    for (method, path) in [
        ("GET", "/api/v1/unknown"),
        ("GET", "/api/v1"),
        ("POST", "/api/v1/comments"),
        ("POST", "/api/v1/star"),
        ("POST", "/api/v1/follow"),
        ("GET", "/api/v1/anything/comments"),
    ] {
        let (status, headers, body) = f.api(method, path, Some(&secret), Some("{}"), &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert!(
            header(&headers, header::CONTENT_TYPE).starts_with("application/json"),
            "{method} {path}: {body}"
        );
        assert_eq!(body["code"], "not_found", "{method} {path}");
    }
    let (status, _, body) = f
        .api("GET", "/api/v1/threads", Some(&secret), None, &[])
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["code"], "method_not_allowed");
    let (status, _, body) = f.api("GET", "/api/v1/unknown", None, None, &[]).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown path without authentication also returns JSON"
    );
    assert_eq!(body["code"], "not_found");

    // Existing HTMX endpoints are unaffected: an anonymous submission of the full form still gets an HTML error page,
    // not JSON.
    let form = format!(
        "_csrf=stale&request_key={}&desired=true",
        identity::random_token()
    );
    let (status, headers, _) = f
        .api(
            "POST",
            "/api/thread-103/star",
            None,
            Some(&form),
            &[
                ("content-type", "application/x-www-form-urlencoded"),
                ("origin", ORIGIN),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(header(&headers, header::CONTENT_TYPE).starts_with("text/html"));
    f.services.cleanup_ref().await.unwrap();
}

/// The first statement in the reply and publish transactions that locks member rows (`activities::lock_members`).
const MEMBER_LOCK: &str = "SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE";

/// Wait until `sql` has been sent but has not returned yet: this means the locking statement is stuck on a row lock
/// held by the test.
async fn wait_blocked(profile: &RequestProfile, sql: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !profile.snapshot().iter().any(|r| {
            r.source == QuerySource::Application
                && r.sql == sql
                && r.rows_returned == 0
                && r.elapsed.is_zero()
        }) {
            // Only to reduce polling overhead; ordering is decided by "the statement has been observed as sent", not
            // by how long we wait.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no waiting statement observed: {sql}"));
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    for scheme in ["Bearer", "bearer", "BEARER", "bEaReR"] {
        let value = format!("{scheme} {secret}");
        let (status, _, body) = f
            .api(
                "GET",
                "/api/v1/me",
                None,
                None,
                &[("authorization", &value)],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "scheme {scheme}: {body}");
        assert_eq!(body["uid"], MEMBER);
    }
    for (case, value) in [
        ("no scheme", secret.clone()),
        ("another scheme", format!("Basic {secret}")),
        ("no space", format!("Bearer{secret}")),
    ] {
        let (status, _, body) = f
            .api(
                "GET",
                "/api/v1/me",
                None,
                None,
                &[("authorization", &value)],
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}");
        assert_eq!(body["code"], "invalid_token", "{case}");
    }
    f.services.cleanup_ref().await.unwrap();
}

/// The token was valid when the request came in and was revoked while queuing for the member row lock: once the
/// revocation commits, this write must not commit.
#[tokio::test]
async fn revoking_a_key_stops_writes_already_waiting_for_the_account_lock() {
    // Only verifies the MySQL row-lock wait orchestration.
    gono_forum_web::mysql_only!();
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    let posts = format!("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={MEMBER}");
    let before = f.scalar(&posts).await;
    let mut blocker = f.db.begin_write().await.unwrap();
    db::query(MEMBER_LOCK)
        .bind(MEMBER)
        .fetch_one(f.db.observe(&mut *blocker))
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let request = trace::scope(
        profile.clone(),
        Box::pin(f.api(
            "POST",
            "/api/v1/threads/103/replies",
            Some(&secret),
            Some(r#"{"body":"Reply whose token is revoked while it waits"}"#),
            &[],
        )),
    );
    let revoke = async {
        wait_blocked(&profile, MEMBER_LOCK).await;
        // Same as `revoke_api_key`: revoke all of the member's tokens inside the member row lock, then commit.
        db::query(
            "UPDATE gonof_common_member_api_key SET revoked_at=GREATEST(dateline, ?), revoked_by=?
             WHERE uid=? AND revoked_at IS NULL",
        )
        .bind(gono_forum_web::identity::now())
        .bind(MEMBER)
        .bind(MEMBER)
        .execute(f.db.observe(&mut *blocker))
        .await
        .unwrap();
        blocker.commit().await.unwrap();
    };
    let ((status, _, body), ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(request, revoke)
    })
    .await
    .expect("the waiting request must finish once the revocation commits");
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        f.scalar(&posts).await,
        before,
        "no reply is written after the revocation"
    );
    f.services.cleanup_ref().await.unwrap();
}

/// Two requests with the same Idempotency-Key in flight at once: only one write, one gets 201 and the other gets
/// 200 from replay.
#[tokio::test]
async fn concurrent_requests_with_one_idempotency_key_create_once() {
    // Only verifies the MySQL row-lock wait orchestration.
    gono_forum_web::mysql_only!();
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    let posts = format!("SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={MEMBER}");
    let before = f.scalar(&posts).await;
    let mut blocker = f.db.begin_write().await.unwrap();
    db::query(MEMBER_LOCK)
        .bind(MEMBER)
        .fetch_one(f.db.observe(&mut *blocker))
        .await
        .unwrap();
    let key = [("idempotency-key", "concurrent-same-key")];
    let body = Some(r#"{"body":"Reply sent twice at once with one key"}"#);
    let (first, second) = (
        Arc::new(RequestProfile::new()),
        Arc::new(RequestProfile::new()),
    );
    let path = "/api/v1/threads/103/replies";
    let a = trace::scope(
        first.clone(),
        Box::pin(f.api("POST", path, Some(&secret), body, &key)),
    );
    let b = trace::scope(
        second.clone(),
        Box::pin(f.api("POST", path, Some(&secret), body, &key)),
    );
    let release = async {
        // Release only once both requests have passed all reads outside the transaction and are queuing for the
        // member row lock.
        wait_blocked(&first, MEMBER_LOCK).await;
        wait_blocked(&second, MEMBER_LOCK).await;
        blocker.rollback().await.unwrap();
    };
    let ((status_a, _, a), (status_b, _, b), ()) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(a, b, release)
        })
        .await
        .expect("both requests must finish once the member row is released");
    let mut statuses = [status_a.as_u16(), status_b.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 201], "{a} / {b}");
    assert_eq!(a["pid"], b["pid"]);
    assert_eq!(f.scalar(&posts).await, before + 1);
    f.services.cleanup_ref().await.unwrap();
}

/// All four write entry points that lock member rows (publish and reply, edit, interaction, activity) re-check the
/// token, not relying only on the request entry.
#[tokio::test]
async fn every_write_transaction_rechecks_the_api_key() {
    let f = Fixture::new(&[]).await;
    let (grant, _) = f.key(MEMBER).await;
    let key_id = f
        .scalar(&format!(
            "SELECT id FROM gonof_common_member_api_key WHERE uid={MEMBER} AND revoked_at IS NULL"
        ))
        .await;
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let reply = |body: &str| NewReply {
        tid: 103,
        root_pid: None,
        reply_to_pid: None,
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    };
    let cookie_actor = f.actor(MEMBER).await;
    let own = writer
        .reply_actor(
            cookie_actor,
            reply("Reply written as the web identity before the revocation"),
        )
        .await
        .unwrap();
    let api_actor = identity::Actor {
        api_key: Some(key_id),
        ..cookie_actor
    };
    // Control: with a valid token the same identity can write.
    writer
        .reply_actor(api_actor, reply("Reply while the token is valid"))
        .await
        .unwrap();
    assert!(
        f.identity
            .revoke_api_key(&grant.session_token)
            .await
            .unwrap()
    );
    let writes = format!(
        "SELECT CAST((SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={MEMBER})
              + (SELECT COUNT(*) FROM gonof_home_favorite WHERE uid={MEMBER})
              + (SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE uid={MEMBER})
              + (SELECT COALESCE(SUM(version),0) FROM gonof_forum_post WHERE authorid={MEMBER}) AS SIGNED)"
    );
    let before = f.scalar(&writes).await;

    assert!(matches!(
        writer
            .reply_actor(api_actor, reply("Reply after the revocation"))
            .await,
        Err(ContentWriteError::Unauthorized)
    ));
    let versions = db::query(
        "SELECT p.version, t.version AS thread_version FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=?",
    )
    .bind(own.pid)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    let edit = ContentEditor::new(f.db.clone(), WriteConfig::default(), 86_400)
        .edit(
            api_actor,
            EditRequest {
                visibility: None,
                pid: own.pid,
                expected_post_version: versions.try_get("version").unwrap(),
                expected_thread_version: versions.try_get("thread_version").unwrap(),
                title: None,
                body: Some("Edit after the revocation".into()),
                request_key: identity::random_token(),
                reason: String::new(),
            },
        )
        .await;
    assert!(matches!(edit, Err(ContentWriteError::Unauthorized)));
    assert!(matches!(
        Interaction::new(f.db.clone())
            .set_favorite(api_actor, 103, true, &identity::random_token())
            .await,
        Err(ContentWriteError::Unauthorized)
    ));
    let check_in = activities::check_in_live(&f.db, api_actor).await;
    assert_eq!(
        check_in
            .unwrap_err()
            .downcast_ref::<activities::ActivityFailure>()
            .map(|failure| failure.0.as_str()),
        Some(Lang::EN.activity_error_api_key())
    );
    assert_eq!(
        f.scalar(&writes).await,
        before,
        "none of the four entry points writes after the revocation"
    );
    f.services.cleanup_ref().await.unwrap();
}

/// Lottery extensions via the API: publish a one-click sign-up lottery with prizes, participation requirements and
/// redemption codes, then sign up after confirming each item; the activity card JSON has the prizes and the
/// member's own result but no redemption codes.
#[tokio::test]
async fn lottery_prizes_and_one_click_entry_through_the_api() {
    let f = Fixture::new(&[("settings.api.writes_per_minute", "60")]).await;
    let now = identity::now();
    gono_forum_web::activities::adjust(
        &f.db,
        f.actor(ADMIN).await,
        MEMBER,
        100,
        &identity::random_token(),
        "Test budget",
        now,
    )
    .await
    .unwrap();
    let (_, secret) = f.key(MEMBER).await;
    let (_, other) = f.key(OTHER).await;
    let deadline = now + 3600;
    let body = format!(
        r#"{{"forum":"daily","title":"API lottery","body":"Body","category":3,"deadline":{deadline},"entry_mode":"click","requirements":["Follow the author"],"review":true,"early_draw":true,"prizes":[{{"name":"Member code","winners":1,"kind":"code","codes":["API-SECRET-CODE-55"]}},{{"name":"Coins","winners":2,"kind":"credit","amount":20}}]}}"#
    );
    let unknown = body.replace(r#""kind":"code","#, r#""kind":"code","gift":1,"#);
    let (status, _, _) = f
        .api(
            "POST",
            "/api/v1/threads",
            Some(&secret),
            Some(&unknown),
            &[],
        )
        .await;
    assert!(
        status.is_client_error(),
        "an unknown field in a prize is rejected"
    );
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&secret), Some(&body), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let tid = created["tid"].as_i64().unwrap();
    assert_eq!(
        f.scalar(&format!(
            "SELECT frozen FROM gonof_common_credit_account WHERE uid={MEMBER}"
        ))
        .await,
        40
    );
    let enter = format!("/api/v1/threads/{tid}/enter");
    let (status, _, rejected) = f.api("POST", &enter, Some(&other), Some("{}"), &[]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert_eq!(rejected["code"], "activity_rejected", "{rejected}");
    let (status, _, entered) = f
        .api(
            "POST",
            &enter,
            Some(&other),
            Some(r#"{"confirm":[1]}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{entered}");
    assert_eq!(entered["entered"], true);
    let (_, _, again) = f
        .api(
            "POST",
            &enter,
            Some(&other),
            Some(r#"{"confirm":[1]}"#),
            &[],
        )
        .await;
    assert_eq!(again["entered"], false);
    gono_forum_web::activities::draw_now(&f.db, f.actor(MEMBER).await, tid, identity::now())
        .await
        .unwrap();
    gono_forum_web::activities::process_due(&f.db, identity::now() + 5, 20)
        .await
        .unwrap();
    let (status, _, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{tid}/activity"),
            Some(&other),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    assert_eq!(card["entry_mode"], "click");
    assert_eq!(card["prizes"].as_array().unwrap().len(), 2);
    assert_eq!(card["my_prize"], "Member code");
    assert_eq!(card["state"], "settled");
    assert!(!card.to_string().contains("API-SECRET-CODE-55"));
    assert_eq!(
        f.scalar(&format!(
            "SELECT available FROM gonof_common_credit_account WHERE uid={MEMBER}"
        ))
        .await,
        100,
        "both points prizes went unclaimed and are refunded in full"
    );
}

/// The activity card's flags only offer what the vote transaction would accept: a muted member, or one who no longer
/// meets the participation requirements, is not offered a vote (docs/api.md, "Account and site rules").
#[tokio::test]
async fn activity_flags_follow_mutes_and_participation_requirements() {
    let f = Fixture::new(&[]).await;
    let (_, owner) = f.key(ADMIN).await;
    let (_, secret) = f.key(MEMBER).await;
    let deadline = (chrono::DateTime::from_timestamp(identity::now() + 86_400, 0).unwrap()
        + chrono::Duration::hours(8))
    .format("%Y-%m-%dT%H:%M")
    .to_string();
    let poll = format!(
        r#"{{"forum":"daily","title":"Flags poll","body":"Pick one","category":4,"options":["Alpha","Beta"],"max_choices":1,"deadline":"{deadline}"}}"#
    );
    let (status, _, created) = f
        .api("POST", "/api/v1/threads", Some(&owner), Some(&poll), &[])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let path = format!("/api/v1/threads/{}/activity", created["tid"]);
    let vote = format!("/api/v1/threads/{}/vote", created["tid"]);
    let (_, _, card) = f.api("GET", &path, Some(&secret), None, &[]).await;
    assert_eq!(card["can_vote"], true, "{card}");

    f.execute(&format!(
        "UPDATE gonof_common_member SET mute_until={} WHERE uid={MEMBER}",
        identity::now() + 3600
    ))
    .await;
    let (_, _, card) = f.api("GET", &path, Some(&secret), None, &[]).await;
    assert_eq!(
        card["can_vote"], false,
        "a muted member cannot vote: {card}"
    );
    let (status, _, body) = f
        .api(
            "POST",
            &vote,
            Some(&secret),
            Some(r#"{"choices":[1]}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    f.execute(&format!(
        "UPDATE gonof_common_member SET mute_until=0,emailstatus=0 WHERE uid={MEMBER}"
    ))
    .await;
    let (_, _, card) = f.api("GET", &path, Some(&secret), None, &[]).await;
    assert_eq!(
        card["can_vote"], false,
        "a member without a verified email cannot take part: {card}"
    );
    let (status, _, body) = f
        .api(
            "POST",
            &vote,
            Some(&secret),
            Some(r#"{"choices":[1]}"#),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    f.services.cleanup_ref().await.unwrap();
}

/// The website still allows favorites and the check-in in read-only mode, but the API refuses every write then. When
/// read-only mode takes effect after the request passed the API's entry check (here: the process cache still holds
/// the old settings), the write transaction itself refuses it.
#[tokio::test]
async fn read_only_mode_reached_after_the_entry_check_still_refuses_api_favorites_and_check_ins() {
    let f = Fixture::new(&[]).await;
    let (_, secret) = f.key(MEMBER).await;
    f.execute(
        "UPDATE gonof_common_credit_rule SET enabled=1,amount=1,daily_limit=1 WHERE rule_key='signin'",
    )
    .await;
    let tid = f
        .scalar(&format!(
            "SELECT tid FROM gonof_forum_post WHERE pid={}",
            f.first_pid
        ))
        .await;
    // Warm the process cache, then switch the database alone to read-only.
    let (status, ..) = f.api("GET", "/api/v1/me", Some(&secret), None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    f.execute(
        "UPDATE gonof_common_settings SET svalue='true' WHERE skey='settings.site.read_only'",
    )
    .await;
    let (status, _, body) = f
        .api("POST", "/api/v1/check-in", Some(&secret), None, &[])
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("read_only")),
        "{body}"
    );
    let (status, _, body) = f
        .api(
            "PUT",
            &format!("/api/v1/threads/{tid}/favorite"),
            Some(&secret),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("read_only")),
        "{body}"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE uid={MEMBER} AND kind='signin'"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_favorite WHERE uid={MEMBER}"
        ))
        .await,
        0
    );
    f.services.cleanup_ref().await.unwrap();
}
