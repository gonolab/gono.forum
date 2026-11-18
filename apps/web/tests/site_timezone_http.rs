//! The whole site has a single site time zone `settings.time.offset_minutes`, which display, forms and "day"/"week"
//! settlement all follow; plus the admin site time zone options and the editor's activity deadline.
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    clock, content,
    content::writes::{ContentWriter, NewThread, WriteConfig},
    db,
    identity::{self, Actor},
    runtime,
    test_support::{SEEDED_OFFSET_MINUTES, TEST_SITE_URL, TestFixture},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9901;

struct Fixture {
    f: TestFixture,
    app: Router,
    identity: identity::Identity,
    admin: identity::SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,1,1)")
            .bind(ADMIN)
            .bind("tz-admin")
            .bind("tz-admin@example.test")
            .execute(f.db.executor())
            .await
            .unwrap();
        let identity = identity::Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let admin = http_auth::login(&identity, &f.db, ADMIN).await;
        // Shares the in-process cache handle with the save functions: the application's next request after a save sees
        // the new value.
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self {
            f,
            app,
            identity,
            admin,
        }
    }

    async fn set_offset(&self, minutes: i64) {
        runtime::update(
            &self.f.db,
            &self.f.global,
            Actor {
                uid: ADMIN,
                credentials_version: self.admin.member.credentials_version,
                api_key: None,
            },
            "site.settings.save",
            |s| s.time.offset_minutes = minutes,
        )
        .await
        .unwrap();
    }

    async fn admin_get(&self, path: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.admin.session_token),
            )
            .body(Body::empty())
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn admin_post(&self, path: &str, fields: &[(&str, &str)]) -> (StatusCode, String) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.admin.session_token),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
}

/// All `<option>`s in the drop-down `name="timezone_offset"`: (value, text, selected).
fn zone_options(html: &str) -> Vec<(String, String, bool)> {
    let start = html
        .find("name=\"timezone_offset\">")
        .expect("missing the site time zone drop-down");
    let select = &html[start..start + html[start..].find("</select>").unwrap()];
    select
        .split("<option value=\"")
        .skip(1)
        .map(|chunk| {
            let value = chunk[..chunk.find('"').unwrap()].to_owned();
            let open = &chunk[..chunk.find('>').unwrap()];
            let text =
                chunk[chunk.find('>').unwrap() + 1..chunk.find("</option>").unwrap()].to_owned();
            (value, text, open.contains("selected"))
        })
        .collect()
}

/// Admin "Site time zone": 11 time zones merge into 6 options by offset; when the database holds a value
/// outside the list, a "current value" option is added and selected; after saving, the whole site follows.
#[tokio::test]
async fn site_timezone_options_cover_every_zone_and_keep_an_unlisted_value() {
    let f = Fixture::new().await;
    let (status, html) = f.admin_get("/manage/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("for=\"settings-timezone_offset\">Site time zone</label>"));
    // Neither the old label nor a hard-coded Beijing day is left.
    assert!(!html.contains("Display time zone"));
    assert!(!html.contains("Beijing"));
    let options = zone_options(&html);
    let values: Vec<&str> = options.iter().map(|(value, _, _)| value.as_str()).collect();
    assert_eq!(values, ["0", "240", "330", "420", "480", "540"]);
    for zone in clock::SITE_ZONES {
        assert!(
            options.iter().any(|(_, text, _)| text.contains(zone.name)),
            "{} is not among the options",
            zone.name
        );
    }
    let selected: Vec<&str> = options
        .iter()
        .filter(|(_, _, selected)| *selected)
        .map(|(value, _, _)| value.as_str())
        .collect();
    assert_eq!(selected, ["480"], "only the seeded UTC+8 is selected");

    // An offset edited directly in the database and not in the list: a "current value" option is added and selected,
    // and submitting it unchanged does not replace it.
    f.set_offset(-300).await;
    let (_, html) = f.admin_get("/manage/settings").await;
    let options = zone_options(&html);
    assert_eq!(options.len(), 7);
    assert_eq!(
        options.last().unwrap(),
        &("-300".to_owned(), "UTC-5 (current value)".to_owned(), true)
    );

    // Saved as UTC through the admin form: reads back 0, and the page's explanation changes to UTC accordingly.
    let saved = f
        .admin_post(
            "/manage/settings",
            &[
                ("site_name", "Sample Community"),
                // Test requests carry no Host: the forum URL must be sent back unchanged, otherwise, left empty, the
                // page has no way to derive the site URL.
                ("site_url", TEST_SITE_URL),
                ("timezone_offset", "0"),
                ("date_format", "ymd-dash"),
                ("clock24", "true"),
                ("relative_hours", "72"),
            ],
        )
        .await;
    assert_eq!(saved.0, StatusCode::SEE_OTHER);
    assert_eq!(runtime::read(&f.f.db).await.unwrap().time.offset_minutes, 0);
    let (status, html) = f.admin_get("/manage/credits").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("Daily limits count per calendar day in the site time zone UTC;"),
        "{html}"
    );
    assert!(!html.contains("Beijing"));
    f.f.cleanup().await.unwrap();
}

/// The editor's activity deadline is entered in the site time zone: the label states the time zone, the input carries
/// the offset for the front end, and the default value is the wall-clock time three days later in the site time zone;
/// the "by calendar day" explanations on each page do not hard-code Beijing time.
#[tokio::test]
async fn editor_deadline_and_daily_notes_follow_the_site_timezone() {
    let f = Fixture::new().await;
    let (status, html) = f.admin_get("/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Activity deadline (site time zone UTC+8)"));
    assert!(html.contains("data-offset-minutes=\"480\""));

    f.set_offset(-300).await;
    let (_, html) = f.admin_get("/new").await;
    assert!(html.contains("Activity deadline (site time zone UTC-5)"));
    assert!(html.contains("data-offset-minutes=\"-300\""));
    let deadline = html
        .split("name=\"deadline\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let at = chrono::NaiveDateTime::parse_from_str(deadline, "%Y-%m-%dT%H:%M")
        .unwrap()
        .and_utc()
        .timestamp()
        + 5 * 3600;
    let expected = identity::now() + 3 * 86_400;
    assert!(
        (at - expected).abs() < 120,
        "the default deadline should be three days later in the site time zone (UTC-5), got: {deadline}"
    );

    // The sample time in the admin mail template preview is written the same way as in real e-mails, in the site time
    // zone (it used to hard-code "(Beijing time)").
    let (status, html) = f
        .admin_post(
            "/manage/mail/templates/login_alert",
            &[
                ("operation", "preview"),
                ("subject", "New device login"),
                ("body", "Time: {{time}} {{link}}"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Time: 2026-10-31 07:15 (UTC-5)"), "{html}");
    assert!(!html.contains("Beijing"));

    for (path, note) in [
        (
            "/my/credits",
            "Counted by calendar day in the site time zone UTC-5.",
        ),
        (
            "/credits/rules",
            "Daily caps count calendar days in the site time zone, UTC-5.",
        ),
        (
            "/my/attachments",
            "(by calendar day in the site time zone UTC-5;",
        ),
    ] {
        let (status, html) = f.admin_get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(html.contains(note), "{path} lacks: {note}");
        assert!(!html.contains("Beijing"), "{path}");
        assert!(!html.contains("UTC calendar day"), "{path}");
    }
    f.f.cleanup().await.unwrap();
}

/// The invite code count "generated today" and the sidebar "contributors this week" both split days and
/// weeks by the site time zone: the same set of records gives different results in another time zone.
#[tokio::test]
async fn invite_day_and_contributor_week_follow_the_site_timezone() {
    let f = Fixture::new().await;
    // A moment that falls on "today" in UTC+8 but not "today" in UTC (or the reverse) necessarily counts 1 in one time
    // zone and 0 in the other.
    let now = identity::now();
    let east8 = clock::day_start(now, 480);
    let utc = clock::day_start(now, 0);
    let (dateline, east8_today) = if east8 < utc {
        (utc - 1, true)
    } else {
        (east8 - 1, false)
    };
    db::query("INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime) VALUES (?, 'tz-code', 'tz-request', ?, ?)")
        .bind(ADMIN)
        .bind(dateline)
        .bind(now + 86_400)
        .execute(f.f.db.executor())
        .await
        .unwrap();
    let count = |page: identity::InvitePage| page.today_generated;
    assert_eq!(
        count(f.identity.invites(ADMIN, 1).await.unwrap()),
        i64::from(east8_today),
        "a UTC+8 site"
    );
    f.set_offset(0).await;
    assert_eq!(
        count(f.identity.invites(ADMIN, 1).await.unwrap()),
        i64::from(!east8_today),
        "a UTC site"
    );

    // 2030-01-07 is a Monday. One hour before Monday 00:00 UTC is already Monday 07:00 in UTC+8: it counts in UTC+8's
    // current week but not in UTC's.
    let monday = 1_893_974_400;
    assert_eq!(clock::week_start(monday + 60, 0), monday);
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(f.f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.f.db.clone(), WriteConfig::default());
    writer
        .publish_at(
            ADMIN,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Late Sunday night across time zones".into(),
                body: "Contributors this week start on Monday in the site time zone".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            monday - 3600,
        )
        .await
        .unwrap();
    let at = u64::try_from(monday + 60).unwrap();
    for (offset, expected) in [(SEEDED_OFFSET_MINUTES, 1), (0, 0)] {
        let statistics = content::statistics_at(&f.f.db, at, offset).await.unwrap();
        assert_eq!(statistics.contributors, expected, "offset {offset}");
    }
    f.f.cleanup().await.unwrap();
}
