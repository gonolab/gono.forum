//! Web forms for lottery extensions: prizes and redemption codes when posting, one-click entry with item-by-item
//! confirmation, the review page, early drawing, the redemption code page and "My prizes".
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities, content, db,
    identity::{self, Identity},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const CODE: &str = "HTTP-SECRET-CODE-91";

struct Fixture {
    services: TestFixture,
    app: Router,
    admin: String,
    author: String,
    entrant: String,
    other: String,
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
        for (uid, name) in [
            (2, "lottery-author"),
            (3, "lottery-entrant"),
            (4, "lottery-other"),
        ] {
            db::query("INSERT INTO gonof_common_member(uid,username,email,password,emailstatus,regdate) SELECT ?,?,?,password,1,regdate-172800 FROM gonof_common_member WHERE uid=1")
                .bind(uid).bind(name).bind(format!("{name}@example.test"))
                .execute(services.db.executor()).await.unwrap();
        }
        let mut csrf_tokens = std::collections::BTreeMap::new();
        let mut sessions = Vec::new();
        for name in [
            "admin",
            "lottery-author",
            "lottery-entrant",
            "lottery-other",
        ] {
            let grant = identity.login(name, password).await.unwrap();
            csrf_tokens.insert(grant.session_token.clone(), grant.csrf_token);
            sessions.push(grant.session_token);
        }
        let [admin, author, entrant, other]: [String; 4] = sessions.try_into().unwrap();
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
            author,
            entrant,
            other,
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
            form.append_pair("_csrf", &self.csrf_tokens[token.unwrap()].clone());
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(form.finish())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Response {
            status,
            headers,
            html: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }
    async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.request("GET", path, token, &[]).await
    }
    async fn post(&self, path: &str, token: &str, fields: &[(&str, &str)]) -> Response {
        self.request("POST", path, Some(token), fields).await
    }
}
struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}
impl Response {
    fn status(&self, expected: StatusCode) -> &Self {
        assert_eq!(self.status, expected, "{}", self.html);
        self
    }
    fn redirect(&self, expected: &str) {
        self.status(StatusCode::SEE_OTHER);
        assert_eq!(self.headers[header::LOCATION], expected);
    }
}

#[tokio::test]
async fn lottery_forms_enter_review_draw_and_reveal_codes_only_to_the_winner() {
    let f = Fixture::new().await;
    let db = &f.services.db;
    activities::adjust(
        db,
        identity::Actor {
            uid: 1,
            credentials_version: 1,
            api_key: None,
        },
        2,
        100,
        &identity::random_token(),
        "HTTP lottery budget",
        identity::now(),
    )
    .await
    .unwrap();
    let deadline = chrono::DateTime::<chrono::Utc>::from_timestamp(identity::now() + 9 * 3_600, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    // The editor has a redemption code field, but redemption codes do not go into the browser's local draft.
    let editor = f.get(&format!("/new?fid={}", f.fid), Some(&f.author)).await;
    assert!(editor.html.contains("name=\"lottery_codes\""));
    let script = std::fs::read_to_string(
        gono_forum_web::project_root().join("apps/web/frontend/composer.js"),
    )
    .unwrap();
    let fields_line = script
        .lines()
        .find(|l| l.starts_with("const ACTIVITY_FIELDS"))
        .unwrap();
    assert!(fields_line.contains("'prizes'") && !fields_line.contains("lottery_codes"));

    let prizes = "Member code | 1 | Code | | Annual membership\nCoins | 1 | Coins | 10";
    let base = |key: &str, prizes: &str, codes: &str| {
        vec![
            ("fid".to_owned(), f.fid.clone()),
            ("category".to_owned(), "3".to_owned()),
            ("title".to_owned(), "Web lottery".to_owned()),
            ("body".to_owned(), "Web lottery body".to_owned()),
            ("request_key".to_owned(), key.to_owned()),
            ("deadline".to_owned(), deadline.clone()),
            ("entry_mode".to_owned(), "click".to_owned()),
            (
                "requirements".to_owned(),
                "Follow the author\nShare this thread".to_owned(),
            ),
            ("review".to_owned(), "1".to_owned()),
            ("early_draw".to_owned(), "1".to_owned()),
            ("prizes".to_owned(), prizes.to_owned()),
            ("lottery_codes".to_owned(), codes.to_owned()),
        ]
    };
    let pairs = |list: &[(String, String)]| -> Vec<(String, String)> { list.to_vec() };
    let key = identity::random_token();
    let bad = pairs(&base(&key, "Member code | one | Code", CODE));
    let refs: Vec<(&str, &str)> = bad.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let rejected = f.post("/new", &f.author, &refs).await;
    rejected.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        rejected
            .html
            .contains("The winners of prize 1 must be a number")
    );
    let missing = pairs(&base(&key, prizes, ""));
    let refs: Vec<(&str, &str)> = missing
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let rejected = f.post("/new", &f.author, &refs).await;
    rejected.status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        rejected
            .html
            .contains("A code prize needs exactly one code per winner")
    );
    assert_eq!(f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await, 0);
    let good = pairs(&base(&key, prizes, CODE));
    let refs: Vec<(&str, &str)> = good.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    f.post("/new", &f.author, &refs)
        .await
        .status(StatusCode::SEE_OTHER);
    let tid = f.scalar("SELECT MAX(tid) FROM gonof_forum_thread").await;
    assert_eq!(
        f.scalar("SELECT frozen FROM gonof_common_credit_account WHERE uid=2")
            .await,
        10
    );
    let topic = format!("/thread-{tid}");
    for token in [None, Some(f.author.as_str()), Some(f.entrant.as_str())] {
        let page = f.get(&topic, token).await;
        page.status(StatusCode::OK);
        assert!(
            page.html
                .contains("Member code · 1 winner · Redemption code · Annual membership")
        );
        assert!(!page.html.contains(CODE), "the thread page holds no code");
    }
    let page = f.get(&topic, Some(&f.entrant)).await;
    assert!(page.html.contains("name=\"confirm_1\"") && page.html.contains("name=\"confirm_2\""));
    assert!(!page.html.contains("data-lottery-draw"));
    let author_page = f.get(&topic, Some(&f.author)).await;
    assert!(author_page.html.contains("data-lottery-draw"));
    assert!(
        author_page
            .html
            .contains(&format!("/threads/{tid}/lottery/codes"))
    );

    // One-click entry: missing one confirmation is rejected.
    let enter = format!("/api/thread-{tid}/enter");
    f.post(&enter, &f.entrant, &[("confirm_1", "1")])
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    f.post(
        &enter,
        &f.entrant,
        &[("confirm_1", "1"), ("confirm_2", "1")],
    )
    .await
    .redirect(&topic);
    f.post(&enter, &f.other, &[("confirm_1", "1"), ("confirm_2", "1")])
        .await
        .redirect(&topic);
    assert!(
        f.get(&topic, Some(&f.entrant))
            .await
            .html
            .contains("You have entered.")
    );

    // Review page: the author can remove entries, administrators can only view, others cannot see it.
    let review = format!("/threads/{tid}/lottery");
    f.get(&review, Some(&f.entrant))
        .await
        .status(StatusCode::NOT_FOUND);
    let admin_review = f.get(&review, Some(&f.admin)).await;
    admin_review.status(StatusCode::OK);
    assert!(
        admin_review.html.contains("lottery-entrant") && !admin_review.html.contains("/exclude")
    );
    let author_review = f.get(&review, Some(&f.author)).await;
    assert!(author_review.html.contains("/entries/4/exclude"));
    f.post(
        &format!("/api/thread-{tid}/entries/4/exclude"),
        &f.author,
        &[("reason", "did not share")],
    )
    .await
    .redirect(&review);
    assert!(
        f.get(&review, Some(&f.author))
            .await
            .html
            .contains("Removed: did not share")
    );
    assert!(
        f.get(&topic, Some(&f.other))
            .await
            .html
            .contains("The author removed your entry: did not share")
    );
    assert!(
        f.get(&topic, None)
            .await
            .html
            .contains("1 removed by the author")
    );

    // Redemption code page: the author sees the plaintext, administrators only see the distribution status, others
    // cannot see it.
    let codes = format!("/threads/{tid}/lottery/codes");
    let board = f.get(&codes, Some(&f.author)).await;
    board.status(StatusCode::OK);
    assert!(board.html.contains(CODE));
    assert_eq!(board.headers[header::CACHE_CONTROL], "private, no-store");
    let admin_board = f.get(&codes, Some(&f.admin)).await;
    admin_board.status(StatusCode::OK);
    assert!(!admin_board.html.contains(CODE) && admin_board.html.contains("Not given out"));
    f.get(&codes, Some(&f.entrant))
        .await
        .status(StatusCode::NOT_FOUND);

    // Early drawing: only the author; entries are rejected after the drawing.
    f.post(&format!("/api/thread-{tid}/draw"), &f.entrant, &[])
        .await
        .status(StatusCode::UNPROCESSABLE_ENTITY);
    f.post(&format!("/api/thread-{tid}/draw"), &f.author, &[])
        .await
        .redirect(&topic);
    assert!(
        f.get(&topic, Some(&f.author))
            .await
            .html
            .contains("the author drew early.")
    );
    let later = identity::now() + 5;
    assert_eq!(activities::process_due(db, later, 20).await.unwrap(), 1);

    // The only eligible member, no. 3, wins the first prize (a redemption code); the credits prize goes unclaimed and
    // is returned to the author.
    let won = f.get(&topic, Some(&f.entrant)).await;
    assert!(won.html.contains("You won: Member code") && won.html.contains("href=\"/my/prizes\""));
    assert!(!won.html.contains(CODE));
    assert_eq!(
        f.scalar("SELECT available FROM gonof_common_credit_account WHERE uid=2")
            .await,
        100
    );
    let prizes_page = f.get("/my/prizes", Some(&f.entrant)).await;
    prizes_page.status(StatusCode::OK);
    assert!(prizes_page.html.contains(CODE));
    assert_eq!(
        prizes_page.headers[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert!(prizes_page.html.contains("aria-current=\"page\""));
    for token in [&f.other, &f.author, &f.admin] {
        let page = f.get("/my/prizes", Some(token)).await;
        page.status(StatusCode::OK);
        assert!(!page.html.contains(CODE));
    }
    f.get("/my/prizes", None)
        .await
        .status(StatusCode::SEE_OTHER);

    // After the drawing the author corrects winners one by one, and the winners see a correction notice.
    let board = f.get(&codes, Some(&f.author)).await;
    assert!(board.html.contains("Given to lottery-entrant"));
    f.post(
        &format!("/threads/{tid}/lottery/codes/1/1"),
        &f.author,
        &[("code", "HTTP-FIXED-CODE-92"), ("expected", "0")],
    )
    .await
    .status(StatusCode::OK);
    let fixed = f.get("/my/prizes", Some(&f.entrant)).await;
    assert!(
        fixed.html.contains("HTTP-FIXED-CODE-92")
            && fixed.html.contains("The author corrected this code")
    );
    f.post(
        &format!("/threads/{tid}/lottery/codes/1/1"),
        &f.author,
        &[("code", "AGAIN"), ("expected", "0")],
    )
    .await
    .status(StatusCode::UNPROCESSABLE_ENTITY);
    f.services.cleanup().await.unwrap();
}
