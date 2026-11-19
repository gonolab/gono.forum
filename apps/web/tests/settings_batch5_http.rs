//! Settings that reach the pages: no registration entry when registration is closed, times in thread lists and
//! "Recently viewed" follow the relative time settings, form limits are derived from server constants, and the day
//! counts in the built-in privacy notice and sanction emails follow the settings.
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    clock, content,
    content::writes::{ContentWriter, MAX_TITLE_CHARS, NewThread, WriteConfig},
    db,
    identity::{self, Actor},
    mail::{
        Mailer,
        templates::{self, MailTemplate},
    },
    messages,
    moderation::{self, SanctionRequest},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9951;
const MEMBER: i64 = 9952;

struct Fixture {
    f: TestFixture,
    app: Router,
    admin: identity::SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        for (uid, role) in [(ADMIN, 1), (MEMBER, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid)
                .bind(format!("batch5-{uid}"))
                .bind(format!("batch5-{uid}@example.test"))
                .bind(role)
                .execute(f.db.executor())
                .await
                .unwrap();
        }
        let identity = identity::Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let admin = http_auth::login(&identity, &f.db, ADMIN).await;
        // Shares the same in-process cache handle with the save functions: the app's next request after saving sees the
        // new value.
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, admin }
    }

    async fn update(&self, change: impl FnOnce(&mut runtime::SiteSettings)) {
        runtime::update(
            &self.f.db,
            &self.f.global,
            Actor {
                uid: ADMIN,
                credentials_version: self.admin.member.credentials_version,
                api_key: None,
            },
            "site.settings.save",
            change,
        )
        .await
        .unwrap();
    }

    async fn get(&self, path: &str, signed_in: bool) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if signed_in {
            request = request.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.admin.session_token),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// The administrator posts a public thread in the "Daily" forum, posted at `at` (which is the time shown in the list).
    async fn thread_at(&self, at: i64) -> i64 {
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
            .fetch_one(self.f.db.executor())
            .await
            .unwrap();
        ContentWriter::new(self.f.db.clone(), WriteConfig::default())
            .publish_at(
                ADMIN,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: "A thread for list times".into(),
                    body: "List times follow the relative time settings of the site".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                at,
            )
            .await
            .unwrap()
            .tid
    }
}

/// The "Sign up" entries anonymous visitors see: page header, mobile menu, sidebar member card, reply box, and the tabs
/// of the login and password recovery pages.
fn register_links(html: &str) -> usize {
    html.matches("href=\"/register").count()
}

/// When the registration mode is closed, the page header, mobile menu, sidebar, reply box and the login and password
/// recovery pages give no "Sign up" entry; `/register` is still reachable and explains that it is closed; in both
/// invite and open modes the entries are there.
#[tokio::test]
async fn registration_links_follow_the_signup_mode() {
    let f = Fixture::new().await;
    let tid = f.thread_at(identity::now() - 60).await;
    let thread = format!("/thread-{tid}");
    let pages = ["/", thread.as_str(), "/login", "/forgot-password"];
    // The seed is invite-only registration: the home page has three (page header, mobile menu, sidebar), the thread
    // page one more in the reply box, and the login and recovery pages have tabs.
    for path in pages {
        let (status, html) = f.get(path, false).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            register_links(&html) >= 1,
            "{path} should have a sign-up entry"
        );
    }
    let (_, home) = f.get("/", false).await;
    assert_eq!(
        register_links(&home),
        3,
        "one each in the page header, mobile menu and sidebar"
    );

    f.update(|s| s.signup.mode = "closed".into()).await;
    for path in pages {
        let (status, html) = f.get(path, false).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            register_links(&html),
            0,
            "{path} still has a sign-up entry after sign-ups closed"
        );
        assert!(
            html.contains("href=\"/login"),
            "{path} still shows the login entry"
        );
    }
    let (status, html) = f.get("/register", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Sign-ups are closed for now."));
    // The registration page's own tab stays (current page), while the page header no longer gives an entry.
    assert_eq!(register_links(&html), 1);
    assert!(html.contains("href=\"/register\" aria-current=\"page\">Sign up</a>"));

    f.update(|s| s.signup.mode = "open".into()).await;
    let (_, home) = f.get("/", false).await;
    assert_eq!(
        register_links(&home),
        3,
        "open sign-ups bring the entries back"
    );
    f.f.cleanup().await.unwrap();
}

/// Times in thread lists follow `settings.time.relative` and `relative_hours`: when off, always an absolute time
/// without the year; when on and within range, "N hours ago"; out of range, an absolute time; the hover tooltip is
/// always an absolute time with the year. The "Recently viewed" card passes the same set of settings to the front end.
#[tokio::test]
async fn topic_list_times_follow_the_relative_time_settings() {
    let f = Fixture::new().await;
    let at = identity::now() - 2 * 3600 - 120;
    f.thread_at(at).await;
    // Seed: UTC+8, ymd-dash, 24-hour clock, relative time off, range 72 hours.
    let absolute = clock::TimeFormat {
        relative_hours: None,
        ..clock::TimeFormat::default()
    };
    let short = clock::short_datetime(at, &absolute);
    let title = clock::datetime(at, &absolute);
    assert!(short.contains(':') && !short.contains("ago"), "{short}");
    // The list time is a <time> with the instant in UTC (docs/skins.md, "Times") and the absolute time as its title.
    let datetime = clock::iso(at);
    let row = |text: &str| {
        format!(
            "<time class=\"row-meta__time\" datetime=\"{datetime}\" title=\"{title}\">{text}</time>"
        )
    };

    let (status, html) = f.get("/", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains(&row(&short)),
        "with relative times off the time is absolute"
    );
    assert!(!html.contains("2 hours ago"));
    for attribute in [
        "data-relative-hours=\"0\"",
        "data-offset-minutes=\"480\"",
        "data-date-format=\"ymd-dash\"",
        "data-clock24=\"true\"",
    ] {
        assert!(
            html.contains(attribute),
            "the recently viewed card lacks {attribute}"
        );
    }

    f.update(|s| s.time.relative = true).await;
    let (_, html) = f.get("/", false).await;
    assert!(
        html.contains(&row("2 hours ago")),
        "with relative times on, times within 72 hours are relative"
    );
    assert!(html.contains("data-relative-hours=\"72\""));

    // Range set to 1 hour: a thread from two hours ago goes back to absolute time. After the date format and clock
    // style change, the list and the card change along.
    f.update(|s| {
        s.time.relative_hours = 1;
        s.time.date_format = "mdy-slash".into();
        s.time.clock24 = false;
    })
    .await;
    let format = clock::TimeFormat {
        date: clock::DateStyle::MdySlash,
        clock24: false,
        ..absolute
    };
    let (_, html) = f.get("/", false).await;
    assert!(
        html.contains(&format!(
            "<time class=\"row-meta__time\" datetime=\"{datetime}\" title=\"{}\">{}</time>",
            clock::datetime(at, &format),
            clock::short_datetime(at, &format)
        )),
        "out of range the time is absolute"
    );
    assert!(!html.contains("2 hours ago"));
    for attribute in [
        "data-relative-hours=\"1\"",
        "data-date-format=\"mdy-slash\"",
        "data-clock24=\"false\"",
    ] {
        assert!(
            html.contains(attribute),
            "the recently viewed card lacks {attribute}"
        );
    }
    f.f.cleanup().await.unwrap();
}

/// Form limits are derived from server constants, choosing the conservative limit: title 255 counted in characters,
/// and the browser's UTF-16 count is never less than the character count; search is 300 bytes divided by 3. The
/// private message field has no browser limit: it would cut a pasted message of 2,000 characters with emoji in it,
/// which the server accepts, so only the hint states the limit.
#[tokio::test]
async fn form_limits_come_from_the_server_constants() {
    let f = Fixture::new().await;
    let (status, html) = f.get("/new", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!(
        "<input id=\"editor-title\" name=\"title\" value=\"\" maxlength=\"{MAX_TITLE_CHARS}\" required>"
    )));
    let fields = ["title", "body", "fid", "visibility", "category"]
        .map(|name| html.find(&format!("name=\"{name}\"")).unwrap());
    assert!(
        fields.windows(2).all(|pair| pair[0] < pair[1]),
        "the posting form fields are out of order"
    );
    assert!(!html.contains("name=\"tags\""));
    assert!(html.contains("type=\"search\" name=\"q\" placeholder=\"Search\" maxlength=\"100\""));
    let (status, html) = f.get("/my/messages/new", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("name=\"body\" rows=\"5\" required>"));
    assert_eq!(messages::BODY_MAX, 2000);
    assert!(html.contains("Up to 2,000 characters"), "{html}");
    // The search box's 100 UTF-16 units are at most 300 bytes: the server still only blocks more than 300 bytes.
    // "\u{6587}" is a Chinese character: one UTF-16 unit and 3 bytes in UTF-8, so 100 of them are exactly 300 bytes.
    let longest = "\u{6587}".repeat(100);
    let (status, _) = f
        .get(&format!("/search?q={}", urlencode(&longest)), false)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = f
        .get(
            &format!("/search?q={}", urlencode(&"\u{6587}".repeat(101))),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.f.cleanup().await.unwrap();
}

/// The raw text of a given kind of email sent to `MEMBER` in the local mailbox.
async fn sent_mail(f: &TestFixture, kind: &str) -> String {
    let id: i64 = db::query_scalar(
        "SELECT eventid FROM gonof_common_outbox WHERE aggregate_id=? AND event_type=?",
    )
    .bind(MEMBER)
    .bind(kind)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    std::fs::read_to_string(f.root.join(format!(".local/mail/{id}.eml"))).unwrap()
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// The retention days for former usernames in the built-in privacy notice follow `settings.username.history_days`; a
/// body saved in the admin area is shown as is.
#[tokio::test]
async fn built_in_privacy_text_follows_the_username_history_days() {
    let f = Fixture::new().await;
    let (status, html) = f.get("/privacy", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("former username stays reserved for 90 days"));
    f.update(|s| s.username.history_days = 120).await;
    let (_, html) = f.get("/privacy", false).await;
    assert!(html.contains("former username stays reserved for 120 days"));
    assert!(!html.contains("former username stays reserved for 90 days"));
    // The data request page's description already reads the same setting.
    let (_, html) = f.get("/my/data", true).await;
    assert!(html.contains("username stays reserved for 120 days so no one else can take it"));
    f.f.cleanup().await.unwrap();
}

/// The built-in bodies of sanction emails are written with the current appeal window and handling time; the three
/// sanction email templates get two more variables `{{appeal_window_days}}` and `{{appeal_handling_days}}`, which the
/// admin listing, preview and delivery all recognise (9.18 item 4).
#[tokio::test]
async fn governance_mail_days_follow_the_appeal_settings() {
    let f = Fixture::new().await;
    let db = &f.f.db;
    let actor = |uid| Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    };
    for kind in ["sanction_issued", "sanction_ended", "appeal_result"] {
        let variables = templates::variables(kind);
        assert!(variables.contains(&"appeal_window_days"), "{kind}");
        assert!(variables.contains(&"appeal_handling_days"), "{kind}");
    }
    // Other templates do not recognise these two variables.
    let foreign = MailTemplate {
        subject: "Content update".into(),
        body: "{{appeal_window_days}} {{link}}".into(),
    };
    assert!(templates::validate("content", &foreign).is_err());
    let custom = MailTemplate {
        subject: "Sanction ended {{username}}".into(),
        body: "CUSTOM appeal {{appeal_window_days}} days, handling {{appeal_handling_days}} days {{link}}"
            .into(),
    };
    let (_, preview) = templates::preview(
        "sanction_ended",
        &custom,
        TEST_SITE_URL,
        &gono_forum_web::clock::TimeFormat::default(),
    )
    .unwrap();
    assert!(preview.contains("CUSTOM appeal 30 days, handling 7 days"));
    let (status, html) = f.get("/manage/mail/templates/sanction_issued", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("appeal_window_days, appeal_handling_days"));

    templates::save(db, actor(ADMIN), "sanction_ended", custom, false)
        .await
        .unwrap();
    f.update(|s| {
        s.governance.appeal_window_days = 45;
        s.governance.appeal_handling_days = 10;
    })
    .await;
    let now = identity::now();
    let sanction = moderation::issue_sanction(
        db,
        actor(ADMIN),
        &SanctionRequest {
            uid: MEMBER,
            expected_version: 1,
            kind: "warning".into(),
            hours: 0,
            reason: "Mail day counts".into(),
            request_key: identity::random_token(),
        },
        now,
    )
    .await
    .unwrap();
    moderation::revoke_sanction(db, actor(ADMIN), sanction, "Warning over", now)
        .await
        .unwrap();
    // Deliver with a newly created cache handle, which reads the current values in the database.
    let mailer = Mailer::new(
        db.clone(),
        Default::default(),
        f.f.root.join(".local/mail"),
        gono_forum_web::test_support::detached_global(),
    );
    // An administrator login may additionally queue a login alert; only require that both sanction emails are sent
    // without failure.
    let report = mailer.deliver_pending(now + 1).await.unwrap();
    assert!(report.delivered >= 2 && report.failed == 0, "{report:?}");
    let issued = sent_mail(&f.f, "sanction_issued").await;
    assert!(
        issued.contains("Each sanction can be appealed once within 45 days after it starts; an appeal is accepted for handling when you submit it and is expected to be handled within 10 days."),
        "{issued}"
    );
    assert!(!issued.contains("within 30 days"));
    let ended = sent_mail(&f.f, "sanction_ended").await;
    assert!(
        ended.contains("CUSTOM appeal 45 days, handling 10 days"),
        "{ended}"
    );
    f.f.cleanup().await.unwrap();
}
