//! Public profile and email-change contracts shared by the three database engines.
use std::path::Path;

use gono_forum_web::{
    db::{self, Database},
    identity::{self, Identity},
};

use super::{ORIGIN, PASSWORD, register_member, registration_counts, scalar, verification_token};

const NEXT_PASSWORD: &str = "Profile-new-password-8593";

async fn execute(database: &Database, statement: &str) {
    gono_forum_web::db::raw_sql(statement)
        .execute(database.executor())
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

pub async fn assert_profile_settings(
    identity: &Identity,
    database: &Database,
    root: &Path,
    admin: i64,
) {
    assert_profile_edits(identity, database, admin).await;
    assert_username_namespace(identity, database, admin).await;
    assert_revoked_access(identity, database, admin).await;
    assert_email_change(identity, database, admin).await;
    assert_email_failures(identity, database, admin).await;
    assert_atomic_changes(identity, database, admin).await;
    assert_email_delivery(identity, database, root, admin).await;
}

async fn token_mail(database: &Database, uid: i64, purpose: &str) -> (i64, String) {
    let row = gono_forum_web::db::query("SELECT eventid, payload FROM gonof_common_outbox WHERE aggregate_id = ? AND event_type = ? ORDER BY eventid DESC LIMIT 1")
        .bind(uid).bind(purpose).fetch_one(database.executor()).await.unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&db::text(&row, "payload").unwrap()).unwrap();
    (
        row.try_get("eventid").unwrap(),
        payload["token"].as_str().unwrap().to_owned(),
    )
}

async fn request_change(
    identity: &Identity,
    database: &Database,
    sid: &str,
    uid: i64,
    target: &str,
) -> String {
    identity
        .request_email_change(sid, PASSWORD, target)
        .await
        .unwrap();
    token_mail(database, uid, "email_change").await.1
}

async fn state(database: &Database, uid: i64) -> (Vec<String>, Vec<i64>) {
    let row = gono_forum_web::db::query(
        "SELECT username, email, password, bio FROM gonof_common_member WHERE uid = ?",
    )
    .bind(uid)
    .fetch_one(database.executor())
    .await
    .unwrap();
    let strings = ["username", "email", "password", "bio"]
        .into_iter()
        .map(|column| {
            row.try_get::<Option<String>, _>(column)
                .unwrap()
                .unwrap_or_default()
        })
        .collect();
    let mut counts = Vec::new();
    for query in [
        format!("SELECT credentials_version FROM gonof_common_member WHERE uid = {uid}"),
        format!("SELECT emailstatus FROM gonof_common_member WHERE uid = {uid}"),
        format!("SELECT COUNT(*) FROM gonof_common_member_username_history WHERE uid = {uid}"),
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
    (strings, counts)
}

async fn assert_profile_edits(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "profile-original").await;
    let other = identity.login("profile-original", PASSWORD).await.unwrap();
    let initial = identity.profile(&member.session_token).await.unwrap();
    assert_eq!(initial.member.username, "profile-original");
    assert!(initial.bio.is_empty());
    assert!(initial.pending_email.is_none());
    assert!(initial.username_available_at <= identity::now());
    identity
        .update_profile(
            &member.session_token,
            " PROFILE-ORIGINAL ",
            "  Line one\r\nLine two\rLine three\t<b>plain text</b>  ",
        )
        .await
        .unwrap();
    let profile = identity.profile(&member.session_token).await.unwrap();
    assert_eq!(
        profile.bio,
        "Line one\nLine two\nLine three\t<b>plain text</b>"
    );
    assert_eq!(
        profile.member.credentials_version,
        member.member.credentials_version
    );
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_member_username_history WHERE uid = {}",
                member.member.uid
            )
        )
        .await,
        0
    );
    // Three-byte euro signs and a four-byte emoji: the 500 limit counts characters, not bytes.
    let long = "€€🍓".repeat(166) + "ok";
    assert_eq!(long.chars().count(), 500);
    identity
        .update_profile(&member.session_token, "profile-original", &long)
        .await
        .unwrap();
    assert_eq!(
        identity.profile(&member.session_token).await.unwrap().bio,
        long
    );
    let before = state(database, member.member.uid).await;
    for bio in [
        long + "x",
        "has a \0 control character".into(),
        "has a \u{7f} control character".into(),
    ] {
        assert!(
            identity
                .update_profile(&member.session_token, "profile-original", &bio)
                .await
                .is_err()
        );
        assert_eq!(state(database, member.member.uid).await, before);
    }
    // Full-width "PROFILE-RENAMED": NFKC and lowercasing turn it into "profile-renamed".
    identity
        .update_profile(
            &member.session_token,
            " \u{ff30}\u{ff32}\u{ff2f}\u{ff26}\u{ff29}\u{ff2c}\u{ff25}\u{ff0d}\u{ff32}\u{ff25}\u{ff2e}\u{ff21}\u{ff2d}\u{ff25}\u{ff24} ",
            "bio after the rename",
        )
        .await
        .unwrap();
    let renamed = identity.profile(&member.session_token).await.unwrap();
    assert_eq!(renamed.member.username, "profile-renamed");
    assert_eq!(renamed.member.uid, member.member.uid);
    assert_eq!(
        renamed.member.credentials_version,
        member.member.credentials_version
    );
    assert!(
        identity
            .session(&other.session_token)
            .await
            .unwrap()
            .is_some()
    );
    assert!(identity.login("profile-original", PASSWORD).await.is_err());
    assert!(identity.login("profile-renamed", PASSWORD).await.is_ok());
    assert!(
        identity
            .login("profile-original@example.test", PASSWORD)
            .await
            .is_ok()
    );
    let history = gono_forum_web::db::query("SELECT uid, dateline, expires_at FROM gonof_common_member_username_history WHERE username = 'profile-original'")
        .fetch_one(database.executor()).await.unwrap();
    assert_eq!(history.try_get::<i64, _>("uid").unwrap(), member.member.uid);
    let changed = history.try_get::<i64, _>("dateline").unwrap();
    assert_eq!(
        history.try_get::<i64, _>("expires_at").unwrap() - changed,
        90 * 86_400
    );
    assert_eq!(renamed.username_available_at, changed + 30 * 86_400);
    let before = state(database, member.member.uid).await;
    assert!(
        identity
            .update_profile(&member.session_token, "profile-third", "must not be stored")
            .await
            .is_err()
    );
    assert_eq!(state(database, member.member.uid).await, before);
    identity
        .update_profile(
            &member.session_token,
            "profile-renamed",
            "the bio can change during the cooldown",
        )
        .await
        .unwrap();
    assert_eq!(
        identity.profile(&member.session_token).await.unwrap().bio,
        "the bio can change during the cooldown"
    );
    let old = identity::now() - 31 * 86_400;
    execute(database, &format!("UPDATE gonof_common_member_username_history SET dateline = {old}, expires_at = {} WHERE uid = {}", old + 90 * 86_400, member.member.uid)).await;
    identity
        .update_profile(
            &member.session_token,
            "profile-original",
            "reclaiming my own reserved name",
        )
        .await
        .unwrap();
    let restored = identity.profile(&member.session_token).await.unwrap();
    assert_eq!(restored.member.username, "profile-original");
    assert!(restored.username_available_at > identity::now());
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_member_username_history WHERE uid = {}",
                member.member.uid
            )
        )
        .await,
        2
    );
}

async fn assert_username_namespace(identity: &Identity, database: &Database, admin: i64) {
    let owner = register_member(identity, admin, "profile-protected").await;
    let outsider = register_member(identity, admin, "profile-outsider").await;
    identity
        .update_profile(&owner.session_token, "profile-protected-new", "")
        .await
        .unwrap();
    let before = state(database, outsider.member.uid).await;
    for name in [
        "profile-protected",
        "profile-protected-new",
        "",
        "has space",
        "x@y",
        "admin",
        &"x".repeat(51),
    ] {
        assert!(
            identity
                .update_profile(
                    &outsider.session_token,
                    name,
                    "a failure must not change the bio"
                )
                .await
                .is_err()
        );
        assert_eq!(state(database, outsider.member.uid).await, before);
    }
    let invite = identity
        .generate_invite(admin, "profile-protected-register")
        .await
        .unwrap();
    let counts = registration_counts(database).await;
    assert!(
        identity
            .register(
                "profile-protected",
                "protected-register@example.test",
                PASSWORD,
                &invite.code
            )
            .await
            .is_err()
    );
    assert_eq!(
        registration_counts(database).await,
        counts,
        "a reserved name conflict rolls back the invite code and the whole registration"
    );
    let expired = identity::now() - 1;
    execute(database, &format!("UPDATE gonof_common_member_username_history SET dateline = {}, expires_at = {expired} WHERE username = 'profile-protected'", expired - 90 * 86_400)).await;
    identity
        .register(
            "profile-protected",
            "protected-register@example.test",
            PASSWORD,
            &invite.code,
        )
        .await
        .unwrap();

    let rename = register_member(identity, admin, "profile-name-race").await;
    let signup = identity
        .generate_invite(admin, "profile-name-race-register")
        .await
        .unwrap();
    let (renamed, registered) = tokio::join!(
        identity.update_profile(&rename.session_token, "profile-contested", "racing rename"),
        identity.register(
            "profile-contested",
            "profile-contested-register@example.test",
            PASSWORD,
            &signup.code
        ),
    );
    assert_ne!(
        renamed.is_ok(),
        registered.is_ok(),
        "a registration and a rename cannot both take the same normalized username"
    );
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_member WHERE username = 'profile-contested'"
        )
        .await,
        1
    );
    let releasing = register_member(identity, admin, "profile-release-race").await;
    let signup = identity
        .generate_invite(admin, "profile-release-race-register")
        .await
        .unwrap();
    let (renamed, registered) = tokio::join!(
        identity.update_profile(
            &releasing.session_token,
            "profile-release-renamed",
            "handover protection"
        ),
        identity.register(
            "profile-release-race",
            "profile-release-register@example.test",
            PASSWORD,
            &signup.code
        ),
    );
    renamed.unwrap();
    assert!(
        registered.is_err(),
        "a registration cannot take the old username while it moves from the member table to the history"
    );
    assert_eq!(scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_username_history WHERE username = 'profile-release-race' AND uid = {}", releasing.member.uid)).await, 1);
    let duplicate = register_member(identity, admin, "profile-double-rename").await;
    let (left, right) = tokio::join!(
        identity.update_profile(&duplicate.session_token, "profile-double-left", "left"),
        identity.update_profile(&duplicate.session_token, "profile-double-right", "right"),
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "only one of two concurrent renames of one account passes the cooldown check"
    );
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_member_username_history WHERE uid = {}",
                duplicate.member.uid
            )
        )
        .await,
        1
    );
}

async fn assert_revoked_access(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "profile-access").await;
    let management_id = identity::token_hash(&member.session_token);
    assert!(identity.profile(&management_id).await.is_err());
    identity.logout_all(&member.session_token).await.unwrap();
    let before = state(database, member.member.uid).await;
    assert!(identity.profile(&member.session_token).await.is_err());
    assert!(
        identity
            .update_profile(
                &member.session_token,
                "profile-access",
                "must not be written"
            )
            .await
            .is_err()
    );
    assert!(
        identity
            .request_email_change(&member.session_token, PASSWORD, "access-new@example.test")
            .await
            .is_err()
    );
    assert_eq!(state(database, member.member.uid).await, before);
    let fresh = identity.login("profile-access", PASSWORD).await.unwrap();
    execute(
        database,
        &format!(
            "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {}",
            member.member.uid
        ),
    )
    .await;
    let before = state(database, member.member.uid).await;
    assert!(identity.profile(&fresh.session_token).await.is_err());
    assert!(
        identity
            .update_profile(
                &fresh.session_token,
                "profile-access",
                "must not be written"
            )
            .await
            .is_err()
    );
    assert!(
        identity
            .request_email_change(&fresh.session_token, PASSWORD, "access-new@example.test")
            .await
            .is_err()
    );
    assert_eq!(state(database, member.member.uid).await, before);
}

async fn assert_email_change(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "profile-email").await;
    let other = identity.login("profile-email", PASSWORD).await.unwrap();
    let verification = verification_token(database, member.member.uid).await;
    identity
        .request_password_reset("profile-email@example.test")
        .await
        .unwrap();
    let reset = token_mail(database, member.member.uid, "password_reset")
        .await
        .1;
    let before = state(database, member.member.uid).await;
    assert!(
        identity
            .request_email_change(
                &member.session_token,
                "Wrong-password-8593",
                "profile-email-new@example.test"
            )
            .await
            .is_err()
    );
    assert!(
        identity
            .request_email_change(&member.session_token, PASSWORD, "not-an-email")
            .await
            .is_err()
    );
    assert!(
        identity
            .request_email_change(
                &member.session_token,
                PASSWORD,
                "profile-email@example.test"
            )
            .await
            .is_err()
    );
    assert_eq!(state(database, member.member.uid).await, before);
    // Full-width "PROFILE-EMAIL-NEW@EXAMPLE.TEST": NFKC and lowercasing turn it into the ASCII address.
    let token = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        " \u{ff30}\u{ff32}\u{ff2f}\u{ff26}\u{ff29}\u{ff2c}\u{ff25}\u{ff0d}\u{ff25}\u{ff2d}\u{ff21}\u{ff29}\u{ff2c}\u{ff0d}\u{ff2e}\u{ff25}\u{ff37}\u{ff20}\u{ff25}\u{ff38}\u{ff21}\u{ff2d}\u{ff30}\u{ff2c}\u{ff25}\u{ff0e}\u{ff34}\u{ff25}\u{ff33}\u{ff34} ",
    )
    .await;
    let pending = identity.profile(&member.session_token).await.unwrap();
    assert_eq!(
        pending.member.email.as_deref(),
        Some("profile-email@example.test")
    );
    assert!(!pending.member.email_verified);
    assert_eq!(
        pending.pending_email.as_deref(),
        Some("profile-email-new@example.test")
    );
    assert_eq!(
        identity
            .email_change_status(&token)
            .await
            .unwrap()
            .as_deref(),
        Some("profile-email-new@example.test")
    );
    assert!(
        identity
            .email_change_status(&token)
            .await
            .unwrap()
            .is_some(),
        "viewing the confirmation link does not consume the token"
    );
    let row = gono_forum_web::db::query("SELECT token_hash, expires_at - dateline AS lifetime FROM gonof_common_member_token WHERE uid = ? AND purpose = 'email_change'")
        .bind(member.member.uid).fetch_one(database.executor()).await.unwrap();
    assert_eq!(
        db::text(&row, "token_hash").unwrap(),
        identity::token_hash(&token)
    );
    assert_eq!(row.try_get::<i64, _>("lifetime").unwrap(), 86_400);
    assert!(identity.confirm_email_change(&verification).await.is_err());
    assert!(identity.confirm_email_change(&reset).await.is_err());
    assert!(identity.verify_email(&token).await.is_err());
    assert!(
        identity
            .reset_password(&token, NEXT_PASSWORD)
            .await
            .is_err()
    );
    let (left, right) = tokio::join!(
        identity.confirm_email_change(&token),
        identity.confirm_email_change(&token)
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "an email change link is consumed atomically, once"
    );
    assert!(
        identity
            .email_change_status(&token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(identity.confirm_email_change(&token).await.is_err());
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
    assert!(
        identity
            .login("profile-email@example.test", PASSWORD)
            .await
            .is_err()
    );
    let logged_in = identity
        .login("profile-email-new@example.test", PASSWORD)
        .await
        .unwrap();
    assert_eq!(logged_in.member.username, "profile-email");
    assert!(logged_in.member.email_verified);
    assert_eq!(
        logged_in.member.credentials_version,
        member.member.credentials_version + 1
    );
    assert!(identity.login("profile-email", PASSWORD).await.is_ok());
    assert!(
        identity
            .profile(&logged_in.session_token)
            .await
            .unwrap()
            .pending_email
            .is_none()
    );
    assert!(!identity.verification_status(&verification).await.unwrap());
    assert!(!identity.password_reset_status(&reset).await.unwrap());
}

async fn assert_email_failures(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "profile-email-reissue").await;
    let old = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        "profile-reissue-old@example.test",
    )
    .await;
    let replacement = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        "profile-reissue-new@example.test",
    )
    .await;
    assert!(identity.email_change_status(&old).await.unwrap().is_none());
    assert!(identity.confirm_email_change(&old).await.is_err());
    assert!(
        identity
            .email_change_status(&replacement)
            .await
            .unwrap()
            .is_some()
    );
    identity
        .change_password(&member.session_token, PASSWORD, NEXT_PASSWORD)
        .await
        .unwrap();
    assert!(
        identity
            .email_change_status(&replacement)
            .await
            .unwrap()
            .is_none()
    );
    assert!(identity.confirm_email_change(&replacement).await.is_err());

    let reset_member = register_member(identity, admin, "profile-email-reset").await;
    let token = request_change(
        identity,
        database,
        &reset_member.session_token,
        reset_member.member.uid,
        "profile-reset-target@example.test",
    )
    .await;
    identity
        .request_password_reset("profile-email-reset@example.test")
        .await
        .unwrap();
    let reset = token_mail(database, reset_member.member.uid, "password_reset")
        .await
        .1;
    identity
        .reset_password(&reset, NEXT_PASSWORD)
        .await
        .unwrap();
    assert!(
        identity
            .email_change_status(&token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(identity.confirm_email_change(&token).await.is_err());

    for name in ["profile-email-expired", "profile-email-frozen"] {
        let member = register_member(identity, admin, name).await;
        let token = request_change(
            identity,
            database,
            &member.session_token,
            member.member.uid,
            &format!("{name}-new@example.test"),
        )
        .await;
        if name.ends_with("expired") {
            let expired = identity::now() - 1;
            execute(database, &format!("UPDATE gonof_common_member_token SET dateline = {}, expires_at = {expired} WHERE uid = {} AND purpose = 'email_change'", expired - 86_400, member.member.uid)).await;
        } else {
            execute(
                database,
                &format!(
                    "UPDATE gonof_common_member SET `freeze` = 1 WHERE uid = {}",
                    member.member.uid
                ),
            )
            .await;
        }
        let before = state(database, member.member.uid).await;
        assert!(
            identity
                .email_change_status(&token)
                .await
                .unwrap()
                .is_none()
        );
        assert!(identity.confirm_email_change(&token).await.is_err());
        assert_eq!(state(database, member.member.uid).await, before);
    }
    assert!(
        identity
            .email_change_status("invalid")
            .await
            .unwrap()
            .is_none()
    );
    assert!(identity.confirm_email_change("invalid").await.is_err());

    let left = register_member(identity, admin, "profile-email-contest-left").await;
    let right = register_member(identity, admin, "profile-email-contest-right").await;
    assert!(
        identity
            .request_email_change(
                &left.session_token,
                PASSWORD,
                "profile-email-contest-right@example.test"
            )
            .await
            .is_err()
    );
    let left_token = request_change(
        identity,
        database,
        &left.session_token,
        left.member.uid,
        "profile-email-contested@example.test",
    )
    .await;
    let right_token = request_change(
        identity,
        database,
        &right.session_token,
        right.member.uid,
        "profile-email-contested@example.test",
    )
    .await;
    let (left_result, right_result) = tokio::join!(
        identity.confirm_email_change(&left_token),
        identity.confirm_email_change(&right_token)
    );
    assert_ne!(
        left_result.is_ok(),
        right_result.is_ok(),
        "two accounts cannot both confirm the same email"
    );
    assert_eq!(scalar(database, "SELECT COUNT(*) FROM gonof_common_member WHERE email = 'profile-email-contested@example.test'").await, 1);
    let loser = if left_result.is_err() { &left } else { &right };
    assert_eq!(
        scalar(
            database,
            &format!(
                "SELECT credentials_version FROM gonof_common_member WHERE uid = {}",
                loser.member.uid
            )
        )
        .await,
        loser.member.credentials_version
    );
    assert!(
        identity
            .session(&loser.session_token)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(scalar(database, &format!("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid = {} AND purpose = 'email_change' AND consumed_at IS NOT NULL", loser.member.uid)).await, 0);
}

async fn install_failure(database: &Database, table: &str, condition: &str) {
    gono_forum_web::test_support::fail_when(
        database,
        "reject_profile_write",
        "BEFORE INSERT",
        table,
        condition,
        "profile rollback test",
    )
    .await
    .unwrap()
}

async fn remove_failure(database: &Database, _table: &str) {
    {
        gono_forum_web::test_support::drop_trigger(database, "reject_profile_write")
            .await
            .unwrap();
    }
}

async fn assert_atomic_changes(identity: &Identity, database: &Database, admin: i64) {
    let member = register_member(identity, admin, "profile-atomic").await;
    let before = state(database, member.member.uid).await;
    install_failure(
        database,
        "gonof_common_log",
        &format!("NEW.uid = {}", member.member.uid),
    )
    .await;
    assert!(
        identity
            .update_profile(
                &member.session_token,
                "profile-atomic-new",
                "must not commit"
            )
            .await
            .is_err()
    );
    remove_failure(database, "gonof_common_log").await;
    assert_eq!(
        state(database, member.member.uid).await,
        before,
        "an audit failure rolls back the rename, the bio and the reserved name history"
    );

    let original = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        "profile-atomic-old@example.test",
    )
    .await;
    let before = state(database, member.member.uid).await;
    install_failure(
        database,
        "gonof_common_outbox",
        &format!(
            "NEW.aggregate_id = {} AND NEW.event_type = 'email_change'",
            member.member.uid
        ),
    )
    .await;
    assert!(
        identity
            .request_email_change(
                &member.session_token,
                PASSWORD,
                "profile-atomic-new@example.test"
            )
            .await
            .is_err()
    );
    remove_failure(database, "gonof_common_outbox").await;
    assert_eq!(
        state(database, member.member.uid).await,
        before,
        "a failure to queue the mail restores the email change token that was valid before"
    );
    assert!(
        identity
            .email_change_status(&original)
            .await
            .unwrap()
            .is_some()
    );

    identity
        .request_password_reset("profile-atomic@example.test")
        .await
        .unwrap();
    let reset = token_mail(database, member.member.uid, "password_reset")
        .await
        .1;
    let before = state(database, member.member.uid).await;
    install_failure(
        database,
        "gonof_common_log",
        &format!("NEW.uid = {}", member.member.uid),
    )
    .await;
    assert!(identity.confirm_email_change(&original).await.is_err());
    remove_failure(database, "gonof_common_log").await;
    assert_eq!(
        state(database, member.member.uid).await,
        before,
        "an audit failure on confirmation rolls back email, verification state, version, all tokens and sessions"
    );
    assert!(
        identity
            .email_change_status(&original)
            .await
            .unwrap()
            .is_some()
    );
    assert!(identity.password_reset_status(&reset).await.unwrap());
    assert!(
        identity
            .session(&member.session_token)
            .await
            .unwrap()
            .is_some()
    );
    identity.confirm_email_change(&original).await.unwrap();
}

async fn assert_email_delivery(identity: &Identity, database: &Database, root: &Path, admin: i64) {
    let directory = root.join(".local/profile-mail");
    while gono_forum_web::test_support::deliver_mail(database, &directory, ORIGIN, identity::now())
        .await
        .unwrap()
        .claimed
        > 0
    {}
    let member = register_member(identity, admin, "profile-mail").await;
    let old = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        "profile-mail-old@example.test",
    )
    .await;
    let (old_event, _) = token_mail(database, member.member.uid, "email_change").await;
    let token = request_change(
        identity,
        database,
        &member.session_token,
        member.member.uid,
        "profile-mail-new@example.test",
    )
    .await;
    let (event, _) = token_mail(database, member.member.uid, "email_change").await;
    let original_payload: String = gono_forum_web::db::query_scalar(
        "SELECT payload FROM gonof_common_outbox WHERE eventid = ?",
    )
    .bind(event)
    .fetch_one(database.executor())
    .await
    .unwrap();
    gono_forum_web::test_support::deliver_mail(database, &directory, ORIGIN, identity::now())
        .await
        .unwrap();
    assert!(
        !directory.join(format!("{old_event}.eml")).exists(),
        "a replaced email change token is not delivered"
    );
    assert!(identity.email_change_status(&old).await.unwrap().is_none());
    let path = directory.join(format!("{event}.eml"));
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.starts_with("To: profile-mail-new@example.test\r\n"));
    assert!(
        contents.contains("Subject: [gonof] Confirm your new email\r\n"),
        "{contents}"
    );
    assert!(!contents.contains(PASSWORD));
    let link = contents
        .lines()
        .find(|line| line.starts_with(ORIGIN) && line.contains(&format!("token={token}")))
        .expect("the email change mail carries this site's one-time confirmation link");
    assert_eq!(
        url::Url::parse(link).unwrap().path(),
        "/confirm-email-change"
    );
    assert_eq!(
        url::Url::parse(link)
            .unwrap()
            .origin()
            .ascii_serialization(),
        ORIGIN
    );
    assert!(
        identity
            .email_change_status(&token)
            .await
            .unwrap()
            .is_some(),
        "delivery must not consume the email change token early"
    );
    // Simulate a crash after sending but before confirming: confirmation and token clearing are in one transaction,
    // so the payload is still there after the crash.
    gono_forum_web::db::query("UPDATE gonof_common_outbox SET status = 'processing', payload = ?, lease_token = 'profile-crashed-worker', lease_until = 0 WHERE eventid = ?")
        .bind(&original_payload)
        .bind(event)
        .execute(database.executor())
        .await
        .unwrap();
    assert_eq!(
        gono_forum_web::test_support::deliver_mail(database, &directory, ORIGIN, identity::now())
            .await
            .unwrap()
            .delivered,
        1
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    identity.confirm_email_change(&token).await.unwrap();
}
