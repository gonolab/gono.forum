use gono_forum_web::db::Database;

async fn execute(db: &Database, statement: &str) {
    gono_forum_web::db::query(statement)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn reject(db: &Database, statement: &str) {
    assert!(
        gono_forum_web::db::query(statement)
            .execute(&db.pool)
            .await
            .is_err(),
        "a write that breaks an identity constraint is rejected: {statement}"
    );
}

/// Writes the database does not reject on the identity tables: the identity check script
/// (`tests/consistency/identity.sql`) catches them instead; the transaction is rolled back and the fixture is
/// unchanged.
async fn flagged(db: &Database, statement: &str, item: &str) {
    gono_forum_web::test_support::assert_identity_rejected_or_flagged(db, statement, item).await;
}

const MEMBER_VALUES: &str = "members with invalid values";
const MEMBER_CREDENTIALS: &str = "members with an invalid combination of credentials";
const INVITE: &str = "invite codes with invalid values or usage records";
const TOKEN: &str = "member tokens with invalid values or an invalid used/revoked combination";

/// MySQL baseline field, unique key and foreign key contracts, plus identity table value rules being caught by the
/// identity check script; cleans up its own fixture afterwards.
pub async fn assert_identity_migration(db: &Database) {
    let version: i64 =
        gono_forum_web::db::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(version, gono_forum_web::db::migrations::LATEST);
    execute(
        db,
        "INSERT INTO gonof_common_member (uid, username) VALUES (9000001, 'migration-author')",
    )
    .await;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, adminid) VALUES (9000002, 'migration-admin', 'password-hash', 1)").await;
    execute(db, "INSERT INTO gonof_common_member (uid, username, password, email, emailstatus) VALUES (9000003, 'migration-user', 'password-hash', 'migration@example.test', 1)").await;
    // The email unique key is still enforced by the database.
    reject(
        db,
        "UPDATE gonof_common_member SET email = 'migration@example.test' WHERE uid = 9000002",
    )
    .await;
    for (statement, item) in [
        (
            "UPDATE gonof_common_member SET email = 'author@example.test' WHERE uid = 9000001",
            MEMBER_CREDENTIALS,
        ),
        (
            "UPDATE gonof_common_member SET password = 'password-hash' WHERE uid = 9000001",
            MEMBER_CREDENTIALS,
        ),
        (
            "UPDATE gonof_common_member SET adminid = 1 WHERE uid = 9000001",
            MEMBER_CREDENTIALS,
        ),
        (
            "UPDATE gonof_common_member SET emailstatus = 1 WHERE uid = 9000002",
            MEMBER_CREDENTIALS,
        ),
        (
            "UPDATE gonof_common_member SET adminid = 2 WHERE uid = 9000002",
            MEMBER_CREDENTIALS,
        ),
        (
            "UPDATE gonof_common_member SET adminid = 6 WHERE uid = 9000003",
            MEMBER_VALUES,
        ),
        (
            "UPDATE gonof_common_member SET emailstatus = 2 WHERE uid = 9000003",
            MEMBER_VALUES,
        ),
        (
            "UPDATE gonof_common_member SET `freeze` = 2 WHERE uid = 9000003",
            MEMBER_VALUES,
        ),
        (
            "UPDATE gonof_common_member SET regdate = -1 WHERE uid = 9000003",
            MEMBER_VALUES,
        ),
        (
            "UPDATE gonof_common_member SET credentials_version = 0 WHERE uid = 9000003",
            MEMBER_VALUES,
        ),
    ] {
        flagged(db, statement, item).await;
    }
    for role in [2, 3, 0] {
        execute(
            db,
            &format!("UPDATE gonof_common_member SET adminid = {role} WHERE uid = 9000003"),
        )
        .await;
    }
    execute(db, "INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime) VALUES (9000002, 'CaseSensitive', 'first', 100, 200)").await;
    execute(db, "INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime) VALUES (9000002, 'CASESENSITIVE', 'second', 100, 200)").await;
    let exact: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_invite WHERE code = 'casesensitive'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(exact, 0, "invite codes are compared case-sensitively");
    for statement in [
        "UPDATE gonof_common_invite SET status = 'used' WHERE code = 'CaseSensitive'",
        "UPDATE gonof_common_invite SET fuid = 9000003 WHERE code = 'CaseSensitive'",
        "UPDATE gonof_common_invite SET status = 'invalid' WHERE code = 'CaseSensitive'",
        "UPDATE gonof_common_invite SET endtime = 99 WHERE code = 'CaseSensitive'",
    ] {
        flagged(db, statement, INVITE).await;
    }
    for statement in [
        "UPDATE gonof_common_invite SET uid = 9999999 WHERE code = 'CaseSensitive'",
        "UPDATE gonof_common_invite SET request_key = 'first' WHERE code = 'CASESENSITIVE'",
        "UPDATE gonof_common_invite SET code = 'CaseSensitive' WHERE code = 'CASESENSITIVE'",
    ] {
        reject(db, statement).await;
    }
    execute(db, "UPDATE gonof_common_invite SET status = 'used', fuid = 9000003, regdateline = 150 WHERE code = 'CaseSensitive'").await;
    reject(db, "UPDATE gonof_common_invite SET status = 'used', fuid = 9000003, regdateline = 150 WHERE code = 'CASESENSITIVE'").await;
    flagged(
        db,
        "UPDATE gonof_common_invite SET regdateline = 201 WHERE code = 'CaseSensitive'",
        INVITE,
    )
    .await;
    let hash = "a".repeat(64);
    execute(db, &format!("INSERT INTO gonof_common_member_token (uid, purpose, token_hash, email, dateline, expires_at) VALUES (9000003, 'email_verification', '{hash}', 'migration@example.test', 100, 200)")).await;
    reject(
        db,
        "UPDATE gonof_common_member_token SET uid = 9999999 WHERE uid = 9000003",
    )
    .await;
    for statement in [
        "UPDATE gonof_common_member_token SET token_hash = 'short' WHERE uid = 9000003",
        "UPDATE gonof_common_member_token SET consumed_at = 201 WHERE uid = 9000003",
        "UPDATE gonof_common_member_token SET consumed_at = 150, revoked_at = 150 WHERE uid = 9000003",
    ] {
        flagged(db, statement, TOKEN).await;
    }
    execute(db, &format!("INSERT INTO gonof_common_session (sid, uid, lastactivity, client_ip) VALUES ('{hash}', 9000003, 100, '127.0.0.1')")).await;
    reject(db, &format!("INSERT INTO gonof_common_session (sid, uid, lastactivity) VALUES ('{hash}', 9000003, 100)")).await;
    // Online records take no part in authentication and keep no old session foreign keys or expiry state.
    let online_columns: Vec<String> = gono_forum_web::db::query_scalar("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='gonof_common_session' ORDER BY ORDINAL_POSITION")
        .sqlite("SELECT name FROM pragma_table_info('gonof_common_session') ORDER BY cid")
        .fetch_all(db.executor()).await.unwrap();
    assert_eq!(online_columns, ["sid", "uid", "lastactivity", "client_ip"]);
    execute(db, "INSERT INTO gonof_common_outbox (event_key, event_type, aggregate_type, aggregate_id, payload, dateline, available_at) VALUES ('migration-mail', 'verify_email', 'member', 9000003, '{}', 100, 100)").await;
    for statement in [
        "UPDATE gonof_common_outbox SET payload = 'not json' WHERE event_key = 'migration-mail'",
        "UPDATE gonof_common_outbox SET available_at = 99 WHERE event_key = 'migration-mail'",
    ] {
        flagged(db, statement, "Outbox events with invalid values").await;
    }
    execute(db, "INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline) VALUES (9000003, 9000002, 'migration.test', '{}', 'test', 100)").await;
    flagged(
        db,
        "UPDATE gonof_common_log SET data = 'not json' WHERE type = 'migration.test'",
        "action log rows with invalid values",
    )
    .await;
    reject(
        db,
        "UPDATE gonof_common_log SET operationuid = 9999999 WHERE type = 'migration.test'",
    )
    .await;
    for statement in [
        "DELETE FROM gonof_common_log WHERE type = 'migration.test'",
        "DELETE FROM gonof_common_outbox WHERE event_key = 'migration-mail'",
        "DELETE FROM gonof_common_session WHERE uid = 9000003",
        "DELETE FROM gonof_common_member_token WHERE uid = 9000003",
        "DELETE FROM gonof_common_invite WHERE uid = 9000002",
        "DELETE FROM gonof_common_member WHERE uid IN (9000001, 9000002, 9000003)",
    ] {
        execute(db, statement).await;
    }
}
