//! Interface languages (docs/architecture.md, "Languages"): the site and domain languages driving the page, mail
//! templates and policies kept per language, and the admin module "Languages".
#[path = "support/http_auth.rs"]
mod http_auth;

use std::collections::BTreeMap;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content, db,
    i18n::Lang,
    identity,
    mail::templates::{self, MailTemplate},
    runtime,
    test_support::{TEST_SITE_URL, TestFixture, host_of},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9600;
const MEMBER: i64 = 9601;

struct Http {
    f: TestFixture,
    app: Router,
    admin: identity::SessionGrant,
}

struct Reply {
    status: StatusCode,
    location: String,
    body: String,
}

impl Http {
    async fn new() -> Self {
        let f = TestFixture::migrated().await.unwrap();
        content::seed_forums(&f.db).await.unwrap();
        for (uid, role) in [(ADMIN, 1), (MEMBER, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid)
                .bind(format!("i18n-{uid}"))
                .bind(format!("i18n-{uid}@example.test"))
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
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, admin }
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map_or(String::new(), |value| value.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            location,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn get(&self, host: &str, path: &str, signed_in: bool) -> Reply {
        let mut request = Request::builder().uri(path).header(header::HOST, host);
        if signed_in {
            request = request.header(
                header::COOKIE,
                format!("gonof_session={}", self.admin.session_token),
            );
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &self.admin.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, host_of(TEST_SITE_URL))
                .header(header::ORIGIN, TEST_SITE_URL)
                .header(
                    header::COOKIE,
                    format!("gonof_session={}", self.admin.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body.finish()))
                .unwrap(),
        )
        .await
    }
}

fn html_lang(page: &str) -> &str {
    let start = page
        .find("<html lang=\"")
        .expect("the page has <html lang>")
        + "<html lang=\"".len();
    &page[start..start + page[start..].find('"').unwrap()]
}

/// The site language comes from the admin module "Languages", another domain can override it, and pages, footers,
/// form errors and banners follow the request's language. The test site starts in English and switches to zh-CN, whose
/// expected text comes from the zh-CN messages.
#[tokio::test]
async fn site_and_domain_languages_drive_the_page() {
    let http = Http::new().await;
    let main = host_of(TEST_SITE_URL);
    let zh = Lang::parse("zh-CN").unwrap();
    let page = http.get(main, "/", false).await;
    assert_eq!(html_lang(&page.body), "en");
    assert!(page.body.contains(">Community rules</a>"));
    assert!(
        page.body
            .contains("<meta property=\"og:locale\" content=\"en_US\">")
    );
    assert!(page.body.contains("/static/i18n/en.js?v="));
    assert!(
        !page.body.contains(" dir=\""),
        "a left-to-right language writes no dir"
    );

    // The language module lists both languages with their completion and saves the site language.
    let languages = http.get(main, "/manage/languages", true).await;
    assert_eq!(languages.status, StatusCode::OK, "{}", languages.body);
    assert!(
        languages
            .body
            .contains("<option value=\"en\" selected lang=\"en\">English (en) · 100%</option>")
    );
    assert!(languages.body.contains(&format!(
        "<option value=\"zh-CN\" lang=\"zh-CN\">{}</option>",
        Lang::EN.language_option(zh.language_name(), "zh-CN", 100)
    )));
    assert!(languages.body.contains("manage-icon-language"));
    let saved = http
        .post("/manage/languages", &[("language", "zh-CN")])
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.location, "/manage/languages?saved=1");
    assert_eq!(
        runtime::read(&http.f.db).await.unwrap().site.language,
        "zh-CN"
    );
    // The form error after a POST is in the new site language.
    let rejected = http.post("/manage/languages", &[("language", "fr")]).await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        rejected.body.contains(zh.language_error_unknown()),
        "{}",
        rejected.body
    );
    // So is the banner after the redirect, and the progress figures: zh-CN writes numbers without grouping.
    let languages = http.get(main, "/manage/languages?saved=1", true).await;
    assert!(
        languages.body.contains(zh.language_saved()),
        "{}",
        languages.body
    );
    let (translated, total) = zh.completion();
    let progress = zh.language_progress(zh.percent(), translated, total);
    assert!(progress.contains(&format!("{total}")), "{progress}");
    assert!(languages.body.contains(&progress), "{}", languages.body);
    let page = http.get(main, "/", false).await;
    assert_eq!(html_lang(&page.body), "zh-CN");
    assert!(
        page.body
            .contains(&format!(">{}</a>", zh.frame_footer_rules()))
    );
    assert!(page.body.contains(&format!(
        "<meta property=\"og:locale\" content=\"{}\">",
        zh.og_locale()
    )));
    assert!(
        page.body.contains(">Sample Forum</a>"),
        "the fixture sets the header brand text"
    );
    assert!(page.body.contains("/static/i18n/zh-CN.js?v="));
    assert!(
        !page.body.contains(" dir=\""),
        "a left-to-right language writes no dir"
    );

    // Another domain with its own language (the domain form's "Language").
    let switched = http
        .post("/manage/domains/switch", &[("enabled", "on")])
        .await;
    assert!(switched.status.is_redirection(), "{}", switched.body);
    let general: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(http.f.db.executor())
        .await
        .unwrap();
    let forum = format!("forum_{general}");
    let added = http
        .post(
            "/manage/domains",
            &[
                ("domain_id", "0"),
                ("version", "0"),
                ("site_url", "http://demo2.test"),
                ("site_name", "Customer Feedback Center"),
                ("canonical", "self"),
                ("language", "en"),
                ("enabled", "1"),
                (&forum, "1"),
            ],
        )
        .await;
    assert!(added.status.is_redirection(), "{}", added.body);
    let other = http.get("demo2.test", "/", false).await;
    assert_eq!(html_lang(&other.body), "en");
    assert!(other.body.contains(">Community rules</a>"));
    assert_eq!(html_lang(&http.get(main, "/", false).await.body), "zh-CN");
    let rejected = http
        .post(
            "/manage/domains",
            &[
                ("domain_id", "0"),
                ("version", "0"),
                ("site_url", "http://demo3.test"),
                ("site_name", "Third"),
                ("canonical", "self"),
                ("language", "fr"),
                ("enabled", "1"),
            ],
        )
        .await;
    assert_eq!(
        rejected.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        rejected.body
    );
    // The site is zh-CN here, so the admin form answers in zh-CN.
    assert!(
        rejected.body.contains(zh.domain_error_language()),
        "{}",
        rejected.body
    );
    let languages = http.get(main, "/manage/languages", true).await;
    assert!(languages.body.contains(zh.language_used_site()));
    assert!(languages.body.contains("http://demo2.test"));
    http.f.cleanup().await.unwrap();
}

/// Policies and mail templates keep one copy per language: the public page shows the request language's copy (or
/// the built-in text), the edit pages switch with `?lang=`, and mail rendering picks the mail's language.
#[tokio::test]
async fn policies_and_mail_templates_are_kept_per_language() {
    let http = Http::new().await;
    let main = host_of(TEST_SITE_URL);
    let zh = Lang::parse("zh-CN").unwrap();
    let switched = http
        .post("/manage/languages", &[("language", "zh-CN")])
        .await;
    assert_eq!(switched.status, StatusCode::SEE_OTHER, "{}", switched.body);

    // The site language (zh-CN) without ?lang=, then English with it.
    let saved = http
        .post(
            "/manage/policies/terms",
            &[("title", "zh-CN terms"), ("body", "zh-CN terms body")],
        )
        .await;
    assert_eq!(saved.location, "/manage/policies/terms");
    let saved = http
        .post(
            "/manage/policies/terms?lang=en",
            &[("title", "English terms"), ("body", "English terms body")],
        )
        .await;
    assert_eq!(saved.location, "/manage/policies/terms?lang=en");
    let edit = http.get(main, "/manage/policies/terms?lang=en", true).await;
    assert!(
        edit.body.contains("value=\"English terms\""),
        "{}",
        edit.body
    );
    assert!(
        edit.body
            .contains("action=\"/manage/policies/terms?lang=en\"")
    );
    // The language tabs are worded in the site language (zh-CN).
    assert!(edit.body.contains(&format!(
        "lang=\"zh-CN\">{}{}</a>",
        zh.language_name(),
        zh.common_parenthesized(zh.policy_manage_customized())
    )));
    assert_eq!(
        http.get(main, "/manage/policies/terms?lang=fr", true)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let page = http.get(main, "/terms", false).await;
    assert!(page.body.contains("zh-CN terms body") && !page.body.contains("English terms body"));
    assert_eq!(
        runtime::document_in(&http.f.db, "policy.terms", Lang::EN)
            .await
            .unwrap()
            .body,
        "English terms body"
    );
    // The privacy policy has no English copy: an English page uses the built-in text.
    assert_eq!(
        runtime::document_in(&http.f.db, "policy.privacy", Lang::EN)
            .await
            .unwrap()
            .updated_at,
        0
    );
    http.post("/manage/languages", &[("language", "en")]).await;
    let page = http.get(main, "/terms", false).await;
    assert!(page.body.contains("English terms body"), "{}", page.body);

    // Mail templates: English only through ?lang=en; reset deletes that language's row.
    let saved = http
        .post(
            "/manage/mail/templates/content?lang=zh-CN",
            &[
                ("subject", "Content update (zh-CN)"),
                ("body", "{{username}}, see {{link}}"),
                ("operation", "save"),
            ],
        )
        .await;
    // The site is English by now, so editing zh-CN keeps ?lang= in the address.
    assert_eq!(saved.location, "/manage/mail/templates/content?lang=zh-CN");
    assert_eq!(
        templates::current_in(&http.f.db, "content", zh)
            .await
            .unwrap()
            .subject,
        "Content update (zh-CN)"
    );
    assert!(
        templates::current_in(&http.f.db, "content", Lang::EN)
            .await
            .unwrap()
            .subject
            .is_empty()
    );
    let values: BTreeMap<String, String> = [
        ("username", "reader"),
        ("title", "A thread"),
        ("link", "https://example.test/thread-1"),
        ("action", "replied"),
        ("site_name", "Forum"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let (subject, body) = templates::render_in(
        &http.f.db,
        "content",
        zh,
        &values,
        "Default subject",
        "Default body",
    )
    .await
    .unwrap();
    assert_eq!(
        (subject.as_str(), body.as_str()),
        (
            "Content update (zh-CN)",
            "reader, see https://example.test/thread-1"
        )
    );
    let (subject, _) = templates::render_in(
        &http.f.db,
        "content",
        Lang::EN,
        &values,
        "Built-in",
        "Built-in body",
    )
    .await
    .unwrap();
    assert_eq!(subject, "Built-in");
    let languages = http.get(main, "/manage/languages", true).await;
    // The site is English by now: the template kind is named in English.
    assert!(
        languages
            .body
            .contains("<a href=\"/manage/mail/templates/content?lang=zh-CN\">Mail template · Content updates</a>"),
        "{}",
        languages.body
    );
    assert!(
        languages
            .body
            .contains("<a href=\"/manage/policies/terms?lang=en\">Policy · Terms of use</a>")
    );
    let reset = http
        .post(
            "/manage/mail/templates/content?lang=zh-CN",
            &[("subject", ""), ("body", ""), ("operation", "reset")],
        )
        .await;
    assert!(reset.status.is_redirection());
    assert_eq!(
        templates::current_in(&http.f.db, "content", zh)
            .await
            .unwrap(),
        MailTemplate::default()
    );
    assert_eq!(
        db::query_scalar::<i64>(
            "SELECT COUNT(*) FROM gonof_common_localized_text WHERE kind='mail.content'"
        )
        .fetch_one(http.f.db.executor())
        .await
        .unwrap(),
        0
    );
    http.f.cleanup().await.unwrap();
}

/// A zh-CN site end to end on public pages: numbers inside sentences are written without grouping
/// (`meta-number-grouping = never`), and reply counts and relative times use the single form zh-CN has where English
/// picks a plural. The expected text comes from the zh-CN messages.
#[tokio::test]
async fn a_zh_cn_site_writes_numbers_without_grouping_and_counts_without_plurals() {
    use gono_forum_web::content::writes::{ContentWriter, NewReply, NewThread, WriteConfig};
    let http = Http::new().await;
    let main = host_of(TEST_SITE_URL);
    let zh = Lang::parse("zh-CN").unwrap();
    gono_forum_web::test_support::raise_posting_quotas(&http.f.db)
        .await
        .unwrap();
    for (key, value) in [
        ("settings.site.language", "zh-CN"),
        ("settings.posting.daily_replies", "2500"),
        ("settings.time.relative", "true"),
    ] {
        gono_forum_web::test_support::set_setting(&http.f.db, key, value)
            .await
            .unwrap();
    }
    http.f.global.clear_local();

    // The posting quotas in a sentence: English would write "1,000" and "2,500".
    let rules = http.get(main, "/credits/rules", false).await;
    assert_eq!(rules.status, StatusCode::OK, "{}", rules.body);
    assert_eq!(html_lang(&rules.body), "zh-CN");
    let quota = zh.site_credits_growth_no_cap(1000, 2500);
    assert!(quota.contains("1000") && quota.contains("2500"), "{quota}");
    assert!(rules.body.contains(&quota), "{}", rules.body);
    assert!(!rules.body.contains("1,000"));

    // One reply and two relative times on a thread page.
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug='daily'")
        .fetch_one(http.f.db.executor())
        .await
        .unwrap();
    let now = identity::now();
    let writer = ContentWriter::new(http.f.db.clone(), WriteConfig::default());
    let thread = writer
        .publish_at(
            MEMBER,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "A thread on a zh-CN site".into(),
                body: "The first post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now - 3 * 3600,
        )
        .await
        .unwrap();
    writer
        .reply_at(
            ADMIN,
            NewReply {
                tid: thread.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "The only reply".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            now - 3600,
        )
        .await
        .unwrap();
    let page = http
        .get(main, &format!("/thread-{}", thread.tid), false)
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    for expected in [
        zh.topic_reply_count(1),
        zh.time_hours_ago(3),
        zh.time_hours_ago(1),
    ] {
        assert!(page.body.contains(&expected), "{expected}: {}", page.body);
    }
    http.f.cleanup().await.unwrap();
}
