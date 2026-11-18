//! HTTP contracts for activity forms, private balances and anonymous ballots.
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    identity::{self, Identity},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";

struct Fixture {
    services: TestFixture,
    app: Router,
    admin: String,
    member: String,
    fid: String,
    csrf_tokens: std::collections::BTreeMap<String, String>,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let config = services.config.clone();
        gono_forum_web::test_support::set_setting(
            &services.db,
            "settings.posting.daily_threads",
            "20",
        )
        .await
        .unwrap();
        content::seed_forums(&services.db).await.unwrap();
        let identity = Identity::new(
            services.db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let password = admin_password.as_str();
        let mut csrf_tokens = std::collections::BTreeMap::new();
        let admin = identity.login("admin", password).await.unwrap();
        csrf_tokens.insert(admin.session_token.clone(), admin.csrf_token);
        let admin = admin.session_token;
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate) SELECT 2,'private-voter','private-voter@example.test',password,1,regdate-172800 FROM gonof_common_member WHERE uid=1")
            .execute(services.db.executor()).await.unwrap();
        let member = identity.login("private-voter", password).await.unwrap();
        csrf_tokens.insert(member.session_token.clone(), member.csrf_token);
        let member = member.session_token;
        let fid: i64 = db::query_scalar(
            "SELECT MIN(fid) FROM gonof_forum_forum WHERE visibility='public' AND status=1",
        )
        .fetch_one(services.db.executor())
        .await
        .unwrap();
        let app = router::build(config, &services.root, services.db.clone());
        Self {
            services,
            app,
            admin,
            member,
            fid: fid.to_string(),
            csrf_tokens,
        }
    }
    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.services.db.executor())
            .await
            .unwrap()
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        fields: &[(&str, &str)],
        csrf: bool,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::COOKIE, format!("gonof_session__port58833={token}"));
        }
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.extend_pairs(fields.iter().copied());
        if method == "POST" {
            request = request
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN));
            if csrf {
                form.append_pair("_csrf", &self.csrf_tokens[token.unwrap()].clone());
            } else {
                form.append_pair("_csrf", "invalid-csrf");
            }
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(form.finish())).unwrap())
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
        Response {
            status,
            headers,
            html,
        }
    }
    async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.request("GET", path, token, &[], false).await
    }
    async fn post(&self, path: &str, token: &str, fields: &[(&str, &str)]) -> Response {
        self.request("POST", path, Some(token), fields, true).await
    }
}
struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}
impl Response {
    fn status(&self, expected: StatusCode) {
        assert_eq!(self.status, expected, "{}", self.html);
    }
    fn redirect(&self, expected: &str) {
        self.status(StatusCode::SEE_OTHER);
        assert_eq!(self.headers[header::LOCATION], expected);
    }
    fn card(&self) -> &str {
        self.html
            .split("data-activity-kind=")
            .nth(1)
            .unwrap()
            .split("</section>")
            .next()
            .unwrap()
    }
}
fn hidden_value<'a>(html: &'a str, name: &str) -> &'a str {
    let marker = format!("name=\"{name}\" value=\"");
    html.split(&marker)
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
}

#[tokio::test]
async fn credits_require_login_csrf_and_admin_while_signin_is_idempotent() {
    let f = Fixture::new().await;
    f.get("/my/credits", None)
        .await
        .status(StatusCode::SEE_OTHER);
    f.get("/manage/credits", Some(&f.member))
        .await
        .status(StatusCode::FORBIDDEN);
    let initial = f.get("/my/credits", Some(&f.member)).await;
    initial.status(StatusCode::OK);
    assert!(initial.html.contains("Check-in rewards are not turned on."));
    let key = identity::random_token();
    let adjustment = [
        ("uid", "2"),
        ("amount", "100"),
        ("reason", "HTTP private ledger check"),
        ("request_key", key.as_str()),
    ];
    f.request(
        "POST",
        "/manage/credits/adjust",
        Some(&f.admin),
        &adjustment,
        false,
    )
    .await
    .status(StatusCode::FORBIDDEN);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_ledger")
            .await,
        0
    );
    f.post("/manage/credits/adjust", &f.member, &adjustment)
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    f.post("/manage/credits/adjust", &f.admin, &adjustment)
        .await
        .redirect("/manage/credits?done=saved");
    // The redirect target must be a real admin page and show the saved notice.
    let saved = f.get("/manage/credits?done=saved", Some(&f.admin)).await;
    saved.status(StatusCode::OK);
    assert!(saved.html.contains("Saved."));
    f.post("/manage/credits/adjust", &f.admin, &adjustment)
        .await
        .status(StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar("SELECT available FROM gonof_common_credit_account WHERE uid=2")
            .await,
        100
    );
    assert!(
        f.get("/my/credits", Some(&f.member))
            .await
            .html
            .contains("HTTP private ledger check")
    );
    assert!(
        !f.get("/my/credits", Some(&f.admin))
            .await
            .html
            .contains("HTTP private ledger check")
    );

    let version = f
        .scalar("SELECT version FROM gonof_common_credit_rule WHERE rule_key='signin'")
        .await
        .to_string();
    let rule = [
        ("key", "signin"),
        ("enabled", "true"),
        ("amount", "2"),
        ("daily_limit", "2"),
        ("version", version.as_str()),
    ];
    f.post("/manage/credits/rules", &f.admin, &rule)
        .await
        .redirect("/manage/credits?done=saved");
    f.post("/manage/credits/rules", &f.admin, &rule)
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    let signup = f.get("/my/credits", Some(&f.member)).await;
    assert!(signup.html.contains("action=\"/api/check-in\""));
    // The check-in form only carries CSRF, no request key any more; a repeated submission on the same day does not
    // award again.
    let fields: [(&str, &str); 0] = [];
    assert!(!signup.html.contains("name=\"request_key\""));
    f.request("POST", "/api/check-in", Some(&f.member), &fields, false)
        .await
        .status(StatusCode::FORBIDDEN);
    f.post("/api/check-in", &f.member, &fields)
        .await
        .redirect("/my/credits?done=signin");
    f.post("/api/check-in", &f.member, &fields)
        .await
        .status(StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar("SELECT available FROM gonof_common_credit_account WHERE uid=2")
            .await,
        102
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE kind='signin'")
            .await,
        1
    );
    assert!(
        f.get("/my/credits", Some(&f.member))
            .await
            .html
            .contains("You have checked in today")
    );
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn ordinary_members_publish_all_six_categories_and_ballots_stay_private() {
    let f = Fixture::new().await;
    let key = identity::random_token();
    f.post(
        "/manage/credits/adjust",
        &f.admin,
        &[
            ("uid", "2"),
            ("amount", "100"),
            ("reason", "activity form budget"),
            ("request_key", &key),
        ],
    )
    .await
    .status(StatusCode::SEE_OTHER);
    let editor = f.get("/new", Some(&f.member)).await;
    editor.status(StatusCode::OK);
    for label in [
        "Discussion",
        "Bounty",
        "Red packet",
        "Lottery",
        "Poll",
        "News",
    ] {
        assert!(
            editor.html.contains(&format!("<span>{label}</span>")),
            "{label}"
        );
    }
    let deadline = chrono::DateTime::<chrono::Utc>::from_timestamp(identity::now() + 9 * 3_600, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    let mut poll_tid = 0;
    let mut bounty_tid = 0;
    for category in 0..=5 {
        let category_text = category.to_string();
        let key = identity::random_token();
        let fields = [
            ("fid", f.fid.as_str()),
            ("category", category_text.as_str()),
            ("title", "Six types form test"),
            ("body", "Activity form body"),
            ("request_key", key.as_str()),
            ("budget", "12"),
            ("shares", "3"),
            ("distribution", "equal"),
            ("deadline", deadline.as_str()),
            ("max_choices", "2"),
            ("options", "Alpha\nBeta\nGamma"),
            ("source_url", "https://example.test/news"),
            ("source_name", "News source"),
            ("source_author", "Sample Author"),
        ];
        f.post("/new", &f.member, &fields)
            .await
            .status(StatusCode::SEE_OTHER);
        f.post("/new", &f.member, &fields)
            .await
            .status(StatusCode::SEE_OTHER);
        let tid = f.scalar("SELECT MAX(tid) FROM gonof_forum_thread").await;
        assert_eq!(
            f.scalar(&format!(
                "SELECT category FROM gonof_forum_thread WHERE tid={tid}"
            ))
            .await,
            category
        );
        let page = f.get(&format!("/thread-{tid}"), Some(&f.member)).await;
        page.status(StatusCode::OK);
        if category == 0 {
            assert!(!page.html.contains("data-activity-kind="));
        } else {
            assert!(
                page.html
                    .contains(&format!("data-activity-kind=\"{category}\""))
            );
        }
        if category == 4 {
            poll_tid = tid;
        }
        if category == 1 {
            bounty_tid = tid;
        }
    }
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await, 6);
    assert_eq!(
        f.scalar("SELECT frozen FROM gonof_common_credit_account WHERE uid=2")
            .await,
        36
    );
    let path = format!("/thread-{poll_tid}");
    let vote_path = format!("/api/thread-{poll_tid}/vote");
    let page = f.get(&path, Some(&f.member)).await;
    assert!(page.card().contains("name=\"choice_1\""));
    let vote_key = hidden_value(page.card(), "request_key");
    assert_eq!(vote_key.len(), 64);
    assert_eq!(
        hidden_value(page.card(), "_csrf"),
        f.csrf_tokens[&f.member].clone()
    );
    let vote = [
        ("request_key", vote_key),
        ("choice_1", "1"),
        ("choice_3", "1"),
    ];
    f.request("POST", &vote_path, Some(&f.member), &vote, false)
        .await
        .status(StatusCode::FORBIDDEN);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_poll_vote").await,
        0
    );
    f.post(&vote_path, &f.member, &vote).await.redirect(&path);
    f.post(&vote_path, &f.member, &vote).await.redirect(&path);
    f.post(
        &vote_path,
        &f.member,
        &[
            ("request_key", &identity::random_token()),
            ("choice_2", "1"),
        ],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    let mine = f.get(&path, Some(&f.member)).await;
    assert!(mine.card().contains("You have voted."));
    assert!(!mine.card().contains("<form"));
    for token in [None, Some(f.admin.as_str())] {
        let other = f.get(&path, token).await;
        other.status(StatusCode::OK);
        assert!(other.card().contains("1 voter · "));
        assert!(other.card().contains("Alpha: 1 vote"));
        assert!(other.card().contains("Gamma: 1 vote"));
        assert!(!other.card().contains("You have voted."));
        assert!(!other.card().contains("private-voter"));
    }
    assert_eq!(f.scalar("SELECT voters FROM gonof_forum_poll").await, 1);
    f.post(
        &format!("/api/thread-{bounty_tid}/comments"),
        &f.admin,
        &[
            ("body", "A reply that can be accepted"),
            ("request_key", &identity::random_token()),
        ],
    )
    .await
    .status(StatusCode::SEE_OTHER);
    let target = f
        .scalar(&format!(
            "SELECT MAX(pid) FROM gonof_forum_post WHERE tid={bounty_tid} AND first=0"
        ))
        .await
        .to_string();
    let bounty_page = f
        .get(&format!("/thread-{bounty_tid}"), Some(&f.member))
        .await;
    assert!(bounty_page.html.contains("Accept this reply"));
    // The deadline is compared with the current time in code: past the deadline the accept button is no longer offered.
    let deadline = f
        .scalar(&format!(
            "SELECT deadline FROM gonof_forum_activity WHERE tid={bounty_tid}"
        ))
        .await;
    db::query("UPDATE gonof_forum_activity SET deadline=1 WHERE tid=?")
        .bind(bounty_tid)
        .execute(f.services.db.executor())
        .await
        .unwrap();
    let expired_page = f
        .get(&format!("/thread-{bounty_tid}"), Some(&f.member))
        .await;
    assert!(!expired_page.html.contains("Accept this reply"));
    db::query("UPDATE gonof_forum_activity SET deadline=? WHERE tid=?")
        .bind(deadline)
        .bind(bounty_tid)
        .execute(f.services.db.executor())
        .await
        .unwrap();
    let accept_path = format!("/api/thread-{bounty_tid}/accept");
    let accept_form_marker = format!("action=\"{accept_path}\"");
    let accept_form = bounty_page
        .html
        .split(&accept_form_marker)
        .nth(1)
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    let accept_key = hidden_value(accept_form, "request_key");
    assert_eq!(accept_key.len(), 64);
    assert_eq!(hidden_value(accept_form, "pid"), target);
    let fields = [("pid", target.as_str()), ("request_key", accept_key)];
    f.request("POST", &accept_path, Some(&f.member), &fields, false)
        .await
        .status(StatusCode::FORBIDDEN);
    f.post(&accept_path, &f.member, &fields)
        .await
        .redirect(&format!("/thread-{bounty_tid}"));
    f.post(&accept_path, &f.member, &fields)
        .await
        .status(StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar("SELECT available FROM gonof_common_credit_account WHERE uid=1")
            .await,
        12
    );
    assert_eq!(
        f.scalar("SELECT frozen FROM gonof_common_credit_account WHERE uid=2")
            .await,
        24
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A vote does not bump the thread's `cache_revision` or the site-wide content version (the version assertion checks
/// this). Activity cards are read fresh on every request and are not in any shared cache, so even though the floor
/// and render cache keys do not change at all, anonymous visitors see the new vote count immediately.
#[tokio::test]
async fn votes_reach_cached_anonymous_pages_without_bumping_cache_versions() {
    let f = Fixture::new().await;
    let deadline = chrono::DateTime::<chrono::Utc>::from_timestamp(identity::now() + 9 * 3_600, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    let key = identity::random_token();
    f.post(
        "/new",
        &f.member,
        &[
            ("fid", f.fid.as_str()),
            ("category", "4"),
            ("title", "Poll in the cache"),
            ("body", "Poll body"),
            ("request_key", key.as_str()),
            ("deadline", deadline.as_str()),
            ("max_choices", "1"),
            ("options", "Alpha\nBeta"),
        ],
    )
    .await
    .status(StatusCode::SEE_OTHER);
    let tid = f.scalar("SELECT MAX(tid) FROM gonof_forum_thread").await;
    let path = format!("/thread-{tid}");
    // Visit anonymously twice: the first goes to the source and writes the floor and render caches, the second hits.
    for _ in 0..2 {
        let page = f.get(&path, None).await;
        page.status(StatusCode::OK);
        assert!(page.card().contains("0 voters · "));
    }
    let versions = format!(
        "SELECT cache_revision * 1000000 + (SELECT revision FROM gonof_common_content_revision WHERE id=1) FROM gonof_forum_thread WHERE tid={tid}"
    );
    let before = f.scalar(&versions).await;
    let ballot = f.get(&path, Some(&f.member)).await;
    let vote_key = hidden_value(ballot.card(), "request_key").to_owned();
    f.post(
        &format!("/api/thread-{tid}/vote"),
        &f.member,
        &[("request_key", vote_key.as_str()), ("choice_1", "1")],
    )
    .await
    .redirect(&path);
    assert_eq!(f.scalar("SELECT voters FROM gonof_forum_poll").await, 1);
    assert_eq!(
        f.scalar(&versions).await,
        before,
        "a vote must not bump the thread version or the site content version"
    );
    let anonymous = f.get(&path, None).await;
    anonymous.status(StatusCode::OK);
    assert!(
        anonymous.card().contains("1 voter · "),
        "{}",
        anonymous.card()
    );
    assert!(anonymous.card().contains("Alpha: 1 vote"));
    f.services.cleanup_ref().await.unwrap();
}
