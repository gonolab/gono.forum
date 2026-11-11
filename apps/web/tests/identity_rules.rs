//! The 15 identity and moderation tables have no triggers; their rules are guaranteed by code (docs/architecture.md,
//! "Data"):
//! - Command-line entry points stop deleted, deleting and synthetic accounts themselves;
//! - The identity consistency check `tests/consistency/identity.sql` detects every kind of violation, and all are 0
//!   at the end of each test.
//!
//! Rules expressed as conditional updates, such as status only moving forward, original content never changing and
//! appeals never being deleted, are pinned by the source scan of `db::source_guards`; removing a conditional update
//! makes that unit test fail.

use gono_forum_web::{
    content::profiles,
    db::{self, Database},
    test_support::{TestFixture, assert_identity_consistent, assert_identity_rejected_or_flagged},
};

async fn execute(db: &Database, statement: &str) {
    db::query(statement)
        .execute(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn scalar(db: &Database, statement: &str) -> i64 {
    db::query_scalar(statement)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn text(db: &Database, statement: &str) -> Option<String> {
    db::query_scalar(statement)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

#[tokio::test]
async fn cli_password_and_interaction_patches_respect_the_account_rules() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus) VALUES (9411, 'rules-user', 'hash', 'user9411@example.test', 1)").await;
    execute(
        db,
        "INSERT INTO gonof_common_member (uid, username) VALUES (9412, 'rules-author')",
    )
    .await;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus, account_state) VALUES (9413, 'rules-closing', 'hash', 'user9413@example.test', 1, 'deleting')").await;
    execute(db, "INSERT INTO gonof_common_member (uid, username, space_visibility, account_state) VALUES (9414, 'deleted-9414-000000000000', 'private', 'deleted')").await;

    profiles::set_password(db, 9411, "correct-horse-battery")
        .await
        .unwrap();
    assert_eq!(
        scalar(
            db,
            "SELECT credentials_version FROM gonof_common_member WHERE uid=9411"
        )
        .await,
        2
    );
    // Synthetic authors have no e-mail (with a password they would become "an ordinary member with a password but no
    // e-mail"); accounts in deletion and deleted accounts can no longer set a password.
    // The database does not reject any of the three; the command-line entry point judges them under the lock.
    for uid in [9412, 9413, 9414] {
        assert!(
            profiles::set_password(db, uid, "correct-horse-battery")
                .await
                .is_err(),
            "uid={uid} cannot set a password"
        );
        assert_eq!(
            scalar(
                db,
                &format!("SELECT credentials_version FROM gonof_common_member WHERE uid={uid}")
            )
            .await,
            1,
            "uid={uid}"
        );
    }
    assert_eq!(
        text(
            db,
            "SELECT password FROM gonof_common_member WHERE uid=9412"
        )
        .await,
        None
    );
    assert_eq!(
        text(
            db,
            "SELECT password FROM gonof_common_member WHERE uid=9413"
        )
        .await
        .as_deref(),
        Some("hash")
    );

    // Only normal accounts get an e-mail filled in: a deleted account's e-mail is also empty, and must not get an
    // e-mail and password written back.
    let path = fixture.root.join("identity-rules-roles.json");
    std::fs::write(
        &path,
        r#"{"roles":[{"uid":9412,"username":"rules-author","bio":""},{"uid":9414,"username":"deleted","bio":""}]}"#,
    )
    .unwrap();
    assert_eq!(profiles::enable_interactions(db, &path).await.unwrap(), 1);
    assert_eq!(
        text(db, "SELECT email FROM gonof_common_member WHERE uid=9412")
            .await
            .as_deref(),
        Some("member9412@gonof.test")
    );
    assert_eq!(
        text(db, "SELECT email FROM gonof_common_member WHERE uid=9414").await,
        None
    );
    assert_eq!(
        text(
            db,
            "SELECT password FROM gonof_common_member WHERE uid=9414"
        )
        .await,
        None
    );

    assert_identity_consistent(db).await;
    fixture.cleanup().await.unwrap();
}

/// Tokens, API tokens and invite codes are only written through the entry points of `identity::rules`: invalid values
/// the database does not reject are rejected at the entry point and never stored; revocation time is never earlier
/// than issue time.
#[tokio::test]
async fn write_entries_reject_what_the_triggers_rejected() {
    use gono_forum_web::identity::rules::{self, TokenPurpose};
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus) VALUES (9421, 'rules-entry', 'hash', 'entry@example.test', 1)").await;
    let hash = "a".repeat(64);
    let mut tx = db.begin_write().await.unwrap();
    for (digest, email, dateline, expires) in [
        ("short", "entry@example.test", 100, 200),
        (hash.as_str(), "ab", 100, 200),
        (hash.as_str(), " entry@example.test", 100, 200),
        (hash.as_str(), "entry@example.test", 200, 200),
        (hash.as_str(), "entry@example.test", -1, 200),
    ] {
        assert!(
            rules::insert_token(
                db,
                &mut tx,
                9421,
                TokenPurpose::PasswordReset,
                digest,
                email,
                dateline,
                expires
            )
            .await
            .is_err(),
            "{digest} {email:?} {dateline} {expires}"
        );
    }
    for (digest, hint, dateline, expires) in [
        ("short", "gonof_00", 100, 200),
        (hash.as_str(), "", 100, 200),
        (hash.as_str(), "gonof_0123456789a", 100, 200),
        (hash.as_str(), "gonof_00", 100, 100),
    ] {
        assert!(
            rules::insert_api_key(db, &mut tx, 9421, digest, hint, dateline, expires)
                .await
                .is_err(),
            "{digest} {hint:?} {dateline} {expires}"
        );
    }
    for (code, key, dateline, endtime) in [
        ("", "key", 100, 200),
        (" code", "key", 100, 200),
        ("code", "", 100, 200),
        ("code", "key", 200, 150),
    ] {
        assert!(
            rules::insert_invite(db, &mut tx, 9421, code, key, dateline, endtime)
                .await
                .is_err(),
            "{code:?} {key:?} {dateline} {endtime}"
        );
    }
    // When the revocation time is earlier than issue (clock skew between machines, clock set back), take the issue
    // time.
    rules::insert_api_key(db, &mut tx, 9421, &hash, "gonof_00", 200, 300)
        .await
        .unwrap();
    assert_eq!(
        rules::revoke_api_keys(db, &mut tx, 9421, Some(9421), 150)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        rules::revoke_api_keys(db, &mut tx, 9421, Some(9421), 250)
            .await
            .unwrap(),
        0,
        "revokes only once"
    );
    tx.commit().await.unwrap();
    for (table, expected) in [
        ("gonof_common_member_token", 0),
        ("gonof_common_member_api_key", 1),
        ("gonof_common_invite", 0),
    ] {
        assert_eq!(
            scalar(db, &format!("SELECT COUNT(*) FROM {table} WHERE uid=9421")).await,
            expected,
            "{table}"
        );
    }
    assert_eq!(
        scalar(
            db,
            "SELECT revoked_at FROM gonof_common_member_api_key WHERE uid=9421"
        )
        .await,
        200
    );
    assert_identity_consistent(db).await;
    fixture.cleanup().await.unwrap();
}

/// Every invalid value the database does not reject, written to the database bypassing code, is detected by the
/// matching item of the check script (written in an uncommitted transaction, rolled back at the end).
#[tokio::test]
async fn the_check_script_reports_each_kind_of_drift() {
    // Only verifies the check script, which runs on MySQL only.
    gono_forum_web::mysql_only!();
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let hex = |c: char| c.to_string().repeat(64);
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus) VALUES (9401, 'rules-user', 'hash', 'user@example.test', 1)").await;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus, adminid) VALUES (9402, 'rules-admin', 'hash', 'admin@example.test', 1, 1)").await;
    execute(
        db,
        "INSERT INTO gonof_common_member (uid, username) VALUES (9403, 'rules-author')",
    )
    .await;
    execute(db, "INSERT INTO gonof_common_member_username_history (username, uid, dateline, expires_at) VALUES ('rules-old', 9401, 100, 200)").await;
    execute(db, &format!("INSERT INTO gonof_common_member_role_request (uid, request_key, request_hash, target_uid) VALUES (9402, '{}', '{}', 9401)", hex('a'), hex('b'))).await;
    execute(db, &format!("INSERT INTO gonof_common_member_token (uid, purpose, token_hash, email, dateline, expires_at) VALUES (9401, 'password_reset', '{}', 'user@example.test', 100, 200)", hex('c'))).await;
    execute(db, &format!("INSERT INTO gonof_common_member_api_key (uid, key_hash, key_hint, dateline, expires_at) VALUES (9401, '{}', 'gonof_00', 100, 200)", hex('d'))).await;
    execute(db, "INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime) VALUES (9401, 'rules-code', 'rules-key', 100, 200)").await;
    execute(db, &format!("INSERT INTO gonof_common_member_sanction (uid, kind, reason, operator_uid, starts_at, request_key) VALUES (9401, 'warning', 'reason', 9402, 100, '{}')", hex('e'))).await;
    let sanction = scalar(
        db,
        "SELECT id FROM gonof_common_member_sanction WHERE uid=9401",
    )
    .await;
    execute(db, &format!("INSERT INTO gonof_common_member_appeal (uid, sanction_id, reason, dateline, request_key) VALUES (9401, {sanction}, 'appeal', 150, '{}')", hex('f'))).await;
    execute(db, &format!("INSERT INTO gonof_common_report (uid, target_type, target_id, reason, dateline, request_key) VALUES (9401, 'profile', 9403, 'report', 100, '{}')", hex('1'))).await;
    execute(db, &format!("INSERT INTO gonof_common_data_request (uid, kind, content_mode, status, request_key, request_hash, dateline, execute_after) VALUES (9401, 'export', 'retain', 'pending', '{}', '{}', 100, 100)", hex('b'), hex('3'))).await;
    execute(db, "INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline) VALUES (9401, 9402, 'rules.test', '{}', 'test', 100)").await;
    execute(db, "INSERT INTO gonof_common_outbox (event_key, event_type, aggregate_type, aggregate_id, payload, dateline, available_at) VALUES ('rules-event', 'rules.test', 'member', 9401, '{}', 100, 100)").await;
    execute(
        db,
        "INSERT INTO gonof_common_settings (skey, svalue) VALUES ('rules.test', 'x')",
    )
    .await;
    execute(db, "INSERT INTO gonof_common_site_asset (slot, aid, version, dateline) VALUES ('share', NULL, 1, 100) ON DUPLICATE KEY UPDATE version=version").await;
    assert_identity_consistent(db).await;

    for (statement, item) in [
        (
            "UPDATE gonof_common_member SET avatar=-1 WHERE uid=9401",
            "members with invalid values",
        ),
        (
            "UPDATE gonof_common_member SET username='rules-user ' WHERE uid=9401",
            "members with invalid values",
        ),
        (
            "UPDATE gonof_common_member SET email=NULL, emailstatus=0 WHERE uid=9401",
            "members with an invalid combination of credentials",
        ),
        (
            "UPDATE gonof_common_member SET emailstatus=1 WHERE uid=9403",
            "members with an invalid combination of credentials",
        ),
        (
            "UPDATE gonof_common_member SET space_visibility='public ' WHERE uid=9401",
            "members with an invalid space visibility or account state",
        ),
        (
            "UPDATE gonof_common_member SET account_state='closed' WHERE uid=9401",
            "members with an invalid space visibility or account state",
        ),
        (
            "UPDATE gonof_common_member SET account_state='deleted' WHERE uid=9401",
            "deleted members that still keep an email, password, avatar, bio or staff role",
        ),
        (
            "UPDATE gonof_common_member_username_history SET expires_at=100 WHERE uid=9401",
            "rename records with invalid values",
        ),
        (
            "UPDATE gonof_common_member_role_request SET request_hash='short' WHERE uid=9402",
            "role applications whose request key or digest is not 64 characters",
        ),
        (
            "UPDATE gonof_common_member_token SET revoked_at=99 WHERE uid=9401",
            "member tokens with invalid values or an invalid used/revoked combination",
        ),
        (
            "UPDATE gonof_common_member_token SET purpose='password_reset ' WHERE uid=9401",
            "member tokens with invalid values or an invalid used/revoked combination",
        ),
        (
            "UPDATE gonof_common_member_api_key SET revoked_by=9402 WHERE uid=9401",
            "API tokens with invalid values",
        ),
        (
            "UPDATE gonof_common_invite SET status='used' WHERE uid=9401",
            "invite codes with invalid values or usage records",
        ),
        (
            "UPDATE gonof_common_member_sanction SET expires_at=200 WHERE uid=9401",
            "sanctions with invalid values or an invalid revocation combination",
        ),
        (
            "UPDATE gonof_common_member_sanction SET revoked_at=150 WHERE uid=9401",
            "sanctions with invalid values or an invalid revocation combination",
        ),
        (
            "UPDATE gonof_common_report SET status='resolved' WHERE uid=9401",
            "reports with invalid values or an invalid handling combination",
        ),
        (
            "UPDATE gonof_common_member_appeal SET status='accepted', handler_uid=9402, resolution='resolution', handled_at=120 WHERE uid=9401",
            "appeals with invalid values or an invalid handling combination",
        ),
        (
            "UPDATE gonof_common_member_appeal SET dateline=2592101 WHERE uid=9401",
            "appeals against a sanction of another member or after the 30-day appeal period",
        ),
        (
            "UPDATE gonof_common_data_request SET execute_after=99 WHERE uid=9401",
            "data requests with invalid values",
        ),
        (
            "UPDATE gonof_common_data_request SET request_key=UPPER(request_key) WHERE uid=9401",
            "data requests with invalid values",
        ),
        (
            "UPDATE gonof_common_log SET data='not json' WHERE type='rules.test'",
            "action log rows with invalid values",
        ),
        (
            "UPDATE gonof_common_outbox SET status='queued' WHERE event_key='rules-event'",
            "Outbox events with invalid values",
        ),
        (
            "UPDATE gonof_common_settings SET skey='' WHERE skey='rules.test'",
            "settings with an empty key",
        ),
        (
            "UPDATE gonof_common_site_asset SET slot='banner' WHERE slot='share'",
            "site assets with an invalid slot or version",
        ),
    ] {
        assert_identity_rejected_or_flagged(db, statement, item).await;
    }
    assert_identity_consistent(db).await;
    fixture.cleanup().await.unwrap();
}
