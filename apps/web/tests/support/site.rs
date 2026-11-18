//! Integration tests only: a site with the fixture boards, an administrator (uid 1), a moderator (uid 2) and a member
//! (uid 3), each signed in, and requests with their session and CSRF token.
#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content,
    db::{self, Database},
    identity, router,
    test_support::{self, TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[path = "http_auth.rs"]
mod http_auth;

pub struct Site {
    pub services: TestFixture,
    pub app: Router,
    /// `(session token, CSRF token)` of each signed-in member.
    pub admin: (String, String),
    pub moderator: (String, String),
    pub member: (String, String),
}

pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Answer {
    pub fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .map(|value| value.to_str().unwrap())
            .unwrap_or_default()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| panic!("{error}: {}", self.body))
    }
}

impl Site {
    /// The fixture boards and members; with `fixture`, also the development fixture's threads (whose authors are
    /// other members). `prepare` runs on the database before the router is built, so its caches see the changes.
    pub async fn new(fixture: bool) -> Self {
        Self::prepared(fixture, |_| async {}).await
    }

    pub async fn prepared<F, Fut>(fixture: bool, prepare: F) -> Self
    where
        F: FnOnce(Database) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        if fixture {
            content::seed_fixture(&db).await.unwrap();
        } else {
            content::seed_forums(&db).await.unwrap();
        }
        let first: i64 = db::query_scalar("SELECT COALESCE(MAX(uid),0) FROM gonof_common_member")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let mut uids = Vec::new();
        for (offset, role) in [(1, 1), (2, 3), (3, 0)] {
            let uid = first + offset;
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'password-hash',?,1,?,1)")
                .bind(uid)
                .bind(format!("tester-{offset}"))
                .bind(format!("tester-{offset}@example.test"))
                .bind(role)
                .execute(db.executor())
                .await
                .unwrap();
            uids.push(uid);
        }
        prepare(db.clone()).await;
        let identity = identity::Identity::new(
            db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            test_support::detached_global(),
        );
        let mut grants = Vec::new();
        for uid in uids {
            let grant = http_auth::login(&identity, &db, uid).await;
            grants.push((grant.session_token, grant.csrf_token));
        }
        let app = router::build_with_global(
            services.config.clone(),
            &services.root,
            db,
            services.cache.clone(),
            services.global.clone(),
        );
        let member = grants.pop().unwrap();
        let moderator = grants.pop().unwrap();
        let admin = grants.pop().unwrap();
        Self {
            services,
            app,
            admin,
            moderator,
            member,
        }
    }

    pub fn db(&self) -> &Database {
        &self.services.db
    }

    /// Clears the process caches, after a test changed cached data with raw SQL.
    pub fn changed(&self) {
        self.services.global.clear_local();
    }

    pub async fn send(&self, request: Request<Body>) -> Answer {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        Answer {
            status,
            headers,
            body,
        }
    }

    fn request(
        method: &str,
        path: &str,
        session: Option<&(String, String)>,
    ) -> axum::http::request::Builder {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, test_support::host_of(TEST_SITE_URL));
        if let Some((token, _)) = session {
            request = request.header(header::COOKIE, format!("gonof_session__port58833={token}"));
        }
        if method == "POST" {
            request = request.header(header::ORIGIN, TEST_SITE_URL);
        }
        request
    }

    pub async fn get(&self, path: &str, session: Option<&(String, String)>) -> Answer {
        self.send(
            Self::request("GET", path, session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// A GET with extra headers.
    pub async fn get_with(
        &self,
        path: &str,
        session: Option<&(String, String)>,
        headers: &[(&str, &str)],
    ) -> Answer {
        let mut request = Self::request("GET", path, session);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    /// A form POST with the session's CSRF token first.
    pub async fn post(
        &self,
        path: &str,
        session: &(String, String),
        fields: &[(&str, &str)],
    ) -> Answer {
        let mut all: Vec<(&str, &str)> = vec![("_csrf", session.1.as_str())];
        all.extend_from_slice(fields);
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(all)
            .finish();
        self.send(
            Self::request("POST", path, Some(session))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    pub async fn text(&self, sql: &str) -> String {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    pub async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db().executor())
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    pub async fn fid(&self, slug: &str) -> i64 {
        self.scalar(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"
        ))
        .await
    }
}
