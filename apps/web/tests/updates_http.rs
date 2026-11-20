//! Tools & maintenance › Version updates: the page, a check against a local release directory, and one-click
//! upgrade refused with reasons in a source build. The download, digest check, unpacking and binary swap are covered
//! by the unit tests in `updates.rs`.
#[path = "support/http_auth.rs"]
mod http_auth;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{db, identity, test_support::TestFixture, updates};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ADMIN: i64 = 9911;
const MEMBER: i64 = 9912;

struct Fixture {
    f: TestFixture,
    app: Router,
    admin: identity::SessionGrant,
    member: identity::SessionGrant,
    source: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_updater(updates::Updater::default()).await
    }

    async fn with_updater(updater: updates::Updater) -> Self {
        let mut f = TestFixture::migrated().await.unwrap();
        for (uid, name, role) in [(ADMIN, "update-admin", 1), (MEMBER, "update-member", 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid)
                .bind(name)
                .bind(format!("{name}@example.test"))
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
        let member = http_auth::login(&identity, &f.db, MEMBER).await;
        // A release directory with the GitHub layout, listing a newer package for every platform.
        let source = f.root.join("releases");
        std::fs::create_dir_all(source.join("latest/download")).unwrap();
        let digest = "ab".repeat(32);
        std::fs::write(
            source.join("latest/download/checksums.txt"),
            format!(
                "{digest}  gono-forum_99.0.0_linux_amd64.tar.gz\n{digest}  gono-forum_99.0.0_linux_arm64.tar.gz\n"
            ),
        )
        .unwrap();
        f.config.update.releases_url = format!("file://{}", source.display());
        let app = gono_forum_web::router::build_with_updater(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
            updater,
        );
        Self {
            f,
            app,
            admin,
            member,
            source,
        }
    }

    async fn send(
        &self,
        grant: &identity::SessionGrant,
        method: &str,
        path: &str,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(path).header(
            header::COOKIE,
            format!("gonof_session__port58833={}", grant.session_token),
        );
        let body = if method == "POST" {
            request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("_csrf", &grant.csrf_token)
                .finish()
        } else {
            String::new()
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
}

#[tokio::test]
async fn the_update_page_checks_a_release_source_and_explains_why_it_cannot_upgrade() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture.send(&fixture.admin, "GET", "/manage/updates").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(&format!("<code>{}</code>", gono_forum_web::VERSION)),
        "{body}"
    );
    assert!(body.contains("Not checked yet."), "{body}");
    assert!(body.contains(&fixture.source.display().to_string()));
    // Only administrators.
    let (status, _) = fixture
        .send(&fixture.member, "GET", "/manage/updates")
        .await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = fixture
        .send(&fixture.member, "POST", "/manage/updates/check")
        .await;
    assert_ne!(status, StatusCode::SEE_OTHER);

    let (status, _) = fixture
        .send(&fixture.admin, "POST", "/manage/updates/check")
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = fixture.send(&fixture.admin, "GET", "/manage/updates").await;
    if updates::platform().is_some() {
        assert!(
            body.contains("version <code>99.0.0</code> is available"),
            "{body}"
        );
        // A source build under the test runner: the upgrade section explains why it is unavailable and offers no
        // button.
        assert!(
            body.contains("This instance cannot upgrade in one click")
                && body.contains("built from source"),
            "{body}"
        );
        assert!(!body.contains("<button>Upgrade to 99.0.0</button>"));
        // The upgrade form names the release the page shows, so that it installs that one and not whatever a later
        // check found.
        assert!(
            body.contains(r#"<input type="hidden" name="version" value="99.0.0">"#)
                && body.contains(&format!(
                    r#"<input type="hidden" name="sha256" value="{}">"#,
                    "ab".repeat(32)
                )),
            "{body}"
        );
    } else {
        assert!(
            body.contains("failed: There are no release packages for this platform"),
            "{body}"
        );
    }
    let (status, body) = fixture
        .send(&fixture.admin, "POST", "/manage/updates/apply")
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("built from source"), "{body}");

    // A source that cannot be read reports the error instead of a version.
    std::fs::remove_file(fixture.source.join("latest/download/checksums.txt")).unwrap();
    fixture
        .send(&fixture.admin, "POST", "/manage/updates/check")
        .await;
    let (_, body) = fixture.send(&fixture.admin, "GET", "/manage/updates").await;
    assert!(body.contains(" failed: "), "{body}");
    fixture.f.cleanup().await.unwrap();
}

#[tokio::test]
async fn while_an_upgrade_runs_the_page_waits_for_the_new_version_and_forgets_the_started_notice_after()
 {
    let updater = updates::Updater::default();
    let fixture = Fixture::with_updater(updater.clone()).await;
    const WAIT: &str = "data-upgrade-wait";
    const NOSCRIPT_REFRESH: &str =
        r#"<noscript><meta http-equiv="refresh" content="3"></noscript>"#;
    const STARTED: &str = "The upgrade has started.";

    // Nothing runs: no refresh, no waiting, and an address left over from the last upgrade shows no "has started".
    let (status, body) = fixture
        .send(&fixture.admin, "GET", "/manage/updates?notice=started")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(STARTED), "{body}");
    assert!(
        !body.contains(WAIT) && !body.contains("http-equiv"),
        "{body}"
    );

    // While it runs, the page marks itself for site.js, which reloads only once the forum answers; without JavaScript
    // the meta refresh reloads it every 3 seconds as before.
    updater.set_running("Downloading gono-forum_99.0.0_linux_amd64.tar.gz");
    let (status, body) = fixture
        .send(&fixture.admin, "GET", "/manage/updates?notice=started")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(STARTED), "{body}");
    assert!(
        body.contains("Downloading gono-forum_99.0.0_linux_amd64.tar.gz"),
        "{body}"
    );
    assert!(body.contains(WAIT), "{body}");
    assert!(body.contains(NOSCRIPT_REFRESH), "{body}");
    assert_eq!(body.matches("http-equiv").count(), 1, "{body}");
    fixture.f.cleanup().await.unwrap();
}

/// The outcome of the last upgrade, as `data/upgrade/last` records it, is shown in words; an upgrade the old version
/// gave up after it was cut short before the switch says to upgrade again.
#[tokio::test]
async fn the_update_page_words_the_last_upgrade_outcome() {
    let fixture = Fixture::new().await;
    let t = gono_forum_web::i18n::current();
    let state = fixture.f.root.join(updates::STATE_DIRECTORY);
    std::fs::create_dir_all(&state).unwrap();
    for (result, expected) in [
        (
            "upgraded",
            t.maintenance_updates_last_upgraded("9.9.1", "9.9.2"),
        ),
        (
            "interrupted",
            t.maintenance_updates_last_interrupted("9.9.1", "9.9.2"),
        ),
        (
            "rolled_back",
            t.maintenance_updates_last_rolled_back("9.9.1", "9.9.2"),
        ),
        (
            "switched",
            t.maintenance_updates_last_switched("9.9.1", "9.9.2"),
        ),
    ] {
        std::fs::write(
            state.join("last"),
            format!("from=9.9.1\nto=9.9.2\nat=1\nresult={result}\nfinished=2\n"),
        )
        .unwrap();
        let (status, body) = fixture.send(&fixture.admin, "GET", "/manage/updates").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&expected), "{result}: {body}");
    }
    assert!(
        t.maintenance_updates_last_interrupted("9.9.1", "9.9.2")
            .contains("Upgrade again")
    );
    fixture.f.cleanup().await.unwrap();
}
