//! The account security contract runs in a separate MySQL test database.
use std::path::Path;

use gono_forum_web::{
    db::{self, Database},
    identity::{self, Identity, SessionMetadata},
};

use super::{ORIGIN, PASSWORD, register_member, scalar, verification_token};

const NEW_PASSWORD: &str = "Changed-password-3957";
const OTHER_PASSWORD: &str = "Another-password-6428";

async fn execute(database: &Database, statement: &str) {
    // MySQL CREATE TRIGGER requires the text protocol rather than a prepared query.
    gono_forum_web::db::raw_sql(statement)
        .execute(database.executor())
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

pub async fn assert_account_security(
    identity: &Identity,
    database: &Database,
    root: &Path,
    admin: i64,
) {
    assert_password_changes(identity, database, admin).await;
    assert_reset_requests(identity, database, admin).await;
    assert_password_resets(identity, database, admin).await;
    assert_devices(identity, database, admin).await;
    assert_atomic_failures(identity, database, admin).await;
    assert_reset_delivery(identity, database, root, admin).await;
}

async fn payload_of(database: &Database, eventid: i64) -> String {
    gono_forum_web::db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE eventid = ?")
        .bind(eventid)
        .fetch_one(database.executor())
        .await
        .unwrap()
}

async fn reset_mail(database: &Database, uid: i64) -> (i64, String) {
    let row = gono_forum_web::db::query("SELECT eventid, payload FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type = 'password_reset' ORDER BY eventid DESC LIMIT 1")
    .bind(uid)
    .fetch_one(database.executor())
    .await
    .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&db::text(&row, "payload").unwrap()).unwrap();
    (
        row.try_get("eventid").unwrap(),
        payload["token"].as_str().unwrap().to_owned(),
    )
}

async fn request_reset(identity: &Identity, database: &Database, uid: i64, email: &str) -> String {
    identity.request_password_reset(email).await.unwrap();
    reset_mail(database, uid).await.1
}

async fn password_state(database: &Database, uid: i64) -> (String, Vec<i64>) {
    let row = gono_forum_web::db::query("SELECT password FROM gonof_common_member WHERE uid = ?")
        .bind(uid)
        .fetch_one(database.executor())
        .await
        .unwrap();
    let mut counts = Vec::new();
    for query in [
        format!("SELECT credentials_version FROM gonof_common_member WHERE uid = {uid}"),
        format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {uid}"),
        format!(
            "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {uid} AND consumed_at IS NOT NULL"
        ),
        format!(
            "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {uid} AND revoked_at IS NOT NULL"
        ),
        format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE aggregate_id = {uid}"),
        format!("SELECT COUNT(*) FROM gonof_common_log WHERE uid = {uid}"),
    ] {
        counts.push(scalar(database, &query).await);
    }
    (db::text(&row, "password").unwrap(), counts)
}

async fn assert_password_changes(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "security-change").await;
    let other = identity.login("security-change", PASSWORD).await.unwrap();
    let token = request_reset(
        identity,
        database,
        member.member.uid,
        "security-change@example.test",
    )
    .await;
    let before = password_state(database, member.member.uid).await;
    assert!(
        identity
            .change_password(&member.session_token, "Wrong-password-1234", NEW_PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .change_password(&member.session_token, PASSWORD, "too-short")
            .await
            .is_err()
    );
    assert_eq!(password_state(database, member.member.uid).await, before);

    let (left, right) = tokio::join!(
        identity.change_password(&member.session_token, PASSWORD, NEW_PASSWORD),
        identity.change_password(&other.session_token, PASSWORD, OTHER_PASSWORD),
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "concurrent password changes commit only once"
    );
    let winning_password = if left.is_ok() {
        NEW_PASSWORD
    } else {
        OTHER_PASSWORD
    };
    assert!(identity.login("security-change", PASSWORD).await.is_err());
    assert!(
        identity
            .login("security-change", winning_password)
            .await
            .is_ok()
    );
    for sid in [&member.session_token, &other.session_token] {
        assert!(identity.session(sid).await.unwrap().is_none());
        assert!(
            identity
                .change_password(sid, winning_password, PASSWORD)
                .await
                .is_err()
        );
    }
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT credentials_version FROM gonof_common_member WHERE uid = {}",
                member.member.uid
            )
        )
        .await,
        member.member.credentials_version + 1,
    );
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_log WHERE uid = {} AND type = 'identity.password.change'", member.member.uid)).await,
        1,
    );
    assert!(
        !identity::valid_actor(database, member.member.actor())
            .await
            .unwrap()
    );
    assert!(!identity.password_reset_status(&token).await.unwrap());
    assert!(identity.reset_password(&token, PASSWORD).await.is_err());

    let racing = register_member(identity, admin, "security-change-reset-race").await;
    let token = request_reset(
        identity,
        database,
        racing.member.uid,
        "security-change-reset-race@example.test",
    )
    .await;
    let (change, reset) = tokio::join!(
        identity.change_password(&racing.session_token, PASSWORD, NEW_PASSWORD),
        identity.reset_password(&token, OTHER_PASSWORD),
    );
    assert_ne!(
        change.is_ok(),
        reset.is_ok(),
        "when a password change races a reset, only one operation on the old credentials commits"
    );
    let password = if change.is_ok() {
        NEW_PASSWORD
    } else {
        OTHER_PASSWORD
    };
    assert!(
        identity
            .login("security-change-reset-race", password)
            .await
            .is_ok()
    );
    assert!(
        identity
            .login("security-change-reset-race", PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .session(&racing.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!identity.password_reset_status(&token).await.unwrap());

    let racing_login = register_member(identity, admin, "security-change-login-race").await;
    let (changed, login) = tokio::join!(
        identity.change_password(&racing_login.session_token, PASSWORD, NEW_PASSWORD),
        identity.login("security-change-login-race", PASSWORD),
    );
    changed.unwrap();
    if let Ok(grant) = login {
        assert!(
            identity
                .session(&grant.session_token)
                .await
                .unwrap()
                .is_none(),
            "a concurrent login finished before the password change is signed out too"
        );
    }
    assert!(
        identity
            .login("security-change-login-race", PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .login("security-change-login-race", NEW_PASSWORD)
            .await
            .is_ok()
    );
}

async fn assert_reset_requests(identity: &Identity, database: &Database, admin: i64) {
    let initial = scalar(
        database,
        "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type = 'password_reset'",
    )
    .await;
    identity
        .request_password_reset("missing-security-account@example.test")
        .await
        .unwrap();
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type = 'password_reset'"
        )
        .await,
        initial
    );

    let member = register_member(identity, admin, "security-request").await;
    let (left, right) = tokio::join!(
        identity.request_password_reset(" SECURITY-REQUEST@EXAMPLE.TEST "),
        identity.request_password_reset("security-request@example.test"),
    );
    left.unwrap();
    right.unwrap();
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {} AND purpose = 'password_reset'", member.member.uid)).await,
        2,
        "the core issues every repeated request; the HTTP layer does the rate limiting",
    );
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE aggregate_id = {} AND event_type = 'password_reset'", member.member.uid)).await,
        2,
    );
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {} AND purpose = 'password_reset' AND consumed_at IS NULL AND revoked_at IS NULL", member.member.uid)).await,
        1,
        "concurrent resends are serialized and only the last token issued stays valid",
    );
    let (_, token) = reset_mail(database, member.member.uid).await;
    assert_eq!(token.len(), 64);
    let row = gono_forum_web::db::query("SELECT token_hash, expires_at - dateline AS lifetime FROM gonof_common_member_token WHERE uid = ? AND purpose = 'password_reset' AND revoked_at IS NULL")
        .bind(member.member.uid).fetch_one(database.executor()).await.unwrap();
    assert_eq!(
        db::text(&row, "token_hash").unwrap(),
        identity::token_hash(&token)
    );
    assert_eq!(row.try_get::<i64, _>("lifetime").unwrap(), 3600);
    assert!(identity.password_reset_status(&token).await.unwrap());
    let replacement = request_reset(
        identity,
        database,
        member.member.uid,
        "security-request@example.test",
    )
    .await;
    assert_ne!(replacement, token);
    assert!(!identity.password_reset_status(&token).await.unwrap());
    assert!(identity.reset_password(&token, NEW_PASSWORD).await.is_err());
    assert!(identity.password_reset_status(&replacement).await.unwrap());

    let frozen = register_member(identity, admin, "security-request-frozen").await;
    execute(
        database,
        &format!(
            "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {}",
            frozen.member.uid
        ),
    )
    .await;
    identity
        .request_password_reset("security-request-frozen@example.test")
        .await
        .unwrap();
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {} AND purpose = 'password_reset'", frozen.member.uid)).await,
        0,
    );
}

async fn assert_password_resets(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "security-reset").await;
    let second = identity.login("security-reset", PASSWORD).await.unwrap();
    let verification = verification_token(database, member.member.uid).await;
    let token = request_reset(
        identity,
        database,
        member.member.uid,
        "security-reset@example.test",
    )
    .await;
    assert!(!identity.password_reset_status(&verification).await.unwrap());
    assert!(
        identity
            .reset_password(&verification, NEW_PASSWORD)
            .await
            .is_err()
    );
    assert!(identity.verify_email(&token).await.is_err());
    assert!(!identity.password_reset_status("invalid").await.unwrap());
    assert!(
        identity
            .reset_password("invalid", NEW_PASSWORD)
            .await
            .is_err()
    );
    assert!(identity.password_reset_status(&token).await.unwrap());
    assert!(
        identity.password_reset_status(&token).await.unwrap(),
        "the GET check does not consume the token"
    );
    assert!(identity.reset_password(&token, "short").await.is_err());
    assert!(identity.password_reset_status(&token).await.unwrap());

    let (left, right) = tokio::join!(
        identity.reset_password(&token, NEW_PASSWORD),
        identity.reset_password(&token, OTHER_PASSWORD),
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "a reset token is consumed only once under concurrency"
    );
    let winning_password = if left.is_ok() {
        NEW_PASSWORD
    } else {
        OTHER_PASSWORD
    };
    assert!(!identity.password_reset_status(&token).await.unwrap());
    assert!(identity.reset_password(&token, PASSWORD).await.is_err());
    assert!(identity.login("security-reset", PASSWORD).await.is_err());
    assert!(
        identity
            .login("security-reset", winning_password)
            .await
            .is_ok()
    );
    assert!(
        identity
            .session(&member.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        identity
            .session(&second.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT emailstatus FROM gonof_common_member WHERE uid = {}",
                member.member.uid
            )
        )
        .await,
        1,
        "an unverified email can reset the password, and using the mailed link verifies it",
    );
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT credentials_version FROM gonof_common_member WHERE uid = {}",
                member.member.uid
            )
        )
        .await,
        member.member.credentials_version + 1,
    );
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {} AND purpose = 'password_reset' AND consumed_at IS NOT NULL", member.member.uid)).await,
        1,
    );
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_log WHERE uid = {} AND type = 'identity.password.reset'", member.member.uid)).await,
        1,
    );

    for (name, mutation) in [
        (
            "security-reset-expired",
            "expires_at = NOW_MINUS_ONE, dateline = NOW_MINUS_HOUR",
        ),
        (
            "security-reset-rebound",
            "email = 'replacement-security@example.test'",
        ),
        ("security-reset-frozen", "`freeze` = 1"),
    ] {
        let member = register_member(identity, admin, name).await;
        let token = request_reset(
            identity,
            database,
            member.member.uid,
            &format!("{name}@example.test"),
        )
        .await;
        if name.ends_with("expired") {
            let mutation = mutation
                .replace("NOW_MINUS_ONE", &(identity::now() - 1).to_string())
                .replace("NOW_MINUS_HOUR", &(identity::now() - 3601).to_string());
            execute(database, &format!("UPDATE gonof_common_member_token SET {mutation} WHERE uid = {} AND purpose = 'password_reset'", member.member.uid)).await;
        } else {
            execute(
                database,
                &format!(
                    "UPDATE gonof_common_member SET {mutation} WHERE uid = {}",
                    member.member.uid
                ),
            )
            .await;
        }
        assert!(!identity.password_reset_status(&token).await.unwrap());
        assert!(identity.reset_password(&token, NEW_PASSWORD).await.is_err());
    }

    let racing_login = register_member(identity, admin, "security-reset-login-race").await;
    let token = request_reset(
        identity,
        database,
        racing_login.member.uid,
        "security-reset-login-race@example.test",
    )
    .await;
    let (reset, login) = tokio::join!(
        identity.reset_password(&token, NEW_PASSWORD),
        identity.login("security-reset-login-race", PASSWORD),
    );
    reset.unwrap();
    if let Ok(grant) = login {
        assert!(
            identity
                .session(&grant.session_token)
                .await
                .unwrap()
                .is_none(),
            "a concurrent login finished before the reset is signed out too"
        );
    }
    assert!(
        identity
            .login("security-reset-login-race", PASSWORD)
            .await
            .is_err()
    );
    assert!(
        identity
            .login("security-reset-login-race", NEW_PASSWORD)
            .await
            .is_ok()
    );

    let freezing = register_member(identity, admin, "security-reset-freeze-race").await;
    let token = request_reset(
        identity,
        database,
        freezing.member.uid,
        "security-reset-freeze-race@example.test",
    )
    .await;
    let before = password_state(database, freezing.member.uid).await;
    let mut transaction = database.begin_write().await.unwrap();
    gono_forum_web::db::query("SELECT uid FROM gonof_common_member WHERE uid = ? FOR UPDATE")
        .bind(freezing.member.uid)
        .fetch_one(database.observe(&mut *transaction))
        .await
        .unwrap();
    let reset = identity.reset_password(&token, NEW_PASSWORD);
    tokio::pin!(reset);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut reset)
            .await
            .is_err(),
        "the reset cannot finish while the account lock is held",
    );
    gono_forum_web::db::query("UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = ?")
        .bind(freezing.member.uid)
        .execute(database.observe(&mut *transaction))
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(
        reset.await.is_err(),
        "after waiting for the account lock the reset reads the frozen state just committed"
    );
    assert_eq!(password_state(database, freezing.member.uid).await, before);
}

async fn assert_devices(identity: &Identity, database: &Database, admin: i64) {
    let first = register_member(identity, admin, "security-devices").await;
    let second = identity
        .login_with_metadata(
            "security-devices",
            PASSWORD,
            &SessionMetadata::new("Mobile browser/2.0", "192.0.2.8"),
        )
        .await
        .unwrap();
    let outsider = register_member(identity, admin, "security-device-outsider").await;
    let (logout, renewed) = tokio::join!(
        identity.logout_all(&first.session_token),
        identity.session(&second.session_token)
    );
    logout.unwrap();
    for cookie in [&first.session_token, &second.session_token] {
        assert!(identity.session(cookie).await.unwrap().is_none());
    }
    if let Some(renewed) = renewed.unwrap() {
        assert!(
            identity
                .session(&renewed.session_token)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        identity
            .session(&outsider.session_token)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT credentials_version FROM gonof_common_member WHERE uid = {}",
                first.member.uid
            )
        )
        .await,
        first.member.credentials_version + 1
    );
    let left = identity.login("security-devices", PASSWORD).await.unwrap();
    let right = identity.login("security-devices", PASSWORD).await.unwrap();
    let (left_result, right_result) = tokio::join!(
        identity.logout_all(&left.session_token),
        identity.logout_all(&right.session_token)
    );
    assert_ne!(
        left_result.is_ok(),
        right_result.is_ok(),
        "concurrent sign-outs of all devices bump the old version only once"
    );
    assert!(
        identity
            .session(&left.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        identity
            .session(&right.session_token)
            .await
            .unwrap()
            .is_none()
    );
    let current = identity.login("security-devices", PASSWORD).await.unwrap();
    assert!(identity.logout_all(&left.session_token).await.is_err());
    assert!(
        identity
            .session(&current.session_token)
            .await
            .unwrap()
            .is_some()
    );
}

async fn install_failure(database: &Database, table: &str, condition: &str) {
    gono_forum_web::test_support::fail_when(
        database,
        "reject_account_security_write",
        "BEFORE INSERT",
        table,
        condition,
        "account security rollback test",
    )
    .await
    .unwrap()
}

async fn remove_failure(database: &Database, _table: &str) {
    {
        gono_forum_web::test_support::drop_trigger(database, "reject_account_security_write")
            .await
            .unwrap();
    }
}

async fn assert_atomic_failures(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "security-atomic").await;
    let other = identity.login("security-atomic", PASSWORD).await.unwrap();
    let before = password_state(database, member.member.uid).await;
    install_failure(
        database,
        "gonof_common_outbox",
        &format!(
            "NEW.aggregate_id = {} AND NEW.event_type = 'password_reset'",
            member.member.uid
        ),
    )
    .await;
    assert!(
        identity
            .request_password_reset("security-atomic@example.test")
            .await
            .is_err()
    );
    remove_failure(database, "gonof_common_outbox").await;
    assert_eq!(
        password_state(database, member.member.uid).await,
        before,
        "an Outbox failure rolls back the new token and the audit row"
    );
    let token = request_reset(
        identity,
        database,
        member.member.uid,
        "security-atomic@example.test",
    )
    .await;
    let before = password_state(database, member.member.uid).await;
    install_failure(
        database,
        "gonof_common_outbox",
        &format!(
            "NEW.aggregate_id = {} AND NEW.event_type = 'password_reset'",
            member.member.uid
        ),
    )
    .await;
    assert!(
        identity
            .request_password_reset("security-atomic@example.test")
            .await
            .is_err()
    );
    remove_failure(database, "gonof_common_outbox").await;
    assert_eq!(
        password_state(database, member.member.uid).await,
        before,
        "a failed resend keeps the reset token that was valid before"
    );
    assert!(identity.password_reset_status(&token).await.unwrap());

    for operation in ["change", "reset", "devices"] {
        let before = password_state(database, member.member.uid).await;
        install_failure(
            database,
            "gonof_common_log",
            &format!("NEW.uid = {}", member.member.uid),
        )
        .await;
        let failed = match operation {
            "change" => identity
                .change_password(&member.session_token, PASSWORD, NEW_PASSWORD)
                .await
                .is_err(),
            "reset" => identity.reset_password(&token, NEW_PASSWORD).await.is_err(),
            "devices" => identity.logout_all(&member.session_token).await.is_err(),
            _ => unreachable!(),
        };
        remove_failure(database, "gonof_common_log").await;
        assert!(
            failed,
            "{operation}: an audit failure reports the operation as not done"
        );
        assert_eq!(
            password_state(database, member.member.uid).await,
            before,
            "{operation}: a failure rolls back password, version, tokens and sessions atomically"
        );
        assert!(identity.password_reset_status(&token).await.unwrap());
        assert!(
            identity
                .session(&member.session_token)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            identity
                .session(&other.session_token)
                .await
                .unwrap()
                .is_some()
        );
    }
    identity.reset_password(&token, NEW_PASSWORD).await.unwrap();
    assert!(
        identity
            .session(&member.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        identity
            .session(&other.session_token)
            .await
            .unwrap()
            .is_none()
    );
}

async fn assert_reset_delivery(identity: &Identity, database: &Database, root: &Path, admin: i64) {
    let mailbox = root.join(".local/security-mail");
    while gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
        .await
        .unwrap()
        .claimed
        > 0
    {}
    assert_eq!(
        scalar(database, "SELECT COUNT(*) FROM gonof_common_outbox o JOIN gonof_common_member m ON m.uid = o.aggregate_id WHERE m.username = 'security-request' AND o.event_type = 'password_reset' AND o.status = 'sent'").await,
        1,
        "repeated requests deliver only the reset mail that is still valid",
    );
    assert_eq!(
        scalar(database, "SELECT COUNT(*) FROM gonof_common_outbox o JOIN gonof_common_member m ON m.uid = o.aggregate_id WHERE m.username = 'security-request' AND o.event_type = 'password_reset' AND o.status = 'failed'").await,
        2,
    );
    let member = register_member(identity, admin, "security-mail").await;
    let token = request_reset(
        identity,
        database,
        member.member.uid,
        "security-mail@example.test",
    )
    .await;
    let (eventid, _) = reset_mail(database, member.member.uid).await;
    let original_payload = payload_of(database, eventid).await;
    let blocked = root.join(".local/security-mail-blocked");
    std::fs::write(&blocked, b"simulate mail storage failure").unwrap();
    let failure =
        gono_forum_web::test_support::deliver_mail(database, &blocked, ORIGIN, identity::now())
            .await
            .unwrap();
    assert!(failure.failed >= 1);
    assert_eq!(failure.delivered, 0);
    assert_eq!(
        scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE eventid = {eventid} AND status = 'pending' AND attempts = 1 AND available_at > dateline")).await,
        1,
    );
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &blocked, ORIGIN, identity::now())
            .await
            .unwrap()
            .claimed,
        0
    );
    std::fs::remove_file(blocked).unwrap();
    execute(
        database,
        &format!(
            "UPDATE gonof_common_outbox SET available_at = dateline WHERE eventid = {eventid}"
        ),
    )
    .await;
    let (left, right) = tokio::join!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now()),
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now()),
    );
    assert_eq!(left.unwrap().delivered + right.unwrap().delivered, 1);
    let path = mailbox.join(format!("{eventid}.eml"));
    let contents = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&contents);
    assert!(text.contains(&format!("{ORIGIN}/reset-password?token={token}")));
    assert!(
        text.contains("Subject: [gonof] Reset your password\r\n"),
        "{text}"
    );
    assert!(!text.contains(PASSWORD));
    assert!(
        identity.password_reset_status(&token).await.unwrap(),
        "sending the mail must not consume the reset token"
    );
    assert_eq!(
        payload_of(database, eventid).await,
        "{}",
        "no raw reset token stays in the database once the mail is sent"
    );
    assert!(!mailbox.join(format!("prepared/{eventid}.eml")).exists());
    let count = std::fs::read_dir(&mailbox).unwrap().count();
    // Simulates a crash after sending but before confirmation: confirmation and clearing the token are in the same
    // transaction, so the payload is still there after the crash.
    gono_forum_web::db::query("UPDATE gonof_common_outbox SET status = 'processing', payload = ?, lease_token = 'crashed-reset-worker', lease_until = 0 WHERE eventid = ?")
        .bind(&original_payload)
        .bind(eventid)
        .execute(database.executor())
        .await
        .unwrap();
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
            .await
            .unwrap()
            .delivered,
        1
    );
    assert_eq!(std::fs::read(&path).unwrap(), contents);
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), count);

    for name in [
        "security-mail-consumed",
        "security-mail-revoked",
        "security-mail-expired",
        "security-mail-frozen",
        "security-mail-rebound",
    ] {
        let member = register_member(identity, admin, name).await;
        let token = request_reset(
            identity,
            database,
            member.member.uid,
            &format!("{name}@example.test"),
        )
        .await;
        let (eventid, _) = reset_mail(database, member.member.uid).await;
        if name.ends_with("consumed") {
            identity.reset_password(&token, NEW_PASSWORD).await.unwrap();
        } else if name.ends_with("revoked") {
            let blocked = root.join(".local/security-stale-retry");
            std::fs::write(&blocked, b"simulate failure before password change").unwrap();
            assert!(
                gono_forum_web::test_support::deliver_mail(
                    database,
                    &blocked,
                    ORIGIN,
                    identity::now()
                )
                .await
                .unwrap()
                .failed
                    >= 1
            );
            std::fs::remove_file(blocked).unwrap();
            identity
                .change_password(&member.session_token, PASSWORD, NEW_PASSWORD)
                .await
                .unwrap();
            execute(
                database,
                &format!(
                    "UPDATE gonof_common_outbox SET available_at = dateline WHERE eventid = {eventid}"
                ),
            )
            .await;
        } else if name.ends_with("expired") {
            execute(database, &format!("UPDATE gonof_common_member_token SET dateline = {}, expires_at = {} WHERE uid = {} AND purpose = 'password_reset'", identity::now() - 3601, identity::now() - 1, member.member.uid)).await;
        } else if name.ends_with("frozen") {
            execute(
                database,
                &format!(
                    "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {}",
                    member.member.uid
                ),
            )
            .await;
        } else {
            execute(database, &format!("UPDATE gonof_common_member SET email = 'mail-security-rebound@example.test' WHERE uid = {}", member.member.uid)).await;
        }
        let delivery =
            gono_forum_web::test_support::deliver_mail(database, &mailbox, ORIGIN, identity::now())
                .await
                .unwrap();
        assert!(delivery.skipped >= 1);
        assert!(
            !mailbox.join(format!("{eventid}.eml")).exists(),
            "a stale reset mail is not delivered: {name}"
        );
        assert_eq!(scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_outbox WHERE eventid = {eventid} AND status = 'failed' AND lease_token IS NULL")).await, 1);
    }
}
