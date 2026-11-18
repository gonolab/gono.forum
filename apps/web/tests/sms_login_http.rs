//! SMS code sign-in: sending codes, limits and verification, SMS registration and completing registration, the admin
//! options when two-factor authentication is on, setting a password and binding, rebinding and unbinding a mobile
//! number in account security, per-domain participation conditions, and the admin sign-in methods page. SMS uses
//! local delivery, and codes are taken from `.local/sms/`.
use std::{collections::BTreeMap, net::SocketAddr};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    },
    db::{self, Database},
    i18n::Lang,
    identity::{self, Identity, SessionGrant},
    login::Participation,
    router,
    test_support::{self, TEST_SITE_URL, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const MOBILE: &str = "13800138000";
const E164: &str = "+8613800138000";
const OTHER: &str = "13900139000";
const PASSWORD: &str = "Sms-login-password-2026!";
const DEMO2: &str = "http://demo2.test:58833";

struct Fixture {
    services: test_support::TestFixture,
    db: Database,
    identity: Identity,
    app: Router,
    admin: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    /// `prepare` changes the database before the application is built (domain table and so on), so the application's
    /// process cache reads the changed values.
    async fn with(prepare: impl FnOnce(&Database)) -> Self {
        let services = test_support::TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        let root = services.root.clone();
        content::seed_forums(&db).await.unwrap();
        set_setting(&db, "settings.login.methods", "sms")
            .await
            .unwrap();
        set_setting(&db, "settings.signup.mode", "open")
            .await
            .unwrap();
        set_setting(
            &db,
            "settings.account.password_change_account_per_5min",
            "20",
        )
        .await
        .unwrap();
        prepare(&db);
        let identity = Identity::new(
            db.clone(),
            services.config.identity.clone(),
            root.clone(),
            test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let app = router::build_with_global(
            services.config.clone(),
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
            admin,
        }
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

    /// The latest local SMS sent to `mobile` (E.164): (purpose, code).
    fn latest_sms(&self, mobile: &str) -> (String, String) {
        let folder = self.services.root.join(".local/sms");
        let mut found: Vec<(i64, String)> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                let id = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".txt")?
                    .parse()
                    .ok()?;
                Some((id, std::fs::read_to_string(entry.path()).ok()?))
            })
            .filter(|(_, body)| body.contains(&format!("Mobile number: {mobile}\n")))
            .collect();
        found.sort();
        let body = &found
            .last()
            .expect("no text message was sent to this number")
            .1;
        let line = |label: &str| {
            body.lines()
                .find_map(|line| line.strip_prefix(label))
                .unwrap()
                .to_owned()
        };
        (line("Purpose: "), line("Code: "))
    }

    /// Moves already sent codes back in time to get around the "per-number interval" (verification looks only at the
    /// latest row; the old rows are moved). Moves only 61 seconds (the interval lower bound is 60 seconds), and not
    /// earlier than midnight of the current day in the site time zone: previously each move was 1 hour, so when run in
    /// the early morning a few moves pushed into the previous day and the "per number per day" and "site-wide per day"
    /// counts were reset.
    async fn age_codes(&self) {
        let offset: i64 = test_support_value(&self.db, "settings.time.offset_minutes")
            .await
            .parse()
            .unwrap();
        let day = gono_forum_web::clock::day_start(identity::now(), offset);
        db::query("UPDATE gonof_common_sms_code SET dateline=GREATEST(dateline-61,?),expires_at=expires_at-61")
            .bind(day)
            .execute(self.db.executor())
            .await
            .unwrap();
    }

    async fn member(
        &self,
        username: &str,
    ) -> (i64, Option<String>, Option<String>, Option<String>) {
        let row =
            db::query("SELECT uid,mobile,email,password FROM gonof_common_member WHERE username=?")
                .bind(username)
                .fetch_one(self.db.executor())
                .await
                .unwrap();
        (
            row.try_get("uid").unwrap(),
            row.try_get("mobile").unwrap(),
            row.try_get("email").unwrap(),
            row.try_get("password").unwrap(),
        )
    }
}

#[derive(Clone, Default)]
struct Cookies(BTreeMap<String, String>);

impl Cookies {
    fn session(raw: &str) -> Self {
        Self(BTreeMap::from([("gonof_session".into(), raw.into())]))
    }
    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self.0.get("gonof_session") {
            identity
                .session(raw)
                .await
                .unwrap()
                .map(|session| session.csrf_token)
                .unwrap_or_default()
        } else {
            self.0.get("gonof_csrf").cloned().unwrap_or_default()
        }
    }
    fn update(&mut self, headers: &HeaderMap) {
        for cookie in headers.get_all(header::SET_COOKIE) {
            let (name, value) = cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap();
            if value.is_empty() {
                self.0.remove(name);
            } else {
                self.0.insert(name.into(), value.into());
            }
        }
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

impl Reply {
    fn redirect(&self, path: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.html);
        assert_eq!(self.headers[header::LOCATION], path);
    }
}

async fn request(
    fixture: &Fixture,
    origin: &str,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, test_support::host_of(origin))
        .header(
            header::COOKIE,
            cookies
                .0
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        );
    if method == "POST" {
        builder = builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, origin)
            .header(
                header::USER_AGENT,
                "Mozilla/5.0 Chrome/141.0.0.0 Safari/537.36",
            );
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut req = builder.body(Body::from(form)).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.61:48321".parse::<SocketAddr>().unwrap(),
    ));
    let response = fixture.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    cookies.update(&headers);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        html: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(fixture: &Fixture, path: &str, cookies: &mut Cookies) -> Reply {
    request(fixture, TEST_SITE_URL, "GET", path, cookies, &[]).await
}

async fn post(
    fixture: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    post_on(fixture, TEST_SITE_URL, path, cookies, fields).await
}

async fn post_on(
    fixture: &Fixture,
    origin: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let csrf = cookies.csrf(&fixture.identity).await;
    let mut fields = fields.to_vec();
    fields.push(("_csrf", &csrf));
    request(fixture, origin, "POST", path, cookies, &fields).await
}

/// An anonymous visitor goes through both steps of SMS sign-in; returns the response after submitting the code.
async fn sms_login(fixture: &Fixture, cookies: &mut Cookies, mobile: &str) -> Reply {
    let page = get(fixture, "/login/sms", cookies).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.html);
    post(fixture, "/login/sms/send", cookies, &[("mobile", mobile)])
        .await
        .redirect("/login/sms");
    let e164 = identity::phone::normalize_mobile(mobile).unwrap();
    let (purpose, code) = fixture.latest_sms(&e164);
    assert_eq!(purpose, "login");
    post(fixture, "/login/sms", cookies, &[("code", &code)]).await
}

#[tokio::test]
async fn sms_login_registers_logs_in_and_enforces_limits() {
    let f = Fixture::new().await;
    let mut visitor = Cookies::default();
    let login = get(&f, "/login", &mut visitor).await;
    assert!(
        login.html.contains("/login/sms\">Text message login</a>"),
        "the login page has a text message login tab"
    );

    // Step one: send the code. The number is normalized to E.164, the database stores only the digest, and the locally
    // delivered file contains the code.
    get(&f, "/login/sms", &mut visitor).await;
    post(
        &f,
        "/login/sms/send",
        &mut visitor,
        &[("mobile", "138 0013 8000")],
    )
    .await
    .redirect("/login/sms");
    let (purpose, code) = f.latest_sms(E164);
    assert_eq!(purpose, "login");
    assert_eq!(code.len(), 6);
    // Only the digest is stored: 64 hex digits (the length is checked in code, the same statement for both backends).
    let hashes: Vec<String> = db::query_scalar(
        "SELECT code_hash FROM gonof_common_sms_code WHERE mobile='+8613800138000' AND purpose='login'",
    )
    .fetch_all(f.db.executor())
    .await
    .unwrap();
    assert_eq!(hashes.len(), 1);
    assert_eq!(hashes[0].len(), 64);
    let plain: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_common_sms_code WHERE code_hash=?")
            .bind(&code)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(plain, 0, "the database does not store the code itself");
    let step = get(&f, "/login/sms", &mut visitor).await;
    assert!(
        step.html.contains("The code was sent to 138****8000."),
        "{}",
        step.html
    );

    // Per-number interval: sending again right away is blocked.
    let again = post(&f, "/login/sms/send", &mut visitor, &[("mobile", MOBILE)]).await;
    assert_eq!(again.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(again.html.contains("before asking again"), "{}", again.html);

    // Wrong input is counted; correct input with an unregistered number goes to complete registration.
    let wrong_code = if code == "000000" { "111111" } else { "000000" };
    let wrong = post(&f, "/login/sms", &mut visitor, &[("code", wrong_code)]).await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.html.contains(Lang::EN.account_error_wrong_code()));
    assert_eq!(
        f.scalar("SELECT attempts FROM gonof_common_sms_code ORDER BY id DESC LIMIT 1")
            .await,
        1
    );
    post(&f, "/login/sms", &mut visitor, &[("code", &code)])
        .await
        .redirect("/register/mobile");
    let complete = get(&f, "/register/mobile", &mut visitor).await;
    assert_eq!(complete.status, StatusCode::OK);
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&complete.html);
    assert!(
        complete
            .html
            .contains("The phone number 138****8000 is verified.")
    );

    // Without ticking consent the registration transaction is not entered; with it ticked the account is created: no
    // password, no email, bound to the mobile number.
    let refused = post(
        &f,
        "/register/mobile",
        &mut visitor,
        &[("username", "sms-member")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username='sms-member'")
            .await,
        0
    );
    post(
        &f,
        "/register/mobile",
        &mut visitor,
        &[
            ("username", "sms-member"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ],
    )
    .await
    .redirect("/");
    assert!(
        visitor.0.contains_key("gonof_session"),
        "signed in after registering"
    );
    let (uid, mobile, email, password) = f.member("sms-member").await;
    assert_eq!(mobile.as_deref(), Some(E164));
    assert_eq!(email, None);
    assert_eq!(password, None);
    let register_log: String = db::query_scalar(
        "SELECT data FROM gonof_common_log WHERE uid=? AND type='identity.register'",
    )
    .bind(uid)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(
        register_log.contains("\"method\":\"sms\""),
        "{register_log}"
    );
    assert!(register_log.contains("138****8000") && !register_log.contains(E164));
    // The same pending-registration credential cannot create a second account.
    let mut replay = visitor.clone();
    replay.0.remove("gonof_session");
    let twice = post(
        &f,
        "/register/mobile",
        &mut replay,
        &[
            ("username", "second-member"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ],
    )
    .await;
    assert!(
        twice.status == StatusCode::SEE_OTHER || twice.html.contains("is already registered"),
        "{}",
        twice.html
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member WHERE username='second-member'")
            .await,
        0
    );

    // An already registered number coming back is a sign-in; a used code cannot be used again.
    f.age_codes().await;
    let mut fresh = Cookies::default();
    sms_login(&f, &mut fresh, MOBILE).await.redirect("/");
    assert!(fresh.0.contains_key("gonof_session"));
    let (_, used) = f.latest_sms(E164);
    let mut replayer = Cookies::default();
    get(&f, "/login/sms", &mut replayer).await;
    f.age_codes().await;
    post(&f, "/login/sms/send", &mut replayer, &[("mobile", MOBILE)]).await;
    let stale = post(&f, "/login/sms", &mut replayer, &[("code", &used)]).await;
    assert!(
        stale.html.contains(Lang::EN.account_error_wrong_code()),
        "an old code is not valid for the new one: {}",
        stale.html
    );

    // After the wrong-input limit is reached, this code is voided, and even correct input fails.
    set_setting(&f.db, "settings.sms.max_attempts", "2")
        .await
        .unwrap();
    f.services.global.clear_local();
    let mut tries = Cookies::default();
    get(&f, "/login/sms", &mut tries).await;
    f.age_codes().await;
    post(&f, "/login/sms/send", &mut tries, &[("mobile", MOBILE)]).await;
    let (_, right) = f.latest_sms(E164);
    let wrong_code = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    for _ in 0..2 {
        post(&f, "/login/sms", &mut tries, &[("code", wrong_code)]).await;
    }
    let locked = post(&f, "/login/sms", &mut tries, &[("code", &right)]).await;
    assert!(
        locked.html.contains(Lang::EN.account_error_expired_code()),
        "{}",
        locked.html
    );

    // Per number per day limit (seeded as 5): the 5th is sent as usual, the 6th is blocked.
    f.age_codes().await;
    post(&f, "/login/sms/send", &mut tries, &[("mobile", MOBILE)])
        .await
        .redirect("/login/sms");
    f.age_codes().await;
    let daily = post(&f, "/login/sms/send", &mut tries, &[("mobile", MOBILE)]).await;
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_sms_code WHERE mobile='+8613800138000'")
            .await,
        5
    );
    assert_eq!(
        daily.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        daily.html
    );
    assert!(
        daily.html.contains("This number has used up today"),
        "{}",
        daily.html
    );

    // Site-wide per day limit: once reached nothing more is sent, and the action log records it only once.
    let sent = f.scalar("SELECT COUNT(*) FROM gonof_common_sms_code").await;
    set_setting(&f.db, "settings.sms.site_daily_limit", &sent.to_string())
        .await
        .unwrap();
    f.services.global.clear_local();
    for _ in 0..2 {
        let site = post(&f, "/login/sms/send", &mut tries, &[("mobile", OTHER)]).await;
        assert_eq!(site.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            site.html.contains("text message codes have run out"),
            "{}",
            site.html
        );
    }
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_log WHERE type='sms.site_daily_limit' AND uid IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_sms_code").await,
        sent
    );
    f.services.cleanup().await.unwrap();
}

/// A referral link straight to SMS sign-in keeps its channel: a new number that finishes signing up there is
/// recorded with it.
#[tokio::test]
async fn a_referral_link_to_sms_login_is_the_registration_source() {
    let f = Fixture::new().await;
    let mut visitor = Cookies::default();
    let landing = get(&f, "/login/sms?utm_source=sms-ad", &mut visitor).await;
    assert_eq!(landing.status, StatusCode::OK, "{}", landing.html);
    assert!(
        visitor
            .0
            .keys()
            .any(|name| name.starts_with("gonof_registration_source")),
        "the landing sets the channel cookie: {:?}",
        visitor.0.keys()
    );
    sms_login(&f, &mut visitor, MOBILE)
        .await
        .redirect("/register/mobile");
    let page = get(&f, "/register/mobile", &mut visitor).await;
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&page.html);
    post(
        &f,
        "/register/mobile",
        &mut visitor,
        &[
            ("username", "sms-referred"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ],
    )
    .await
    .redirect("/");
    let (uid, _, _, _) = f.member("sms-referred").await;
    let channel: Option<String> =
        db::query_scalar("SELECT channel FROM gonof_common_member_register WHERE uid=?")
            .bind(uid)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(channel.as_deref(), Some("sms-ad"));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn sms_login_follows_registration_mode_and_two_factor_choice() {
    let f = Fixture::new().await;
    // When SMS sign-in is off, both the page and code sending are 404.
    set_setting(&f.db, "settings.login.methods", "")
        .await
        .unwrap();
    f.services.global.clear_local();
    let mut visitor = Cookies::default();
    assert_eq!(
        get(&f, "/login/sms", &mut visitor).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        !get(&f, "/login", &mut visitor)
            .await
            .html
            .contains("/login/sms\">")
    );
    set_setting(&f.db, "settings.login.methods", "sms")
        .await
        .unwrap();

    // Registration closed: an unregistered number cannot register even after verification.
    set_setting(&f.db, "settings.signup.mode", "closed")
        .await
        .unwrap();
    f.services.global.clear_local();
    let closed = sms_login(&f, &mut visitor, MOBILE).await;
    assert_eq!(closed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        closed.html.contains("sign-ups are closed for now"),
        "{}",
        closed.html
    );

    // Invite-only: completing registration requires an invite code; codes starting with a prefix pass as usual.
    set_setting(&f.db, "settings.signup.mode", "invite")
        .await
        .unwrap();
    set_setting(&f.db, "settings.invite.prefixes", "SMS")
        .await
        .unwrap();
    f.services.global.clear_local();
    f.age_codes().await;
    let mut invited = Cookies::default();
    sms_login(&f, &mut invited, MOBILE)
        .await
        .redirect("/register/mobile");
    let page = get(&f, "/register/mobile", &mut invited).await;
    assert!(page.html.contains("This site is invite-only"));
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&page.html);
    let missing = post(
        &f,
        "/register/mobile",
        &mut invited,
        &[
            ("username", "invited-sms"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ],
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    post(
        &f,
        "/register/mobile",
        &mut invited,
        &[
            ("username", "invited-sms"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
            ("invite", "SMS-2026"),
            ("email", "sms-member@example.test"),
        ],
    )
    .await
    .redirect("/verify-email");
    let (uid, mobile, email, _) = f.member("invited-sms").await;
    assert_eq!(mobile.as_deref(), Some(E164));
    assert_eq!(email.as_deref(), Some("sms-member@example.test"));
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid={uid} AND purpose='email_verification'"
        ))
        .await,
        1,
        "with an email given, the verification mail is sent as usual"
    );

    // Two-factor authentication on: seeded to match password sign-in, so it goes to the second step; after changing to
    // direct sign-in, a correct code issues the session.
    f.execute(&format!(
        "INSERT INTO gonof_common_member_two_factor(uid,enabled_at) VALUES({uid},1)"
    ))
    .await;
    f.age_codes().await;
    let mut second = Cookies::default();
    sms_login(&f, &mut second, MOBILE)
        .await
        .redirect("/login/verify");
    assert!(!second.0.contains_key("gonof_session"));
    assert!(second.0.contains_key("gonof_2fa"));
    set_setting(&f.db, "settings.login.external_two_factor", "skip")
        .await
        .unwrap();
    f.services.global.clear_local();
    f.age_codes().await;
    let mut direct = Cookies::default();
    sms_login(&f, &mut direct, MOBILE).await.redirect("/");
    assert!(direct.0.contains_key("gonof_session"));
    let method: String = db::query_scalar(
        "SELECT data FROM gonof_common_log WHERE uid=? AND type='identity.login' ORDER BY id DESC LIMIT 1",
    )
    .bind(uid)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(method.contains("\"method\":\"sms\""), "{method}");

    // Frozen accounts cannot sign in by SMS.
    f.execute(&format!(
        "UPDATE gonof_common_member SET `freeze`=1 WHERE uid={uid}"
    ))
    .await;
    f.age_codes().await;
    let mut frozen = Cookies::default();
    let refused = sms_login(&f, &mut frozen, MOBILE).await;
    assert_ne!(refused.status, StatusCode::SEE_OTHER);
    assert!(!frozen.0.contains_key("gonof_session"));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn account_security_sets_password_and_binds_changes_unbinds_mobile() {
    let f = Fixture::new().await;
    let mut member = Cookies::default();
    sms_login(&f, &mut member, MOBILE)
        .await
        .redirect("/register/mobile");
    let page = get(&f, "/register/mobile", &mut member).await;
    let [terms, privacy] = gono_forum_web::test_support::policy_fields(&page.html);
    post(
        &f,
        "/register/mobile",
        &mut member,
        &[
            ("username", "no-password-member"),
            ("accept_terms", "1"),
            (terms.0, &terms.1),
            (privacy.0, &privacy.1),
        ],
    )
    .await
    .redirect("/");

    // No password: shows "set password"; rebinding the mobile number and two-factor authentication both
    // require setting a password first.
    let account = get(&f, "/my/account", &mut member).await;
    assert!(
        account.html.contains("data-set-password"),
        "{}",
        account.html
    );
    assert!(account.html.contains("data-mobile-status=\"bound\""));
    assert!(account.html.contains("138****8000 is linked"));
    assert!(
        account
            .html
            .contains("Set a password first to turn on two-step verification.")
    );
    assert!(!account.html.contains("/my/account/mobile/send"));
    let early = post(
        &f,
        "/my/account/mobile/unbind",
        &mut member,
        &[("current_password", "anything")],
    )
    .await;
    assert!(
        early.html.contains(Lang::EN.account_error_no_password()),
        "{}",
        early.html
    );

    // Setting a password: the code is sent to the bound number; once set, all devices are signed out, and sign-in with
    // username and password works.
    f.age_codes().await;
    post(&f, "/my/account/password/send", &mut member, &[])
        .await
        .redirect("/my/account#password");
    let (purpose, code) = f.latest_sms(E164);
    assert_eq!(purpose, "confirm");
    let shown = get(&f, "/my/account", &mut member).await;
    assert!(shown.html.contains("/my/account/password/set"));
    let mismatch = post(
        &f,
        "/my/account/password/set",
        &mut member,
        &[
            ("code", &code),
            ("password", PASSWORD),
            ("password2", "different"),
        ],
    )
    .await;
    assert!(mismatch.html.contains("The two passwords do not match"));
    post(
        &f,
        "/my/account/password/set",
        &mut member,
        &[
            ("code", &code),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ],
    )
    .await
    .redirect("/login?password_set=true");
    assert!(!member.0.contains_key("gonof_session"));
    let (uid, _, _, password) = f.member("no-password-member").await;
    assert!(password.is_some());
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.password.set'"
        ))
        .await,
        1
    );
    let grant = f
        .identity
        .login("no-password-member", PASSWORD)
        .await
        .unwrap();
    let mut member = Cookies::session(&grant.session_token);

    // Rebinding: the new number's code plus the current password.
    f.age_codes().await;
    post(
        &f,
        "/my/account/mobile/send",
        &mut member,
        &[("mobile", OTHER)],
    )
    .await
    .redirect("/my/account#mobile");
    let (purpose, code) = f.latest_sms("+8613900139000");
    assert_eq!(purpose, "bind");
    let page = get(&f, "/my/account", &mut member).await;
    assert!(
        page.html.contains("The code was sent to 139****9000."),
        "{}",
        page.html
    );
    let wrong_password = post(
        &f,
        "/my/account/mobile",
        &mut member,
        &[("code", &code), ("current_password", "wrong-password")],
    )
    .await;
    assert!(
        wrong_password
            .html
            .contains(Lang::EN.account_error_wrong_password())
    );
    post(
        &f,
        "/my/account/mobile",
        &mut member,
        &[("code", &code), ("current_password", PASSWORD)],
    )
    .await
    .redirect("/my/account?mobile_bound=true#mobile");
    assert_eq!(
        f.member("no-password-member").await.1.as_deref(),
        Some("+8613900139000")
    );
    let change_log: String = db::query_scalar(
        "SELECT data FROM gonof_common_log WHERE uid=? AND type='identity.mobile.change'",
    )
    .bind(uid)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(change_log.contains("139****9000") && change_log.contains("138****8000"));

    // Another account cannot bind the same number.
    let admin_cookies = &mut Cookies::session(&f.admin.session_token);
    f.age_codes().await;
    post(
        &f,
        "/my/account/mobile/send",
        admin_cookies,
        &[("mobile", OTHER)],
    )
    .await;
    let (_, code) = f.latest_sms("+8613900139000");
    let taken = post(
        &f,
        "/my/account/mobile",
        admin_cookies,
        &[("code", &code), ("current_password", "irrelevant")],
    )
    .await;
    assert!(
        taken.html.contains("is linked to another account")
            || taken.html.contains(Lang::EN.account_error_wrong_password()),
        "{}",
        taken.html
    );

    // Accounts without email cannot unbind (they could not recover the password after unbinding); those with email
    // bound can.
    let refused = post(
        &f,
        "/my/account/mobile/unbind",
        &mut member,
        &[("current_password", PASSWORD)],
    )
    .await;
    assert!(
        refused.html.contains("Please add an email first"),
        "{}",
        refused.html
    );
    f.execute(&format!(
        "UPDATE gonof_common_member SET email='mobile-owner@example.test',emailstatus=1 WHERE uid={uid}"
    ))
    .await;
    post(
        &f,
        "/my/account/mobile/unbind",
        &mut member,
        &[("current_password", PASSWORD)],
    )
    .await
    .redirect("/my/account?mobile_unbound=true#mobile");
    assert_eq!(f.member("no-password-member").await.1, None);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid={uid} AND type='identity.mobile.unbind'"
        ))
        .await,
        1
    );
    // Binding, rebinding, unbinding and setting a password all queue security alerts (when there is a verified email).
    assert!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='security_notice' AND aggregate_id={uid}"
        ))
        .await
            >= 1
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn participation_follows_the_domain_and_the_admin_page_saves_it() {
    let f = Fixture::new().await;
    // A member with a verified email and no mobile number bound.
    let invite = f
        .identity
        .generate_invite(f.admin.member.uid, "participation-member")
        .await
        .unwrap();
    let grant = f
        .identity
        .register(
            "mail-member",
            "mail-member@example.test",
            PASSWORD,
            &invite.code,
        )
        .await
        .unwrap();
    let uid = grant.member.uid;
    // While the email is unverified no phone number can be added: no code is sent, and binding with a code that
    // went out anyway is refused under the member row lock.
    let mut member = Cookies::session(&grant.session_token);
    let refused = post(
        &f,
        "/my/account/mobile/send",
        &mut member,
        &[("mobile", "13700137000")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.html);
    assert!(
        refused
            .html
            .contains(Lang::EN.account_error_verify_email_first())
    );
    f.identity
        .send_sms_code(
            "13700137000",
            identity::phone::SmsPurpose::Bind,
            "192.0.2.1",
        )
        .await
        .unwrap();
    let (_, code) = f.latest_sms("+8613700137000");
    assert!(matches!(
        f.identity
            .bind_mobile(&grant.session_token, PASSWORD, "13700137000", &code)
            .await,
        Err(identity::IdentityError::Conflict(_))
    ));
    f.age_codes().await;
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={uid}"
    ))
    .await;
    assert_eq!(
        get(&f, "/my/invites", &mut member).await.status,
        StatusCode::OK
    );

    // The admin area first ticks no participation condition, then changes it to "mobile number bound".
    let mut admin = Cookies::session(&f.admin.session_token);
    let page = get(&f, "/manage/login", &mut admin).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.html);
    assert!(
        page.html.contains("data-sms-local"),
        "local delivery shows a notice"
    );
    let limits = [
        ("sms_mobile_interval_seconds", "60"),
        ("sms_mobile_daily_limit", "5"),
        ("sms_ip_hourly_limit", "10"),
        ("sms_site_daily_limit", "500"),
        ("sms_code_ttl_minutes", "5"),
        ("sms_max_attempts", "5"),
        ("external_two_factor", "require"),
        ("method_sms", "1"),
    ];
    post(&f, "/manage/login", &mut admin, &limits)
        .await
        .redirect("/manage/login?saved=1");
    assert_eq!(
        test_support_value(&f.db, "settings.login.participation").await,
        ""
    );
    // Nothing ticked means nothing is required: a member with an unverified email and no phone number may still
    // create invites and post, and no notice bar asks for anything.
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=0 WHERE uid={uid}"
    ))
    .await;
    assert_eq!(
        get(&f, "/my/invites", &mut member).await.status,
        StatusCode::OK
    );
    let home = get(&f, "/", &mut member).await;
    assert!(
        !home.html.contains(Lang::EN.frame_verify_banner_email()),
        "{}",
        home.html
    );
    // This test publishes twice on the member's first day.
    set_setting(&f.db, "settings.posting.first_day_threads", "2")
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let topic = || NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid: 1,
        title: "Participation requirement".into(),
        body: "Body text".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    };
    let unrestricted = gono_forum_web::site::Site {
        participation: Participation::parse(""),
        ..Default::default()
    };
    gono_forum_web::site::scope(unrestricted, writer.publish(uid, topic()))
        .await
        .unwrap();
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={uid}"
    ))
    .await;
    let mut fields = limits.to_vec();
    fields.push(("participation_mobile", "1"));
    post(&f, "/manage/login", &mut admin, &fields)
        .await
        .redirect("/manage/login?saved=1");
    assert_eq!(
        test_support_value(&f.db, "settings.login.participation").await,
        "mobile"
    );

    // Member pages and write entry points both follow the new condition: the invite code page is 403, and the notice
    // bar says to bind a mobile number.
    let refused = get(&f, "/my/invites", &mut member).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(
        refused
            .html
            .contains("This site needs a phone number first")
    );
    let home = get(&f, "/", &mut member).await;
    assert!(
        home.html
            .contains("Add a phone number to create invites and post."),
        "{}",
        home.html
    );
    let only_mobile = gono_forum_web::site::Site {
        participation: Participation::parse("mobile"),
        ..Default::default()
    };
    let blocked =
        gono_forum_web::site::scope(only_mobile.clone(), writer.publish(uid, topic())).await;
    assert!(
        matches!(
            blocked,
            Err(ContentWriteError::Forbidden(
                "This site needs a phone number first"
            ))
        ),
        "{blocked:?}"
    );
    f.execute(&format!(
        "UPDATE gonof_common_member SET mobile='+8613700137000' WHERE uid={uid}"
    ))
    .await;
    gono_forum_web::site::scope(only_mobile, writer.publish(uid, topic()))
        .await
        .unwrap();
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn other_domains_can_require_their_own_conditions() {
    let f = Fixture::new().await;
    // Turn on multi-domain: the main domain follows the site-wide condition (email), demo2 customizes it to
    // "mobile number bound".
    for sql in [
        format!("INSERT INTO gonof_common_site_domain(site_url,dateline) VALUES('{TEST_SITE_URL}',1)"),
        format!(
            "INSERT INTO gonof_common_site_domain(site_url,site_name,login_methods,participation,dateline) VALUES('{DEMO2}','Second Site','','mobile',1)"
        ),
        "INSERT INTO gonof_common_site_domain_forum(domain_id,fid) SELECT d.domain_id,f.fid FROM gonof_common_site_domain d JOIN gonof_forum_forum f ON f.parent_fid IS NULL".to_owned(),
    ] {
        f.execute(&sql).await;
    }
    set_setting(&f.db, "settings.domains.enabled", "true")
        .await
        .unwrap();
    f.services.global.clear_local();
    let invite = f
        .identity
        .generate_invite(f.admin.member.uid, "domain-member")
        .await
        .unwrap();
    let grant = f
        .identity
        .register(
            "domain-member",
            "domain-member@example.test",
            PASSWORD,
            &invite.code,
        )
        .await
        .unwrap();
    let uid = grant.member.uid;
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={uid}"
    ))
    .await;
    let mut member = Cookies::session(&grant.session_token);
    let main = request(&f, TEST_SITE_URL, "GET", "/my/invites", &mut member, &[]).await;
    assert_eq!(main.status, StatusCode::OK, "{}", main.html);
    let demo2 = request(&f, DEMO2, "GET", "/my/invites", &mut member, &[]).await;
    assert_eq!(demo2.status, StatusCode::FORBIDDEN, "{}", demo2.html);
    assert!(demo2.html.contains("This site needs a phone number first"));
    // demo2's sign-in methods are customized to empty: only password sign-in.
    let login = request(&f, DEMO2, "GET", "/login", &mut Cookies::default(), &[]).await;
    assert!(!login.html.contains("/login/sms\">"), "{}", login.html);
    let main_login = request(
        &f,
        TEST_SITE_URL,
        "GET",
        "/login",
        &mut Cookies::default(),
        &[],
    )
    .await;
    assert!(main_login.html.contains("/login/sms\">"));
    // After binding a mobile number, demo2 can participate too.
    f.execute(&format!(
        "UPDATE gonof_common_member SET mobile='+8613600136000' WHERE uid={uid}"
    ))
    .await;
    let after = request(&f, DEMO2, "GET", "/my/invites", &mut member, &[]).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.html);
    // The admin domain edit page can save these two items: removing the customization goes back to NULL.
    let id: i64 = f
        .scalar(&format!(
            "SELECT domain_id FROM gonof_common_site_domain WHERE site_url='{DEMO2}'"
        ))
        .await;
    let mut admin = Cookies::session(&f.admin.session_token);
    let form = request(
        &f,
        TEST_SITE_URL,
        "GET",
        &format!("/manage/domains/{id}"),
        &mut admin,
        &[],
    )
    .await;
    assert!(
        form.html
            .contains("name=\"participation_custom\" value=\"1\" checked")
    );
    let version = f
        .scalar(&format!(
            "SELECT version FROM gonof_common_site_domain WHERE domain_id={id}"
        ))
        .await
        .to_string();
    let id_text = id.to_string();
    let forums: Vec<i64> =
        db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE parent_fid IS NULL")
            .fetch_all(f.db.executor())
            .await
            .unwrap();
    let forum_fields: Vec<(String, &str)> = forums
        .iter()
        .map(|fid| (format!("forum_{fid}"), "1"))
        .collect();
    let mut fields: Vec<(&str, &str)> = vec![
        ("domain_id", &id_text),
        ("version", &version),
        ("site_url", DEMO2),
        ("site_name", "Second Site"),
        ("canonical", "self"),
        ("enabled", "1"),
        ("login_custom", "1"),
        ("login_sms", "1"),
    ];
    for (key, value) in &forum_fields {
        fields.push((key, value));
    }
    post(&f, "/manage/domains", &mut admin, &fields).await;
    let row = db::query(
        "SELECT login_methods,participation FROM gonof_common_site_domain WHERE domain_id=?",
    )
    .bind(id)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(
        row.try_get::<Option<String>, _>("login_methods")
            .unwrap()
            .as_deref(),
        Some("sms")
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("participation").unwrap(),
        None
    );
    // Customizing with no condition ticked stores the empty set: demo2 then requires nothing, while the main domain
    // still asks for a verified email.
    let version = f
        .scalar(&format!(
            "SELECT version FROM gonof_common_site_domain WHERE domain_id={id}"
        ))
        .await
        .to_string();
    let mut fields: Vec<(&str, &str)> = vec![
        ("domain_id", &id_text),
        ("version", &version),
        ("site_url", DEMO2),
        ("site_name", "Second Site"),
        ("canonical", "self"),
        ("enabled", "1"),
        ("participation_custom", "1"),
    ];
    for (key, value) in &forum_fields {
        fields.push((key, value));
    }
    post(&f, "/manage/domains", &mut admin, &fields).await;
    let participation: Option<String> =
        db::query_scalar("SELECT participation FROM gonof_common_site_domain WHERE domain_id=?")
            .bind(id)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(participation.as_deref(), Some(""));
    f.execute(&format!(
        "UPDATE gonof_common_member SET emailstatus=0,mobile=NULL WHERE uid={uid}"
    ))
    .await;
    let open = request(&f, DEMO2, "GET", "/my/invites", &mut member, &[]).await;
    assert_eq!(open.status, StatusCode::OK, "{}", open.html);
    let main = request(&f, TEST_SITE_URL, "GET", "/my/invites", &mut member, &[]).await;
    assert_eq!(main.status, StatusCode::FORBIDDEN, "{}", main.html);
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn provider_settings_need_the_private_key() {
    let f = Fixture::new().await;
    let mut admin = Cookies::session(&f.admin.session_token);
    let page = get(&f, "/manage/login", &mut admin).await;
    assert!(
        page.html.contains("data-providers-key-missing"),
        "the test configuration has no encryption key"
    );
    let refused = post(
        &f,
        "/manage/login/sms",
        &mut admin,
        &[
            ("revision", "0"),
            ("transport", "aliyun"),
            ("access_key_id", "LTAIexample"),
            ("access_key_secret", "secret"),
            ("sign_name", "Sample Signature"),
            ("template_code", "100001"),
        ],
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.html
    );
    assert!(
        refused.html.contains("[common] encryption_key"),
        "{}",
        refused.html
    );
    // Local delivery needs no key.
    post(
        &f,
        "/manage/login/sms",
        &mut admin,
        &[("revision", "0"), ("transport", "local")],
    )
    .await
    .redirect("/manage/login?saved=sms");
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_settings WHERE skey='login.providers'")
            .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

async fn test_support_value(db: &Database, key: &str) -> String {
    db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=?")
        .bind(key)
        .fetch_one(db.executor())
        .await
        .unwrap()
}
