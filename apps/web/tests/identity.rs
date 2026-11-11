#[path = "support/account_security.rs"]
mod account_security;
#[path = "support/identity_migration.rs"]
mod identity_migration;
#[path = "support/profile_settings.rs"]
mod profile_settings;

use std::path::Path;

use gono_forum_web::{
    db::{self, Database},
    identity::{self, Identity, IdentityConfig, SessionGrant},
};

const PASSWORD: &str = "Contract-password-4839";
const ORIGIN: &str = "http://localhost:58833";

fn test_identity_config() -> IdentityConfig {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static TWO_FACTOR_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mut config = IdentityConfig::default();
    config.cookie.active_key = KEY.get_or_init(identity::random_token).clone();
    config.two_factor.active_key = TWO_FACTOR_KEY.get_or_init(identity::random_token).clone();
    config
}

async fn execute(database: &Database, statement: &str) {
    gono_forum_web::db::query(statement)
        .execute(&database.pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn scalar(database: &Database, statement: &str) -> i64 {
    gono_forum_web::db::query_scalar(statement)
        .fetch_one(&database.pool)
        .await
        .unwrap()
}

async fn registration_counts(database: &Database) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "gonof_common_member",
        "gonof_common_member_token",
        "gonof_common_outbox",
    ] {
        counts.push(scalar(database, &format!("SELECT COUNT(*) FROM {table}")).await);
    }
    counts.push(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_invite WHERE status = 'used'",
        )
        .await,
    );
    counts
}

async fn verification_token(database: &Database, uid: i64) -> String {
    let row = gono_forum_web::db::query("SELECT payload FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type = 'email_verification' ORDER BY eventid DESC LIMIT 1")
    .bind(uid)
    .fetch_one(&database.pool)
    .await
    .unwrap();
    serde_json::from_str::<serde_json::Value>(&db::text(&row, "payload").unwrap()).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn register_member(identity: &Identity, inviter: i64, name: &str) -> SessionGrant {
    let invite = identity.generate_invite(inviter, name).await.unwrap();
    identity
        .register(
            name,
            &format!("{name}@example.test"),
            PASSWORD,
            &invite.code,
        )
        .await
        .unwrap()
}

async fn verify_member(identity: &Identity, database: &Database, uid: i64) {
    let token = verification_token(database, uid).await;
    identity.verify_email(&token).await.unwrap();
}

// Every engine executes this same behavior contract in a fresh, isolated database.
async fn identity_contract(database: Database, root: &Path) {
    database.migrate().await.unwrap();
    database.migrate().await.unwrap();
    identity_migration::assert_identity_migration(&database).await;
    // The administrator generates invite codes for every later case: invite quotas are admin settings (the write gate
    // reads the database), so raise them to the maximum first.
    invite_limits(&database, 100).await;
    // The e-mail sending switch is seeded as off; mail is delivered later, so turn it on as for an existing site,
    // like `TestFixture::migrated` does.
    gono_forum_web::test_support::set_setting(&database, "settings.email.enabled", "true")
        .await
        .unwrap();
    // Built-in mail follows the site language (docs/architecture.md, "Languages"): this contract checks the English
    // subjects of the seeded `en` site.
    let identity = Identity::new(
        database.clone(),
        test_identity_config(),
        root.to_path_buf(),
        gono_forum_web::test_support::detached_global(),
    );
    let admin = assert_bootstrap(&identity, &database, root).await;
    assert_registration(&identity, &database, admin.member.uid).await;
    assert_invitation_limits(&identity, &database, root, admin.member.uid).await;
    assert_verification(&identity, &database, admin.member.uid).await;
    assert_sessions(&identity, &database, admin.member.uid).await;
    assert_delivery(&identity, &database, root, admin.member.uid).await;
    account_security::assert_account_security(&identity, &database, root, admin.member.uid).await;
    profile_settings::assert_profile_settings(&identity, &database, root, admin.member.uid).await;
    assert_sensitive_account_reads(&identity, &database, root, admin.member.uid).await;
}

async fn account_reads<T>(
    expected: usize,
    operation: impl std::future::Future<Output = Result<T, identity::IdentityError>>,
) -> T {
    let profile = std::sync::Arc::new(gono_forum_web::debug::trace::RequestProfile::new());
    let value = gono_forum_web::debug::trace::scope(profile.clone(), operation)
        .await
        .unwrap();
    let queries: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|query| {
            query.source == gono_forum_web::debug::trace::QuerySource::Application
                && query.sql.starts_with("SELECT ")
                && query.sql.contains("FROM gonof_common_member ")
        })
        .collect();
    assert_eq!(
        queries.len(),
        expected,
        "a locked account must not read the password again"
    );
    // The debug trace records the text actually executed: on SQLite the rewriter removes locking reads (write
    // transactions queue as a whole), so only count them on MySQL.
    if !gono_forum_web::test_support::sqlite_tests() {
        assert_eq!(
            queries
                .iter()
                .filter(|query| query.sql.contains("FOR UPDATE"))
                .count(),
            1
        );
    }
    assert!(queries.iter().all(|query| !query.sql.contains("COALESCE")));
    value
}

async fn assert_sensitive_account_reads(
    identity: &Identity,
    database: &Database,
    root: &Path,
    admin: i64,
) {
    let member = register_member(identity, admin, "account-read-contract").await;
    let login = account_reads(2, identity.login("account-read-contract", PASSWORD)).await;
    account_reads(
        2,
        identity.change_password(&login.session_token, PASSWORD, PASSWORD),
    )
    .await;
    let login = identity
        .login("account-read-contract", PASSWORD)
        .await
        .unwrap();
    account_reads(
        2,
        identity.request_password_reset("account-read-contract@example.test"),
    )
    .await;
    let token = |purpose: &'static str| async move {
        let payload: String = gono_forum_web::db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type = ? ORDER BY eventid DESC LIMIT 1")
            .bind(member.member.uid).bind(purpose).fetch_one(database.executor()).await.unwrap();
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    account_reads(
        1,
        identity.reset_password(&token("password_reset").await, PASSWORD),
    )
    .await;
    assert!(
        identity
            .session(&login.session_token)
            .await
            .unwrap()
            .is_none()
    );
    let login = identity
        .login("account-read-contract", PASSWORD)
        .await
        .unwrap();
    account_reads(
        3,
        identity.request_email_change(
            &login.session_token,
            PASSWORD,
            "account-read-new@example.test",
        ),
    )
    .await;
    account_reads(
        1,
        identity.confirm_email_change(&token("email_change").await),
    )
    .await;
    assert!(
        identity
            .session(&login.session_token)
            .await
            .unwrap()
            .is_none()
    );

    let nullable = register_member(identity, admin, "nullable-account-contract").await;
    identity
        .request_password_reset("nullable-account-contract@example.test")
        .await
        .unwrap();
    let reset_payload: String = gono_forum_web::db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type = 'password_reset'")
        .bind(nullable.member.uid).fetch_one(database.executor()).await.unwrap();
    let reset_token = serde_json::from_str::<serde_json::Value>(&reset_payload).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    execute(database, &format!("UPDATE gonof_common_member SET password = NULL, email = NULL, emailstatus = 0, adminid = 0 WHERE uid = {}", nullable.member.uid)).await;
    assert!(
        identity
            .login("nullable-account-contract", PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .change_password(&nullable.session_token, PASSWORD, PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .request_email_change(
                &nullable.session_token,
                PASSWORD,
                "nullable-new@example.test"
            )
            .await
            .is_err()
    );
    assert!(
        identity
            .reset_password(&reset_token, PASSWORD)
            .await
            .is_err()
    );
    let mailbox = root.join(".local/mail");
    gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
        .await
        .unwrap();
    let events = gono_forum_web::db::query("SELECT eventid, status FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type IN ('email_verification', 'password_reset')")
        .bind(nullable.member.uid).fetch_all(database.executor()).await.unwrap();
    assert_eq!(events.len(), 2);
    for event in events {
        assert_eq!(event.try_get::<String, _>("status").unwrap(), "failed");
        assert!(
            !mailbox
                .join(format!(
                    "{}.eml",
                    event.try_get::<i64, _>("eventid").unwrap()
                ))
                .exists()
        );
    }
}

async fn assert_bootstrap(identity: &Identity, database: &Database, root: &Path) -> SessionGrant {
    // The setup wizard creates the first administrator and writes nothing locally; later cases use the local
    // directory for mail.
    std::fs::create_dir_all(root.join(".local")).unwrap();
    assert!(identity.needs_admin().await.unwrap());
    let password = "Setup-wizard-password-1";
    for (username, email, candidate) in [
        ("bad name", None, password),
        ("admin", Some("not-an-email"), password),
        ("admin", None, "too-short"),
    ] {
        assert!(
            identity
                .setup_admin(username, email, candidate)
                .await
                .is_err(),
            "the setup wizard checks the administrator against the registration rules: {username} {email:?} {candidate}"
        );
    }
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_member WHERE username = 'admin'"
        )
        .await,
        0
    );
    let (first, second) = tokio::join!(
        identity.setup_admin("admin", None, password),
        identity.setup_admin("admin", None, password)
    );
    assert_eq!(
        usize::from(first.is_ok()) + usize::from(second.is_ok()),
        1,
        "concurrent setup creates only one administrator"
    );
    assert!(matches!(
        first.err().or(second.err()),
        Some(identity::IdentityError::Conflict(_))
    ));
    assert!(!identity.needs_admin().await.unwrap());
    assert!(
        identity
            .setup_admin("another-admin", None, password)
            .await
            .is_err(),
        "the wizard cannot create an administrator once one exists"
    );
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_member WHERE adminid = 1"
        )
        .await,
        1
    );
    let admin = identity.login("admin", password).await.unwrap();
    assert!(admin.member.is_admin);
    assert!(!admin.member.email_verified);
    assert!(admin.member.email.is_none());
    let invite = identity
        .generate_invite(admin.member.uid, "admin-without-email")
        .await
        .unwrap();
    identity
        .revoke_invite(admin.member.uid, invite.id)
        .await
        .unwrap();
    execute(
        database,
        "INSERT INTO gonof_common_member (username) VALUES ('synthetic-author')",
    )
    .await;
    assert!(identity.login("synthetic-author", PASSWORD).await.is_err());
    admin
}

async fn assert_registration(identity: &Identity, database: &Database, admin: i64) {
    let invitation = identity
        .generate_invite(admin, "registration-race")
        .await
        .unwrap();
    let stored: String =
        gono_forum_web::db::query_scalar("SELECT code FROM gonof_common_invite WHERE id = ?")
            .bind(invitation.id)
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        stored, invitation.code,
        "invite codes are stored in plain text"
    );
    let alternate = if invitation.code.to_lowercase() != invitation.code {
        invitation.code.to_lowercase()
    } else {
        invitation.code.to_uppercase()
    };
    if alternate != invitation.code {
        assert!(
            identity
                .register("case-sensitive", "case@example.test", PASSWORD, &alternate)
                .await
                .is_err()
        );
    }
    let results = tokio::join!(
        identity.register(
            "racer-one",
            "racer-one@example.test",
            PASSWORD,
            &invitation.code
        ),
        identity.register(
            "racer-two",
            "racer-two@example.test",
            PASSWORD,
            &invitation.code
        ),
        identity.register(
            "racer-three",
            "racer-three@example.test",
            PASSWORD,
            &invitation.code
        ),
        identity.register(
            "racer-four",
            "racer-four@example.test",
            PASSWORD,
            &invitation.code
        ),
    );
    let winners: Vec<_> = [results.0, results.1, results.2, results.3]
        .into_iter()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "one code registers only one account across four racing connections"
    );
    let winner = &winners[0];
    assert_eq!(
        identity
            .generate_invite(admin, "registration-race")
            .await
            .unwrap()
            .id,
        invitation.id
    );
    assert!(
        identity
            .register(
                "fifth-racer",
                "fifth@example.test",
                PASSWORD,
                &invitation.code
            )
            .await
            .is_err()
    );
    assert!(
        identity
            .generate_invite(winner.member.uid, "unverified")
            .await
            .is_err()
    );
    assert!(identity.invites(winner.member.uid, 1).await.is_err());
    let session = identity
        .session(&winner.session_token)
        .await
        .unwrap()
        .unwrap();
    assert!(!session.member.email_verified);
    assert!(identity::check_csrf(&winner.csrf_token, &session));
    assert!(!identity::check_csrf("forged", &session));
    let duplicate = identity
        .generate_invite(admin, "duplicate-user")
        .await
        .unwrap();
    let before = registration_counts(database).await;
    assert!(
        identity
            .register(
                &winner.member.username,
                "different@example.test",
                PASSWORD,
                &duplicate.code
            )
            .await
            .is_err()
    );
    assert_eq!(
        registration_counts(database).await,
        before,
        "a username conflict rolls back the whole registration transaction"
    );
    assert!(
        identity
            .register(
                "duplicate-email",
                winner.member.email.as_deref().unwrap(),
                PASSWORD,
                &duplicate.code
            )
            .await
            .is_err()
    );
    assert_eq!(
        registration_counts(database).await,
        before,
        "an email conflict rolls back the whole registration transaction"
    );
    let duplicated = identity
        .generate_invite(admin, "duplicate-user")
        .await
        .unwrap();
    assert_eq!(duplicated.id, duplicate.id);
    assert_eq!(duplicated.status, "available");
    let canonical = identity
        .register(
            "canonical-email",
            "  Canonical@Example.Test  ",
            PASSWORD,
            &duplicate.code,
        )
        .await
        .unwrap();
    assert_eq!(
        canonical.member.email.as_deref(),
        Some("canonical@example.test")
    );
    assert!(
        identity
            .login("CANONICAL@EXAMPLE.TEST", PASSWORD)
            .await
            .is_ok()
    );
    assert!(
        identity
            .login("canonical-email", "wrong password")
            .await
            .is_err()
    );

    let revoked = identity
        .generate_invite(admin, "revoke-race")
        .await
        .unwrap();
    let (register, revoke) = tokio::join!(
        identity.register(
            "revoke-racer",
            "revoke-racer@example.test",
            PASSWORD,
            &revoked.code
        ),
        identity.revoke_invite(admin, revoked.id)
    );
    assert_ne!(
        register.is_ok(),
        revoke.is_ok(),
        "only one of revoking and redeeming may win the race"
    );
    let final_invite = identity
        .generate_invite(admin, "revoke-race")
        .await
        .unwrap();
    assert_eq!(
        final_invite.id, revoked.id,
        "a repeated request for a revoked invite must not create a new code"
    );
    assert_eq!(
        final_invite.status,
        if register.is_ok() { "used" } else { "revoked" }
    );
    let expired = identity
        .generate_invite(admin, "expired-invitation")
        .await
        .unwrap();
    gono_forum_web::db::query(
        "UPDATE gonof_common_invite SET dateline = ?, endtime = ? WHERE id = ?",
    )
    .bind(identity::now() - 100)
    .bind(identity::now() - 1)
    .bind(expired.id)
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        identity
            .register(
                "expired-invite-user",
                "expired-invite@example.test",
                PASSWORD,
                &expired.code
            )
            .await
            .is_err()
    );
    let retry_expired = identity
        .generate_invite(admin, "expired-invitation")
        .await
        .unwrap();
    assert_eq!(retry_expired.id, expired.id);
    assert!(!retry_expired.is_available());

    // A unique email race with separate invitations must leave the loser's invitation available.
    let left = identity
        .generate_invite(admin, "email-race-left")
        .await
        .unwrap();
    let right = identity
        .generate_invite(admin, "email-race-right")
        .await
        .unwrap();
    let before = registration_counts(database).await;
    let (left_result, right_result) = tokio::join!(
        identity.register(
            "email-race-left",
            "unique-race@example.test",
            PASSWORD,
            &left.code
        ),
        identity.register(
            "email-race-right",
            "unique-race@example.test",
            PASSWORD,
            &right.code
        )
    );
    assert_ne!(left_result.is_ok(), right_result.is_ok());
    let after = registration_counts(database).await;
    assert_eq!(
        after,
        before
            .into_iter()
            .map(|count| count + 1)
            .collect::<Vec<_>>()
    );
}

async fn assert_invitation_limits(
    identity: &Identity,
    database: &Database,
    root: &Path,
    admin: i64,
) {
    let member = register_member(identity, admin, "quota-member").await;
    verify_member(identity, database, member.member.uid).await;
    // Quotas are settings (the write gate reads the database): set them back to the seeded 5 only during the limit
    // checks below, and lift them again before the administrator sends invites.
    invite_limits(database, 5).await;
    let restricted = Identity::new(
        database.clone(),
        test_identity_config(),
        root.to_path_buf(),
        gono_forum_web::test_support::detached_global(),
    );
    let uid = member.member.uid;
    let (first, duplicate) = tokio::join!(
        restricted.generate_invite(uid, "same-request"),
        restricted.generate_invite(uid, "same-request")
    );
    let first = first.unwrap();
    assert_eq!(first.id, duplicate.unwrap().id);
    assert_eq!(restricted.invites(uid, 1).await.unwrap().total, 1);
    let results = tokio::join!(
        restricted.generate_invite(uid, "quota-1"),
        restricted.generate_invite(uid, "quota-2"),
        restricted.generate_invite(uid, "quota-3"),
        restricted.generate_invite(uid, "quota-4"),
        restricted.generate_invite(uid, "quota-5"),
        restricted.generate_invite(uid, "quota-6")
    );
    assert_eq!(
        [
            results.0, results.1, results.2, results.3, results.4, results.5
        ]
        .into_iter()
        .filter(Result::is_ok)
        .count(),
        4
    );
    assert_eq!(restricted.invites(uid, 1).await.unwrap().total, 5);
    assert!(
        restricted.revoke_invite(admin, first.id).await.is_err(),
        "an administrator cannot revoke another member's private invite either"
    );
    restricted.revoke_invite(uid, first.id).await.unwrap();
    assert!(
        restricted
            .generate_invite(uid, "revocation-no-refund")
            .await
            .is_err()
    );
    assert_eq!(
        restricted
            .generate_invite(uid, "same-request")
            .await
            .unwrap()
            .id,
        first.id
    );

    invite_limits(database, 100).await;
    let active = register_member(identity, admin, "active-quota").await;
    verify_member(identity, database, active.member.uid).await;
    invite_limits(database, 5).await;
    let active_uid = active.member.uid;
    for index in 0..5 {
        restricted
            .generate_invite(active_uid, &format!("active-{index}"))
            .await
            .unwrap();
    }
    gono_forum_web::db::query("UPDATE gonof_common_invite SET dateline = ? WHERE uid = ?")
        .bind(identity::now() - 172800)
        .bind(active_uid)
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        restricted
            .generate_invite(active_uid, "next-day-still-five")
            .await
            .is_err(),
        "the cap on live unused invites still applies on the next day"
    );
    let frozen_invite = restricted
        .invites(active_uid, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    gono_forum_web::db::query("UPDATE gonof_common_invite SET status = 'frozen' WHERE id = ?")
        .bind(frozen_invite.id)
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        restricted
            .generate_invite(active_uid, "frozen-still-counts")
            .await
            .is_err(),
        "a frozen unused invite still counts against the open invite quota"
    );
    restricted
        .revoke_invite(active_uid, frozen_invite.id)
        .await
        .unwrap();
    assert_eq!(
        restricted
            .generate_invite(active_uid, &frozen_invite.request_key)
            .await
            .unwrap()
            .status,
        "revoked"
    );
    assert!(
        restricted
            .generate_invite(active_uid, "after-frozen-revocation")
            .await
            .is_ok(),
        "the owner can revoke a frozen invite and free its quota"
    );
    let before_freeze = restricted
        .invites(uid, 1)
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|invite| invite.status == "available")
        .unwrap();
    execute(
        database,
        &format!("UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {uid}"),
    )
    .await;
    assert!(restricted.generate_invite(uid, "frozen").await.is_err());
    assert!(restricted.invites(uid, 1).await.is_err());
    assert!(restricted.revoke_invite(uid, first.id).await.is_err());
    assert!(
        restricted
            .session(&member.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(restricted.login("quota-member", PASSWORD).await.is_err());
    assert!(
        identity
            .register(
                "frozen-inviter",
                "frozen-inviter@example.test",
                PASSWORD,
                &before_freeze.code
            )
            .await
            .is_err(),
        "a frozen inviter cannot recruit new accounts"
    );
    invite_limits(database, 100).await;
}

/// Per-member daily and concurrently active invite code quotas (`settings.invite.daily_limit`, `active_limit`,
/// 1–100).
async fn invite_limits(database: &Database, limit: i64) {
    for key in [
        "settings.invite.daily_limit",
        "settings.invite.active_limit",
    ] {
        gono_forum_web::test_support::set_setting(database, key, &limit.to_string())
            .await
            .unwrap();
    }
}

async fn assert_verification(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "verification-member").await;
    let original = verification_token(database, member.member.uid).await;
    assert!(identity.verification_status(&original).await.unwrap());
    assert!(
        identity.verification_status(&original).await.unwrap(),
        "a read-only check must not consume the verification token"
    );
    identity
        .resend_verification(member.member.uid)
        .await
        .unwrap();
    let replacement = verification_token(database, member.member.uid).await;
    assert_ne!(replacement, original);
    assert!(identity.verify_email(&original).await.is_err());
    let (first, second) = tokio::join!(
        identity.verify_email(&replacement),
        identity.verify_email(&replacement)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "a verification token is consumed only once under concurrency"
    );
    assert!(!identity.verification_status(&replacement).await.unwrap());
    assert!(identity.verify_email(&replacement).await.is_err());
    assert!(
        identity
            .session(&member.session_token)
            .await
            .unwrap()
            .unwrap()
            .member
            .email_verified
    );
    assert!(
        identity
            .generate_invite(member.member.uid, "verified-can-invite")
            .await
            .is_ok()
    );

    let expiring = register_member(identity, admin, "expired-verification").await;
    let token = verification_token(database, expiring.member.uid).await;
    gono_forum_web::db::query(
        "UPDATE gonof_common_member_token SET dateline = ?, expires_at = ? WHERE uid = ?",
    )
    .bind(identity::now() - 100)
    .bind(identity::now() - 1)
    .bind(expiring.member.uid)
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(!identity.verification_status(&token).await.unwrap());
    assert!(identity.verify_email(&token).await.is_err());
    let rebound = register_member(identity, admin, "rebound-verification").await;
    let token = verification_token(database, rebound.member.uid).await;
    execute(
        database,
        &format!(
            "UPDATE gonof_common_member SET email = 'changed@example.test' WHERE uid = {}",
            rebound.member.uid
        ),
    )
    .await;
    assert!(
        identity.verify_email(&token).await.is_err(),
        "verification is bound to the email the token was issued for"
    );
}

async fn assert_sessions(identity: &Identity, database: &Database, admin: i64) {
    let grant = register_member(identity, admin, "session-member").await;
    for invalid in ["invalid-session".to_owned(), identity::random_token()] {
        assert!(identity.session(&invalid).await.unwrap().is_none());
    }
    let profile = std::sync::Arc::new(gono_forum_web::debug::trace::RequestProfile::new());
    let session = gono_forum_web::debug::trace::scope(
        profile.clone(),
        identity.session(&grant.session_token),
    )
    .await
    .unwrap()
    .unwrap();
    let queries: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|q| q.source == gono_forum_web::debug::trace::QuerySource::Application)
        .collect();
    assert_eq!(
        queries.len(),
        1,
        "authentication and the header unread count read the member once"
    );
    assert_eq!(queries[0].rows_returned, 1);
    let sql = &queries[0].sql;
    assert!(sql.contains("unread_notifications"));
    assert!(
        !sql.contains("COALESCE")
            && !sql.contains("FOR UPDATE")
            && !sql.contains("COUNT(")
            && !sql.contains("gonof_common_session")
    );
    assert_eq!(sql.split("WHERE").nth(1).unwrap().trim(), "uid = ?");
    if database.backend() == gono_forum_web::db::Backend::Sqlite {
        // SQLite reads the member table by INTEGER PRIMARY KEY (rowid).
        let plan = gono_forum_web::test_support::sqlite_plan(
            database,
            &sql.replace('?', &grant.member.uid.to_string()),
        )
        .await
        .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("USING INTEGER PRIMARY KEY")),
            "{plan:?}"
        );
    } else {
        let plan = gono_forum_web::db::query(&format!("EXPLAIN FORMAT=JSON {sql}"))
            .bind(grant.member.uid)
            .fetch_one(database.executor())
            .await
            .unwrap();
        let plan: serde_json::Value =
            serde_json::from_str(&plan.try_get::<String, _>(0).unwrap()).unwrap();
        assert_eq!(plan["query_block"]["table"]["key"], "PRIMARY");
    }
    assert_eq!(session.member.uid, grant.member.uid);
    assert_eq!(session.member.avatar_aid, None);
    assert_eq!(
        session.member.email.as_deref(),
        Some("session-member@example.test")
    );
    assert_eq!(session.csrf_token, grant.csrf_token);
    assert!(identity::check_csrf(&grant.csrf_token, &session));
    assert!(!identity::check_csrf(&identity::random_token(), &session));
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_session WHERE uid = {}",
                grant.member.uid
            )
        )
        .await,
        0,
        "login credentials are not written to the in-memory online table"
    );
    identity.touch_online(&session, "192.0.2.1").await;
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_session WHERE uid = {}",
                grant.member.uid
            )
        )
        .await,
        1
    );
    execute(
        database,
        &format!(
            "DELETE FROM gonof_common_session WHERE uid = {}",
            grant.member.uid
        ),
    )
    .await;
    assert!(
        identity
            .session(&grant.session_token)
            .await
            .unwrap()
            .is_some(),
        "losing the online record does not revoke the encrypted cookie"
    );

    let mut tampered = grant.session_token.clone().into_bytes();
    let at = tampered.len() - 10;
    tampered[at] = if tampered[at] == b'A' { b'B' } else { b'A' };
    assert!(
        identity
            .session(&String::from_utf8(tampered).unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let (logout, renewal) = tokio::join!(
        identity.logout_all(&grant.session_token),
        identity.session(&grant.session_token)
    );
    logout.unwrap();
    if let Some(renewed) = renewal.unwrap() {
        assert!(
            identity
                .session(&renewed.session_token)
                .await
                .unwrap()
                .is_none(),
            "an in-flight renewal must not bump the credential version"
        );
    }
    assert!(
        identity
            .session(&grant.session_token)
            .await
            .unwrap()
            .is_none()
    );
    let fresh = identity.login("session-member", PASSWORD).await.unwrap();
    assert!(identity.logout_all(&grant.session_token).await.is_err());
    assert!(
        identity
            .session(&fresh.session_token)
            .await
            .unwrap()
            .is_some(),
        "a stale logout request cannot revoke the cookie of a later login"
    );
    execute(
        database,
        &format!(
            "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {}",
            grant.member.uid
        ),
    )
    .await;
    assert!(
        identity
            .session(&fresh.session_token)
            .await
            .unwrap()
            .is_none()
    );
    execute(database, &format!("UPDATE gonof_common_member SET `freeze` = 0, credentials_version = credentials_version + 1 WHERE uid = {}", grant.member.uid)).await;
    assert!(
        identity
            .session(&fresh.session_token)
            .await
            .unwrap()
            .is_none()
    );
}

async fn assert_delivery(identity: &Identity, database: &Database, root: &Path, admin: i64) {
    let mailbox = root.join(".local/mail");
    std::fs::write(&mailbox, b"simulate failed storage").unwrap();
    let failure =
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
            .await
            .unwrap();
    assert!(failure.failed > 0);
    assert_eq!(failure.delivered, 0);
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE status = 'processing'"
        )
        .await,
        0
    );
    let deferred =
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
            .await
            .unwrap();
    assert_eq!(
        deferred.claimed, 0,
        "a failure is not retried in the same round"
    );
    std::fs::remove_file(&mailbox).unwrap();
    // Once delivery is final the raw token is cleared from the database; record the original payloads first and put
    // them back later when simulating "sent, crashed before confirming" — a real crash happens before the
    // confirmation, and clearing and confirming belong to one transaction, so the payload would still be there.
    let payloads: std::collections::HashMap<i64, String> =
        gono_forum_web::db::query("SELECT eventid, payload FROM gonof_common_outbox")
            .fetch_all(&database.pool)
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.try_get("eventid").unwrap(),
                    db::text(row, "payload").unwrap(),
                )
            })
            .collect();
    let future = identity::now() + 3600;
    let (left, right) = tokio::join!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, future),
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, future)
    );
    let delivered = left.unwrap().delivered + right.unwrap().delivered;
    assert_eq!(
        delivered as i64,
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE status = 'sent'"
        )
        .await
    );
    assert!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE status = 'failed'"
        )
        .await
            > 0,
        "used, expired or revoked verification tokens are not delivered"
    );
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE status IN ('sent', 'failed') AND payload LIKE '%\"token\"%'"
        )
        .await,
        0,
        "no raw token stays in the database once mail is sent or given up"
    );
    assert!(
        std::fs::read_dir(mailbox.join("prepared"))
            .unwrap()
            .next()
            .is_none(),
        "a finished event keeps no outgoing draft with the token"
    );
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, future)
            .await
            .unwrap()
            .claimed,
        0
    );
    let entries: Vec<_> = std::fs::read_dir(&mailbox)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_file())
        .collect();
    assert_eq!(entries.len(), delivered);
    assert!(entries.iter().all(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "eml")
    }));

    // Publication succeeded, then the process died before committing its acknowledgement.
    let row = gono_forum_web::db::query(
        "SELECT eventid FROM gonof_common_outbox WHERE status = 'sent' ORDER BY eventid LIMIT 1",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    let eventid: i64 = row.try_get("eventid").unwrap();
    let path = mailbox.join(format!("{eventid}.eml"));
    let original = std::fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("/verify-email?token="));
    gono_forum_web::db::query("UPDATE gonof_common_outbox SET status = 'processing', payload = ?, lease_token = 'crashed-worker', lease_until = ? WHERE eventid = ?")
        .bind(&payloads[&eventid])
        .bind(future)
        .bind(eventid)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, future - 1)
            .await
            .unwrap()
            .claimed,
        0
    );
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, future + 1)
            .await
            .unwrap()
            .delivered,
        1
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "recovery must not regenerate or overwrite delivered mail"
    );
    assert_eq!(
        std::fs::read_dir(&mailbox)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().unwrap().is_file())
            .count(),
        delivered
    );

    // A fresh registration is committed before the outbox worker can observe it.
    let member = register_member(identity, admin, "delivery-member").await;
    assert!(
        identity
            .session(&member.session_token)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
            .await
            .unwrap()
            .delivered,
        1
    );
    let superseded = register_member(identity, admin, "superseded-mail").await;
    let old_event = scalar(database, "SELECT MAX(eventid) FROM gonof_common_outbox").await;
    identity
        .resend_verification(superseded.member.uid)
        .await
        .unwrap();
    let resend =
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
            .await
            .unwrap();
    assert_eq!(resend.skipped, 1);
    assert_eq!(resend.delivered, 1);
    assert!(
        !mailbox.join(format!("{old_event}.eml")).exists(),
        "a resend must not deliver mail with the old token"
    );
    let freezing = register_member(identity, admin, "mail-freeze-race").await;
    let freeze_event = scalar(database, "SELECT MAX(eventid) FROM gonof_common_outbox").await;
    let mut freeze_transaction = database.begin_write().await.unwrap();
    gono_forum_web::db::query("UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = ?")
        .bind(freezing.member.uid)
        .execute(database.observe(&mut *freeze_transaction))
        .await
        .unwrap();
    let (delivery, freeze) = tokio::join!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now()),
        async {
            tokio::task::yield_now().await;
            freeze_transaction.commit().await
        }
    );
    freeze.unwrap();
    let delivery = delivery.unwrap();
    assert_eq!(
        delivery.skipped, 1,
        "mail delivery reads the committed frozen state after waiting for the account lock"
    );
    assert_eq!(delivery.delivered, 0);
    assert!(!mailbox.join(format!("{freeze_event}.eml")).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&mailbox).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn mysql_invite_pages_bound_rows_and_keep_whole_account_quotas() {
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};

    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let database = &services.db;
    let identity = Identity::new(
        database.clone(),
        test_identity_config(),
        services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    execute(database, "INSERT INTO gonof_common_member (uid, username, password, adminid) VALUES (901, 'invite-page-owner', 'test-password-hash', 1), (902, 'invite-page-other', 'test-password-hash', 1)").await;
    let empty = identity.invites(901, 1).await.unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(
        (empty.total, empty.today_generated, empty.active),
        (0, 0, 0)
    );
    assert!(!empty.has_next);
    assert!(matches!(
        identity.invites(901, 2).await,
        Err(identity::IdentityError::NotFound)
    ));

    let timestamp = identity::now();
    let mut ids = Vec::new();
    for index in 0..45 {
        let dateline = if index < 5 {
            timestamp + 86_400
        } else {
            timestamp - 172_800
        };
        let endtime = if index < 35 {
            timestamp + 172_800
        } else {
            timestamp - 86_400
        };
        let status = ["available", "frozen", "revoked"][index % 3];
        let id = gono_forum_web::db::query("INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime, status) VALUES (901, ?, ?, ?, ?, ?)")
            .bind(format!("page-code-{index}")).bind(format!("page-request-{index}"))
            .bind(dateline).bind(endtime).bind(status).execute(database.executor()).await.unwrap().last_insert_id() as i64;
        ids.push(id);
    }
    gono_forum_web::db::query("INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime) VALUES (902, 'other-page-code', 'other-page-request', ?, ?)")
        .bind(timestamp).bind(timestamp + 172_800).execute(database.executor()).await.unwrap();
    ids.reverse();
    let mut actual = Vec::new();
    for page in 1..=3 {
        let profile = std::sync::Arc::new(RequestProfile::new());
        let result = trace::scope(profile.clone(), identity.invites(901, page))
            .await
            .unwrap();
        assert_eq!(
            (result.total, result.today_generated, result.active),
            (45, 5, 24)
        );
        assert_eq!(result.rows.len(), if page == 3 { 5 } else { 20 });
        assert_eq!(result.has_next, page < 3);
        assert!(result.rows.iter().all(|invite| invite.uid == 901));
        actual.extend(result.rows.into_iter().map(|invite| invite.id));
        let reads: Vec<_> = profile
            .snapshot()
            .into_iter()
            .filter(|query| {
                query.source == QuerySource::Application
                    && query.sql.contains("FROM gonof_common_invite")
                    && !query.sql.contains("COUNT(*)")
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert!(reads[0].sql.contains("LIMIT ? OFFSET ?"));
        assert!(
            reads[0].rows_returned <= 20,
            "the invite rows the database returns are bounded"
        );
    }
    assert_eq!(
        actual, ids,
        "paging keeps descending order with no overlap and no gaps"
    );
    for page in [0, 4, usize::MAX] {
        assert!(matches!(
            identity.invites(901, page).await,
            Err(identity::IdentityError::NotFound)
        ));
    }
    execute(
        database,
        "UPDATE gonof_common_member SET freeze = 1 WHERE uid = 901",
    )
    .await;
    assert!(matches!(
        identity.invites(901, 1).await,
        Err(identity::IdentityError::Forbidden)
    ));
    services.cleanup().await.unwrap();
}

async fn token_previews(identity: &Identity, tokens: &[String; 3]) -> [bool; 3] {
    [
        identity.verification_status(&tokens[0]).await.unwrap(),
        identity.password_reset_status(&tokens[1]).await.unwrap(),
        identity
            .email_change_status(&tokens[2])
            .await
            .unwrap()
            .is_some(),
    ]
}

/// The pre-check on the e-mail link page must agree with the re-check under lock at submission: an account in
/// deletion still has its e-mail and password, so the pre-check must not show it the form just because it only
/// looks at the freeze bit.
#[tokio::test]
async fn mysql_token_previews_follow_account_state() {
    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let database = &services.db;
    let identity = Identity::new(
        database.clone(),
        test_identity_config(),
        services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    execute(database, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus) VALUES (911, 'token-preview', 'test-password-hash', 'preview@example.test', 0)").await;
    let timestamp = identity::now();
    let mut tokens: [String; 3] = Default::default();
    for (slot, (purpose, email)) in [
        ("email_verification", "preview@example.test"),
        ("password_reset", "preview@example.test"),
        ("email_change", "preview-new@example.test"),
    ]
    .into_iter()
    .enumerate()
    {
        let raw = identity::random_token();
        gono_forum_web::db::query("INSERT INTO gonof_common_member_token (uid, purpose, token_hash, email, dateline, expires_at) VALUES (911, ?, ?, ?, ?, ?)")
            .bind(purpose).bind(identity::token_hash(&raw)).bind(email)
            .bind(timestamp - 10).bind(timestamp + 3600)
            .execute(&database.pool).await.unwrap();
        tokens[slot] = raw;
    }
    assert_eq!(token_previews(&identity, &tokens).await, [true; 3]);

    execute(
        database,
        "UPDATE gonof_common_member SET account_state = 'deleting' WHERE uid = 911",
    )
    .await;
    assert_eq!(
        token_previews(&identity, &tokens).await,
        [false; 3],
        "an account being deleted shows no verification, reset or email change form"
    );
    assert!(identity.verify_email(&tokens[0]).await.is_err());
    assert!(
        identity
            .reset_password(&tokens[1], "Preview-password-5921")
            .await
            .is_err()
    );
    assert!(identity.confirm_email_change(&tokens[2]).await.is_err());

    execute(
        database,
        "UPDATE gonof_common_member SET account_state = 'active', `freeze` = 1 WHERE uid = 911",
    )
    .await;
    assert_eq!(token_previews(&identity, &tokens).await, [false; 3]);

    execute(
        database,
        "UPDATE gonof_common_member SET `freeze` = 0, email = 'preview-new@example.test' WHERE uid = 911",
    )
    .await;
    assert_eq!(
        token_previews(&identity, &tokens).await,
        [false; 3],
        "email changed: verification and reset are bound to the old email, and the change target is already the current email"
    );
    services.cleanup().await.unwrap();
}

#[tokio::test]
async fn mysql_identity_contract() {
    let services = gono_forum_web::test_support::TestFixture::new()
        .await
        .unwrap();
    identity_contract(services.db.clone(), &services.root).await;
    services.cleanup().await.unwrap();
}
