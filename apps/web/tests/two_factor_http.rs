//! Member two-factor authentication (authenticator and recovery codes): enabling, the second
//! sign-in step, replay protection, lockout, recovery codes, password recovery cannot bypass it, other devices signed
//! out on enabling, API tokens unaffected, command-line reset and key rotation, and no secrets, codes or recovery codes
//! in logs, mail or the outbox.
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content,
    db::{self, Database},
    identity::{self, Identity, LoginOutcome, SecondFactorOutcome, SessionGrant, SessionMetadata},
    router, test_support,
};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

/// Error messages for the two-factor forms on the account security page, placed in the two-factor block.
const TWO_FACTOR_ALERT_START: &str =
    "<p class=\"form-feedback\" role=\"alert\" data-two-factor-message>";
const ORIGIN: &str = "http://127.0.0.1:58833";
const USERNAME: &str = "twofactormember";
const EMAIL: &str = "two-factor@example.test";
const PASSWORD: &str = "Two-factor-original-password-2026!";
const NEW_PASSWORD: &str = "Two-factor-replacement-password-2026!";

struct Fixture {
    services: test_support::TestFixture,
    config: Config,
    db: Database,
    identity: Identity,
    app: Router,
    member: SessionGrant,
}

impl Fixture {
    async fn new() -> Self {
        let services = test_support::TestFixture::migrated().await.unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        // The rate limit bucket shared by two-factor settings and password changes defaults to 5 per account per 300
        // seconds; this case changes things several times in a row, so raise it to the maximum.
        gono_forum_web::test_support::set_setting(
            &db,
            "settings.account.password_change_account_per_5min",
            "20",
        )
        .await
        .unwrap();
        content::seed_forums(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let invite = identity
            .generate_invite(admin.member.uid, "two-factor-member")
            .await
            .unwrap();
        let member = identity
            .register(USERNAME, EMAIL, PASSWORD, &invite.code)
            .await
            .unwrap();
        // Security alerts go only to verified email addresses; API tokens also require verification.
        db::query("UPDATE gonof_common_member SET emailstatus=1 WHERE uid=?")
            .bind(member.member.uid)
            .execute(db.executor())
            .await
            .unwrap();
        let app = router::build(config.clone(), &root, db.clone());
        Self {
            services,
            config,
            db,
            identity,
            app,
            member,
        }
    }

    fn uid(&self) -> i64 {
        self.member.member.uid
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .bind(self.uid())
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }

    fn mail_dir(&self) -> PathBuf {
        self.services.root.join(".local/mail")
    }

    async fn deliver(&self) {
        test_support::deliver_mail(&self.db, &self.mail_dir(), ORIGIN, identity::now())
            .await
            .unwrap();
    }

    /// Security alert emails (.eml delivered to the local directory), sorted by event ID.
    async fn notices(&self) -> Vec<String> {
        let ids: Vec<i64> = db::query_scalar("SELECT eventid FROM gonof_common_outbox WHERE event_type='security_notice' AND aggregate_id=? AND status='sent' ORDER BY eventid")
            .bind(self.uid())
            .fetch_all(self.db.executor())
            .await
            .unwrap();
        ids.into_iter()
            .map(|id| std::fs::read_to_string(self.mail_dir().join(format!("{id}.eml"))).unwrap())
            .collect()
    }

    /// All text that has appeared in the two-factor table, logs, outbox, mail directory and application logs, used to
    /// check whether secrets leaked.
    async fn everything_written(&self) -> String {
        let mut all = String::new();
        for sql in [
            "SELECT data FROM gonof_common_log",
            "SELECT payload FROM gonof_common_outbox",
            "SELECT IFNULL(last_error,'') FROM gonof_common_outbox",
            "SELECT IFNULL(totp_secret,'') FROM gonof_common_member_two_factor",
            "SELECT code_hash FROM gonof_common_member_recovery_code",
        ] {
            let rows: Vec<String> = db::query_scalar(sql)
                .fetch_all(self.db.executor())
                .await
                .unwrap();
            all.push_str(&rows.join("\n"));
        }
        for directory in [self.mail_dir(), self.services.root.join(".local/logs")] {
            for entry in std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
            {
                if entry.path().is_file() {
                    all.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
                }
            }
        }
        all
    }
}

#[derive(Clone, Default)]
struct Cookies(BTreeMap<String, String>);

impl Cookies {
    fn session(raw: &str) -> Self {
        Self(BTreeMap::from([("gonof_session".into(), raw.into())]))
    }
    fn session_token(&self) -> Option<&String> {
        self.0.get("gonof_session")
    }
    async fn csrf(&self, identity: &Identity) -> String {
        if let Some(raw) = self.session_token() {
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
    fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .find(|value| value.starts_with(&format!("{name}=")))
    }
}

async fn request(
    fixture: &Fixture,
    method: &str,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path).header(
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
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, test_support::host_of(ORIGIN))
            .header(
                header::USER_AGENT,
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/141.0.0.0 Safari/537.36",
            );
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let mut req = builder.body(Body::from(form)).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.0.2.44:48321".parse::<SocketAddr>().unwrap(),
    ));
    let response = fixture.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    cookies.update(&headers);
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
    Reply {
        status,
        headers,
        html,
    }
}

async fn get(fixture: &Fixture, path: &str, cookies: &mut Cookies) -> Reply {
    request(fixture, "GET", path, cookies, &[]).await
}

async fn post(
    fixture: &Fixture,
    path: &str,
    cookies: &mut Cookies,
    fields: &[(&str, &str)],
) -> Reply {
    let csrf = cookies.csrf(&fixture.identity).await;
    let mut fields = fields.to_vec();
    fields.push(("_csrf", &csrf));
    request(fixture, "POST", path, cookies, &fields).await
}

fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("missing {start}"))
        + start.len();
    let to = html[from..].find(end).unwrap() + from;
    &html[from..to]
}

fn hidden(html: &str, name: &str) -> String {
    between(html, &format!("name=\"{name}\" value=\""), "\"").to_owned()
}

/// The code `steps` time steps before or after the current moment, simulating the authenticator app.
fn code(secret: &str, steps: i64) -> String {
    identity::totp_code_for_test(secret, identity::now() + steps * 30).unwrap()
}

fn recovery_codes(html: &str) -> Vec<String> {
    between(html, "data-recovery-codes>", "</ol>")
        .split("<li><code>")
        .skip(1)
        .map(|item| item.split("</code>").next().unwrap().to_owned())
        .collect()
}

/// Goes through the enabling flow on the account security page; returns (Base32 secret, recovery codes). Enabling uses
/// the previous time step's code, so the sign-in right after can use the current step's code.
async fn enable(fixture: &Fixture, cookies: &mut Cookies) -> (String, Vec<String>) {
    let setup = post(
        fixture,
        "/my/account/two-factor/setup",
        cookies,
        &[("current_password", PASSWORD)],
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.html);
    assert!(setup.html.contains("data-two-factor-qr><svg "));
    assert!(setup.html.contains("otpauth://totp/"));
    let secret = between(&setup.html, "data-two-factor-secret>", "</code>").to_owned();
    assert_eq!(
        secret.split(' ').count(),
        8,
        "32 Base32 characters in groups of 4"
    );
    let token = hidden(&setup.html, "token");
    // The first setup step does not write to the database.
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        0
    );
    let wrong = post(
        fixture,
        "/my/account/two-factor/enable",
        cookies,
        &[("token", &token), ("code", "000000")],
    )
    .await;
    if wrong.status != StatusCode::SEE_OTHER {
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(wrong.html.contains("Wrong code"));
        assert!(
            wrong.html.contains(&secret),
            "after a wrong code the same QR code is offered again"
        );
    }
    let enabled = post(
        fixture,
        "/my/account/two-factor/enable",
        cookies,
        &[("token", &token), ("code", &code(&secret, -1))],
    )
    .await;
    enabled.redirect("/my/account/recovery-codes?enabled=true");
    let reveal = enabled.set_cookie("gonof_recovery").unwrap();
    assert!(reveal.contains("Path=/my/account/recovery-codes; HttpOnly"));
    let page = get(fixture, "/my/account/recovery-codes?enabled=true", cookies).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.html.contains("Two-step verification is on"));
    let codes = recovery_codes(&page.html);
    assert_eq!(codes.len(), identity::RECOVERY_CODE_COUNT);
    assert!(
        codes
            .iter()
            .all(|code| code.len() == 19 && code.matches('-').count() == 3)
    );
    // Shown only once: the display Cookie has been cleared, so it does not appear after a refresh.
    assert!(!cookies.0.contains_key("gonof_recovery"));
    let again = get(fixture, "/my/account/recovery-codes", cookies).await;
    assert!(again.html.contains("data-recovery-empty"));
    assert!(!again.html.contains(&codes[0]));
    (secret, codes)
}

/// Signs in anonymously up to the second step: returns a browser carrying the awaiting-second-step Cookie.
async fn password_step(fixture: &Fixture, password: &str) -> Cookies {
    let mut cookies = Cookies::default();
    get(fixture, "/login", &mut cookies).await;
    let reply = post(
        fixture,
        "/login",
        &mut cookies,
        &[
            ("username", USERNAME),
            ("password", password),
            ("return_to", "/my"),
        ],
    )
    .await;
    reply.redirect("/login/verify?return_to=%2Fmy");
    assert!(
        cookies.session_token().is_none(),
        "a correct password alone issues no session"
    );
    assert!(cookies.0.contains_key("gonof_2fa"));
    let page = get(fixture, "/login/verify?return_to=%2Fmy", &mut cookies).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.html.contains("Two-step verification"));
    cookies
}

async fn second_step(fixture: &Fixture, cookies: &mut Cookies, answer: &str) -> Reply {
    post(
        fixture,
        "/login/verify",
        cookies,
        &[("code", answer), ("return_to", "/my")],
    )
    .await
}

#[tokio::test]
async fn totp_login_accepts_fresh_codes_and_rejects_wrong_expired_and_replayed_ones() {
    let fixture = Fixture::new().await;
    let mut browser = Cookies::session(&fixture.member.session_token);
    let page = get(&fixture, "/my/account", &mut browser).await;
    assert!(page.html.contains("data-two-factor-status=\"disabled\""));
    let (secret, codes) = enable(&fixture, &mut browser).await;
    let page = get(&fixture, "/my/account", &mut browser).await;
    assert!(page.html.contains("data-two-factor-status=\"enabled\""));
    assert!(page.html.contains("10 recovery codes left"));

    // The login() used by tests and the command line cannot bypass the second step either.
    assert!(matches!(
        fixture.identity.login(USERNAME, PASSWORD).await,
        Err(identity::IdentityError::Validation(_))
    ));
    // A wrong password is still a sign-in failure and does not enter the second step.
    let mut stranger = Cookies::default();
    get(&fixture, "/login", &mut stranger).await;
    let wrong_password = post(
        &fixture,
        "/login",
        &mut stranger,
        &[("username", USERNAME), ("password", "Wrong-password-2026!")],
    )
    .await;
    assert_eq!(wrong_password.status, StatusCode::UNAUTHORIZED);
    assert!(!stranger.0.contains_key("gonof_2fa"));

    let mut login = password_step(&fixture, PASSWORD).await;
    // A wrong code.
    let wrong = second_step(&fixture, &mut login, "123456").await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert!(wrong.html.contains("Wrong code or recovery code"));
    // A code was entered: the recovery code section stays collapsed, and focus stays on the code input box.
    assert!(wrong.html.contains("<details class=\"auth-fallback\">"));
    assert!(wrong.html.contains("maxlength=\"8\" required autofocus>"));
    // An expired code: the code from three time steps ago (90 seconds ago) is no longer in the window of 1 step either
    // side.
    let expired = second_step(&fixture, &mut login, &code(&secret, -3)).await;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    assert!(login.session_token().is_none());
    // The current code: issues the session, returns to the original page, and clears the awaiting-second-step Cookie.
    let current = code(&secret, 0);
    let granted = second_step(&fixture, &mut login, &current).await;
    granted.redirect("/my");
    assert!(login.session_token().is_some());
    assert!(!login.0.contains_key("gonof_2fa"));
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        0,
        "one success resets the failure count"
    );
    let logged = db::query_scalar::<String>("SELECT data FROM gonof_common_log WHERE uid=? AND type='identity.login' ORDER BY id DESC LIMIT 1")
        .bind(fixture.uid())
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
    assert!(logged.contains("\"method\":\"totp\""));

    // Replay: the same code used once more on another device.
    let mut replay = password_step(&fixture, PASSWORD).await;
    let replayed = second_step(&fixture, &mut replay, &current).await;
    assert_eq!(
        replayed.status,
        StatusCode::UNAUTHORIZED,
        "the same code cannot be used twice"
    );
    // After using a new code, earlier codes are voided too.
    let older = second_step(&fixture, &mut replay, &code(&secret, -1)).await;
    assert_eq!(older.status, StatusCode::UNAUTHORIZED);
    // The next time step's code (phone clock slightly fast) can be used.
    second_step(&fixture, &mut replay, &code(&secret, 1))
        .await
        .redirect("/my");

    // When the awaiting-second-step credential expires or is lost, the password must be entered again.
    let mut lost = Cookies::default();
    get(&fixture, "/login", &mut lost).await;
    get(&fixture, "/login/verify", &mut lost)
        .await
        .redirect("/login?second_factor_expired=true");
    second_step(&fixture, &mut lost, &code(&secret, 0))
        .await
        .redirect("/login?second_factor_expired=true");

    // The authenticator secret is stored only as ciphertext; logs, mail, the outbox and application logs contain no
    // secret, codes or recovery codes.
    fixture.deliver().await;
    let written = fixture.everything_written().await;
    let compact = secret.replace(' ', "");
    assert!(!written.contains(&compact) && !written.contains(&secret));
    assert!(!written.contains(&hex_secret(&compact)));
    for used in [current.as_str(), "123456"] {
        assert!(!written.contains(&format!("\"{used}\"")), "{used}");
    }
    for code in &codes {
        assert!(!written.contains(code.as_str()) && !written.contains(&code.replace('-', "")));
    }
    let stored = db::query_scalar::<String>(
        "SELECT totp_secret FROM gonof_common_member_two_factor WHERE uid=?",
    )
    .bind(fixture.uid())
    .fetch_one(fixture.db.executor())
    .await
    .unwrap();
    assert!(stored.starts_with("v1."));
    // Enabling sent one security alert; the email contains no secrets, only the event and a link.
    let notices = fixture.notices().await;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("turned on two-step verification"));
    assert!(notices[0].contains("/my/account"));
    assert!(notices[0].contains("Chrome · macOS"));
}

fn hex_secret(base32: &str) -> String {
    const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut buffer, mut bits, mut out) = (0_u32, 0_u32, String::new());
    for c in base32.chars() {
        buffer = (buffer << 5) | ALPHABET.find(c).unwrap() as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push_str(&format!("{:02x}", (buffer >> bits) & 0xff));
        }
        buffer &= (1 << bits) - 1;
    }
    out
}

#[tokio::test]
async fn wrong_codes_lock_persistently_and_password_reset_clears_the_lock_not_the_requirement() {
    let fixture = Fixture::new().await;
    let mut browser = Cookies::session(&fixture.member.session_token);
    let (secret, _) = enable(&fixture, &mut browser).await;
    let mut login = password_step(&fixture, PASSWORD).await;
    for attempt in 1..=4 {
        let reply = second_step(&fixture, &mut login, "abcd").await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
    }
    let locked = second_step(&fixture, &mut login, "abcd").await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.html.contains("locked until"));
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        5
    );
    let until = fixture
        .scalar("SELECT locked_until FROM gonof_common_member_two_factor WHERE uid=?")
        .await;
    assert!(
        (until - identity::now() - 900).abs() <= 5,
        "the 5th failure locks for 15 minutes"
    );
    // The lockout is stored in the database: with another process (a new Identity) or another device, even a correct
    // code is not accepted.
    let other = Identity::new(
        fixture.db.clone(),
        fixture.config.identity.clone(),
        fixture.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let LoginOutcome::SecondFactor(pending) = other
        .begin_login(USERNAME, PASSWORD, &SessionMetadata::default())
        .await
        .unwrap()
    else {
        panic!("expected the second step");
    };
    assert!(matches!(
        other
            .complete_login(
                &pending.token,
                &code(&secret, 0),
                &SessionMetadata::default()
            )
            .await
            .unwrap(),
        SecondFactorOutcome::Locked { .. }
    ));
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        5,
        "attempts during the lockout are not counted"
    );
    // Entering lockout sends a security alert (the log has the IP, not the entered content).
    fixture.deliver().await;
    let notices = fixture.notices().await;
    assert!(
        notices
            .iter()
            .any(|mail| mail.contains("is locked for a while"))
    );
    let page = get(&fixture, "/my/account", &mut browser).await;
    assert!(page.html.contains("locked until"));

    // Each lockout level is longer than the last: 5 more errors lock for 1 hour.
    db::query("UPDATE gonof_common_member_two_factor SET locked_until=0 WHERE uid=?")
        .bind(fixture.uid())
        .execute(fixture.db.executor())
        .await
        .unwrap();
    for _ in 0..5 {
        second_step(&fixture, &mut login, "999999x").await;
    }
    let until = fixture
        .scalar("SELECT locked_until FROM gonof_common_member_two_factor WHERE uid=?")
        .await;
    assert!(
        (until - identity::now() - 3600).abs() <= 5,
        "the 10th failure locks for 1 hour"
    );

    // Recovering and resetting the password: the failure count is reset, but two-factor authentication stays, and
    // signing in with the new password still requires the second step (no bypass).
    fixture
        .identity
        .request_password_reset(EMAIL)
        .await
        .unwrap();
    let token: String = {
        let payload = db::query_scalar::<String>("SELECT payload FROM gonof_common_outbox WHERE event_type='password_reset' AND aggregate_id=? ORDER BY eventid DESC LIMIT 1")
            .bind(fixture.uid())
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["token"]
            .as_str()
            .unwrap()
            .into()
    };
    fixture
        .identity
        .reset_password(&token, NEW_PASSWORD)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts + locked_until FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        0
    );
    let mut after_reset = password_step(&fixture, NEW_PASSWORD).await;
    second_step(&fixture, &mut after_reset, &code(&secret, 1))
        .await
        .redirect("/my");
}

#[tokio::test]
async fn recovery_codes_work_once_and_regenerating_voids_the_old_batch() {
    let fixture = Fixture::new().await;
    let mut browser = Cookies::session(&fixture.member.session_token);
    let (_, codes) = enable(&fixture, &mut browser).await;
    let mut login = password_step(&fixture, PASSWORD).await;
    // Recovery codes are case-insensitive and may omit hyphens.
    let used = codes[3].to_uppercase().replace('-', " ");
    second_step(&fixture, &mut login, &used)
        .await
        .redirect("/my/account?recovery_login=true");
    let page = get(&fixture, "/my/account?recovery_login=true", &mut login).await;
    assert!(
        page.html
            .contains("You just logged in with a recovery code")
    );
    assert!(page.html.contains("9 recovery codes left"));
    // The same recovery code can be used only once.
    let mut again = password_step(&fixture, PASSWORD).await;
    let reused = second_step(&fixture, &mut again, &codes[3]).await;
    assert_eq!(reused.status, StatusCode::UNAUTHORIZED);
    // A wrong recovery code was entered: when re-rendering, the recovery code section stays expanded and focus lands on
    // the recovery code input box, so it need not be clicked again.
    assert!(
        reused
            .html
            .contains("<details class=\"auth-fallback\" open>")
    );
    assert!(reused.html.contains("maxlength=\"32\" required autofocus>"));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_recovery_code WHERE uid=?")
            .await,
        9
    );

    // Regenerating requires the current password; the whole old batch is voided, and the new one is shown only once.
    let wrong = post(
        &fixture,
        "/my/account/recovery-codes",
        &mut browser,
        &[("current_password", "Wrong-password-2026!")],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.html.contains("The current password is wrong"));
    assert!(
        wrong.html.contains(TWO_FACTOR_ALERT_START),
        "the error for regenerating recovery codes shows in the two-step verification block, not at the password form"
    );
    let regenerated = post(
        &fixture,
        "/my/account/recovery-codes",
        &mut browser,
        &[("current_password", PASSWORD)],
    )
    .await;
    regenerated.redirect("/my/account/recovery-codes?regenerated=true");
    let page = get(
        &fixture,
        "/my/account/recovery-codes?regenerated=true",
        &mut browser,
    )
    .await;
    let fresh = recovery_codes(&page.html);
    assert_eq!(fresh.len(), 10);
    assert!(fresh.iter().all(|code| !codes.contains(code)));
    let stale = second_step(&fixture, &mut again, &codes[4]).await;
    assert_eq!(
        stale.status,
        StatusCode::UNAUTHORIZED,
        "the old recovery codes no longer work"
    );
    second_step(&fixture, &mut again, &fresh[0])
        .await
        .redirect("/my/account?recovery_login=true");

    // Security alerts: enabling, signing in with a recovery code (twice), regenerating; the emails contain no recovery
    // codes.
    fixture.deliver().await;
    let notices = fixture.notices().await;
    assert_eq!(notices.len(), 4);
    assert!(notices.iter().any(|mail| mail.contains("9 codes are left")));
    assert!(
        notices
            .iter()
            .any(|mail| mail.contains("generated new recovery codes"))
    );
    let written = fixture.everything_written().await;
    for code in codes.iter().chain(&fresh) {
        assert!(!written.contains(code.as_str()) && !written.contains(&code.replace('-', "")));
    }
}

#[tokio::test]
async fn enabling_signs_out_other_devices_keeps_this_one_and_leaves_api_keys_alone() {
    let fixture = Fixture::new().await;
    let other_device = fixture.identity.login(USERNAME, PASSWORD).await.unwrap();
    let api = fixture
        .identity
        .create_api_key(&fixture.member.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    let mut browser = Cookies::session(&fixture.member.session_token);
    let (secret, codes) = enable(&fixture, &mut browser).await;
    // The current device switches to a new session and stays signed in; the original session and other devices'
    // sessions are all invalidated (credentials version +1).
    let current = browser.session_token().unwrap().clone();
    assert_ne!(current, fixture.member.session_token);
    assert!(fixture.identity.session(&current).await.unwrap().is_some());
    for old in [&fixture.member.session_token, &other_device.session_token] {
        assert!(fixture.identity.session(old).await.unwrap().is_none());
    }
    assert_eq!(
        get(&fixture, "/my/account", &mut browser).await.status,
        StatusCode::OK
    );
    // API tokens are unaffected.
    assert!(
        fixture
            .identity
            .authenticate_api_key(&api.secret)
            .await
            .unwrap()
            .is_some()
    );

    // Turning off: requires the password and passing the second step once more; wrong input counts toward the failure
    // count.
    let mut device = password_step(&fixture, PASSWORD).await;
    second_step(&fixture, &mut device, &code(&secret, 0))
        .await
        .redirect("/my");
    let wrong = post(
        &fixture,
        "/my/account/two-factor/disable",
        &mut browser,
        &[("current_password", PASSWORD), ("code", "000000")],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    // The message sits right next to the two-factor forms; the password change form at the top of the page has no
    // message.
    assert!(wrong.html.contains(&format!(
        "{TWO_FACTOR_ALERT_START}Wrong code or recovery code. Please try again</p>"
    )));
    let password_form = &wrong.html[wrong.html.find("action=\"/my/account/password\"").unwrap()
        ..wrong.html.find("id=\"two-factor\"").unwrap()];
    assert!(!password_form.contains("role=\"alert\""));
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        1
    );
    let no_password = post(
        &fixture,
        "/my/account/two-factor/disable",
        &mut browser,
        &[
            ("current_password", "Wrong-password-2026!"),
            ("code", &codes[0]),
        ],
    )
    .await;
    assert_eq!(no_password.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_recovery_code WHERE uid=?")
            .await,
        10,
        "with a wrong password the recovery code is neither checked nor used up"
    );
    post(
        &fixture,
        "/my/account/two-factor/disable",
        &mut browser,
        &[("current_password", PASSWORD), ("code", &codes[0])],
    )
    .await
    .redirect("/my/account?two_factor_disabled=true");
    for table in [
        "gonof_common_member_two_factor",
        "gonof_common_member_recovery_code",
    ] {
        assert_eq!(
            fixture
                .scalar(&format!("SELECT COUNT(*) FROM {table} WHERE uid=?"))
                .await,
            0
        );
    }
    // Turning off signs no device out and API tokens work as usual; afterwards password sign-in issues the session
    // directly.
    for session in [&current, device.session_token().unwrap()] {
        assert!(fixture.identity.session(session).await.unwrap().is_some());
    }
    assert!(
        fixture
            .identity
            .authenticate_api_key(&api.secret)
            .await
            .unwrap()
            .is_some()
    );
    assert!(fixture.identity.login(USERNAME, PASSWORD).await.is_ok());
    let page = get(
        &fixture,
        "/my/account?two_factor_disabled=true",
        &mut browser,
    )
    .await;
    assert!(page.html.contains("Two-step verification is off"));
    fixture.deliver().await;
    assert!(
        fixture
            .notices()
            .await
            .iter()
            .any(|mail| mail.contains("turned off two-step verification"))
    );
}

fn key_id(key: &str) -> String {
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&key[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    Sha256::digest(&bytes)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn command_line_reset_and_key_rotation() {
    let fixture = Fixture::new().await;
    let mut browser = Cookies::session(&fixture.member.session_token);
    let (secret, _) = enable(&fixture, &mut browser).await;
    let stored = || async {
        db::query_scalar::<String>(
            "SELECT totp_secret FROM gonof_common_member_two_factor WHERE uid=?",
        )
        .bind(fixture.uid())
        .fetch_one(fixture.db.executor())
        .await
        .unwrap()
    };
    let old_key = fixture.config.identity.two_factor.active_key.clone();
    assert!(
        stored()
            .await
            .starts_with(&format!("v1.{}.", key_id(&old_key)))
    );

    // Rotation: the new key is current and the old key kept for decryption. A successful sign-in re-encrypts with the
    // new key while at it.
    let mut rotated = fixture.config.identity.clone();
    rotated.two_factor.active_key = identity::random_token();
    rotated.two_factor.previous_keys = vec![old_key.clone()];
    let new_key = rotated.two_factor.active_key.clone();
    let rotated = Identity::new(
        fixture.db.clone(),
        rotated,
        fixture.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let report = rotated.rekey_two_factor().await.unwrap();
    assert_eq!((report.rekeyed, report.current), (1, 0));
    assert!(report.unreadable.is_empty());
    assert!(
        stored()
            .await
            .starts_with(&format!("v1.{}.", key_id(&new_key)))
    );
    let report = rotated.rekey_two_factor().await.unwrap();
    assert_eq!((report.rekeyed, report.current), (0, 1));
    // After deleting the old key, sign-in works as usual.
    let mut only_new = fixture.config.identity.clone();
    only_new.two_factor.active_key = new_key;
    only_new.two_factor.previous_keys = vec![];
    let only_new = Identity::new(
        fixture.db.clone(),
        only_new,
        fixture.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let LoginOutcome::SecondFactor(pending) = only_new
        .begin_login(USERNAME, PASSWORD, &SessionMetadata::default())
        .await
        .unwrap()
    else {
        panic!("expected the second step");
    };
    assert!(matches!(
        only_new
            .complete_login(
                &pending.token,
                &code(&secret, 0),
                &SessionMetadata::default()
            )
            .await
            .unwrap(),
        SecondFactorOutcome::Granted { .. }
    ));
    // The original key can no longer decrypt the new ciphertext: the authenticator temporarily cannot verify, and it is
    // not counted as a failure.
    let LoginOutcome::SecondFactor(pending) = fixture
        .identity
        .begin_login(USERNAME, PASSWORD, &SessionMetadata::default())
        .await
        .unwrap()
    else {
        panic!("expected the second step");
    };
    assert!(matches!(
        fixture
            .identity
            .complete_login(
                &pending.token,
                &code(&secret, 1),
                &SessionMetadata::default()
            )
            .await
            .unwrap(),
        SecondFactorOutcome::Unavailable
    ));
    assert_eq!(
        fixture
            .scalar("SELECT failed_attempts FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        0
    );

    // Command-line reset: deletes all two-factor data, signs out all devices, revokes API tokens, writes an audit entry
    // (source cli), sends an alert.
    let api = fixture
        .identity
        .create_api_key(browser.session_token().unwrap(), PASSWORD, 86_400)
        .await
        .unwrap();
    let session = browser.session_token().unwrap().clone();
    assert!(matches!(
        fixture.identity.reset_two_factor(fixture.uid(), " ").await,
        Err(identity::IdentityError::Validation(_))
    ));
    assert!(
        fixture
            .identity
            .reset_two_factor(fixture.uid(), "Phone lost; identity checked in person")
            .await
            .unwrap()
    );
    assert!(fixture.identity.session(&session).await.unwrap().is_none());
    assert!(
        fixture
            .identity
            .authenticate_api_key(&api.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_member_two_factor WHERE uid=?")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_log WHERE uid=? AND type='identity.two_factor.reset' AND source='cli' AND operationuid IS NULL")
            .await,
        1
    );
    assert!(
        !fixture
            .identity
            .reset_two_factor(fixture.uid(), "Run once more")
            .await
            .unwrap(),
        "nothing changes when it is not on"
    );
    assert!(fixture.identity.login(USERNAME, PASSWORD).await.is_ok());
    fixture.deliver().await;
    let notices = fixture.notices().await;
    let reset = notices
        .iter()
        .find(|mail| mail.contains("had its two-step verification reset"))
        .unwrap();
    assert!(
        !reset.contains("checked in person"),
        "the reason goes only to the audit log, not to the mail"
    );
}
