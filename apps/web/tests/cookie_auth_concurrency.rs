//! Isolated-database contracts for Cookie authentication and online statistics; does not touch global services.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures_util::FutureExt;
use gono_forum_web::{
    db::{self, Database},
    debug::trace::{self, QueryRecord, QuerySource, RequestProfile},
    identity::{Identity, IdentityError, SessionGrant},
    test_support::TestFixture,
};

const UID: i64 = 7;

async fn test_database(fixture: &TestFixture) -> Database {
    // The audit barrier, sign-out and three competing writes each take one connection; does not depend on the pool size
    // in the user's configuration.
    let mut config = fixture.config.database.clone();
    config.max_connections = 6;
    Database::connect(&config).await.unwrap()
}

async fn subject(fixture: &TestFixture, database: &Database) -> (Identity, SessionGrant) {
    // The administrator can issue invite codes and the unverified email can resend the verification email, so all three
    // entry points are in a state where they can succeed.
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid) VALUES (?, 'cookie-lock-subject', 'cookie-lock@example.test', 'fixture-pending-hash', 0, 1)")
        .bind(UID).execute(database.executor()).await.unwrap();
    let identity = Identity::new(
        database.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let grant = http_auth::login(&identity, database, UID).await;
    (identity, grant)
}

fn application(profile: &RequestProfile) -> Vec<QueryRecord> {
    profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .collect()
}

async fn wait_for_query(profile: &RequestProfile, fragment: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if application(profile)
                .iter()
                .any(|record| record.sql.contains(fragment))
            {
                return;
            }
            // Wait for an observable query-start event; the interval only lowers polling overhead and does not decide
            // the concurrency order.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the query blocked at the barrier must be seen starting");
}

async fn scalar(database: &Database, statement: &str) -> i64 {
    db::query_scalar(statement)
        .fetch_one(database.executor())
        .await
        .unwrap()
}

async fn locked_writes_contract(fixture: &TestFixture, database: &Database) {
    let (identity, grant) = subject(fixture, database).await;
    let raw = grant.session_token.as_str();
    let target = identity
        .generate_invite_for_session(raw, "revoke-target")
        .await
        .unwrap();
    let preflight = identity
        .generate_invite_for_session(raw, "successful-preflight")
        .await
        .unwrap();
    identity
        .revoke_invite_for_session(raw, preflight.id)
        .await
        .unwrap();
    identity.resend_verification_for_session(raw).await.unwrap();
    let mail_before = scalar(database, "SELECT COUNT(*) FROM gonof_common_outbox").await;
    let tokens_before = scalar(
        database,
        "SELECT COUNT(*) FROM gonof_common_member_token WHERE revoked_at IS NULL",
    )
    .await;
    for _ in 0..3 {
        assert!(
            identity.session(raw).await.unwrap().is_some(),
            "all three requests first get a valid identity context"
        );
    }

    // Lock the end gap of the audit auto-increment index: logout_all will first take the member lock and update the
    // version, then wait at the existing audit INSERT. This does not depend on the first-come-first-served order of the
    // member lock wait queue.
    let mut audit_connection = database.pool.acquire().await.unwrap();
    db::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(database.observe(&mut *audit_connection))
        .await
        .unwrap();
    let mut audit_barrier = audit_connection.begin().await.unwrap();
    let next_log_id = scalar(
        database,
        "SELECT COALESCE(MAX(id), 0) + 1 FROM gonof_common_log",
    )
    .await;
    let empty = db::query("SELECT id FROM gonof_common_log WHERE id >= ? FOR UPDATE")
        .bind(next_log_id)
        .fetch_all(database.observe(&mut *audit_barrier))
        .await
        .unwrap();
    assert!(empty.is_empty());

    let logout_profile = Arc::new(RequestProfile::new());
    let generate_profile = Arc::new(RequestProfile::new());
    let revoke_profile = Arc::new(RequestProfile::new());
    let resend_profile = Arc::new(RequestProfile::new());
    let logout = trace::scope(logout_profile.clone(), identity.logout_all(raw));
    let generate = async {
        wait_for_query(&logout_profile, "INSERT INTO gonof_common_log").await;
        trace::scope(
            generate_profile.clone(),
            identity.generate_invite_for_session(raw, "must-not-generate"),
        )
        .await
    };
    let revoke = async {
        wait_for_query(&logout_profile, "INSERT INTO gonof_common_log").await;
        trace::scope(
            revoke_profile.clone(),
            identity.revoke_invite_for_session(raw, target.id),
        )
        .await
    };
    let resend = async {
        wait_for_query(&logout_profile, "INSERT INTO gonof_common_log").await;
        trace::scope(
            resend_profile.clone(),
            identity.resend_verification_for_session(raw),
        )
        .await
    };
    let release = async {
        for profile in [&generate_profile, &revoke_profile, &resend_profile] {
            wait_for_query(profile, "FROM gonof_common_member WHERE uid = ? FOR UPDATE").await;
            let records = application(profile);
            let pending = records
                .iter()
                .find(|record| {
                    record
                        .sql
                        .contains("FROM gonof_common_member WHERE uid = ? FOR UPDATE")
                })
                .unwrap();
            assert_eq!(
                pending.rows_returned, 0,
                "the write entry point waits on the member lock held by logout_all"
            );
            assert!(
                pending.elapsed.is_zero(),
                "the member query has not finished yet"
            );
        }
        audit_barrier.commit().await.unwrap();
    };
    let (logout, generate, revoke, resend, ()) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(logout, generate, revoke, resend, release)
        })
        .await
        .expect("every account operation finishes once the lock is released");
    logout.unwrap();
    assert!(matches!(generate, Err(IdentityError::Unauthorized)));
    assert!(matches!(revoke, Err(IdentityError::Unauthorized)));
    assert!(matches!(resend, Err(IdentityError::Unauthorized)));
    assert!(identity.session(raw).await.unwrap().is_none());
    let version: i64 =
        db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid = ?")
            .bind(UID)
            .fetch_one(database.executor())
            .await
            .unwrap();
    assert_eq!(version, grant.member.credentials_version + 1);
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_log WHERE type = 'identity.session.logout_all'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_invite WHERE request_key = 'must-not-generate'"
        )
        .await,
        0
    );
    let target_status: String =
        db::query_scalar("SELECT status FROM gonof_common_invite WHERE id = ?")
            .bind(target.id)
            .fetch_one(database.executor())
            .await
            .unwrap();
    assert_eq!(target_status, "available");
    assert_eq!(
        scalar(database, "SELECT COUNT(*) FROM gonof_common_outbox").await,
        mail_before
    );
    assert_eq!(
        scalar(
            database,
            "SELECT COUNT(*) FROM gonof_common_member_token WHERE revoked_at IS NULL"
        )
        .await,
        tokens_before
    );
    for (profile, has_runtime_guard) in [
        (&generate_profile, true),
        (&revoke_profile, false),
        (&resend_profile, false),
    ] {
        let records = application(profile);
        let member_queries: Vec<_> = records
            .iter()
            .filter(|record| {
                record
                    .sql
                    .contains("FROM gonof_common_member WHERE uid = ? FOR UPDATE")
            })
            .collect();
        assert_eq!(
            member_queries.len(),
            1,
            "the failing entry point locks and rechecks the member only once"
        );
        assert_eq!(member_queries[0].rows_returned, 1);
        assert!(member_queries[0].error.is_none());
        // Generating invite codes first holds the runtime shared lock (to be blocked during read-only mode); revoking
        // invite codes and resending security emails are not restricted by the read-only switch.
        // Explicitly enumerate the allowed queries and their order; a relaxed total must not mask business writes after
        // an authentication failure.
        assert_eq!(records.len(), 1 + usize::from(has_runtime_guard));
        if has_runtime_guard {
            assert_eq!(records[0].sql, *gono_forum_web::runtime::GATE_SQL);
            assert!(records[0].error.is_none());
        }
        assert!(
            records
                .last()
                .unwrap()
                .sql
                .contains("FROM gonof_common_member WHERE uid = ? FOR UPDATE"),
            "after the credential version recheck fails, no business query or write runs"
        );
    }
}

#[tokio::test]
async fn account_writes_recheck_cookie_after_waiting_for_logout_all_member_lock() {
    // Only verifies MySQL's isolation level and row lock waits.
    gono_forum_web::mysql_only!();
    let fixture = TestFixture::migrated().await.unwrap();
    let database = test_database(&fixture).await;
    let outcome = AssertUnwindSafe(locked_writes_contract(&fixture, &database))
        .catch_unwind()
        .await;
    database.pool.close().await;
    fixture.cleanup().await.unwrap();
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

async fn online_failure_contract(fixture: &TestFixture, database: &Database) {
    let (identity, grant) = subject(fixture, database).await;
    let session = identity
        .session(&grant.session_token)
        .await
        .unwrap()
        .unwrap();
    db::query("RENAME TABLE gonof_common_session TO gonof_common_session_unavailable")
        .sqlite("ALTER TABLE gonof_common_session RENAME TO gonof_common_session_unavailable")
        .execute(database.executor())
        .await
        .unwrap();
    let unavailable = Arc::new(RequestProfile::new());
    trace::scope(
        unavailable.clone(),
        identity.touch_online(&session, "127.0.0.1"),
    )
    .await;
    let authentication = Arc::new(RequestProfile::new());
    let recovered = trace::scope(
        authentication.clone(),
        identity.session(&grant.session_token),
    )
    .await;
    // Whatever authentication returns, restore the table first; the outer layer also cleans up the whole isolated
    // database.
    db::query("RENAME TABLE gonof_common_session_unavailable TO gonof_common_session")
        .sqlite("ALTER TABLE gonof_common_session_unavailable RENAME TO gonof_common_session")
        .execute(database.executor())
        .await
        .unwrap();
    let failed = application(&unavailable);
    // An online table write failure does not affect authentication. Last online is written back only
    // when expired sessions are cleaned up (docs/queries.md, "Last online"), and is not written in requests.
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0]
            .sql
            .starts_with("INSERT INTO gonof_common_session")
    );
    assert!(failed[0].error.is_some());
    let recovered = recovered
        .unwrap()
        .expect("an unavailable online table must not revoke a valid cookie");
    assert_eq!(recovered.member.uid, UID);
    let authentication = application(&authentication);
    assert_eq!(authentication.len(), 1);
    assert!(
        authentication[0]
            .sql
            .contains("FROM gonof_common_member WHERE uid = ?")
    );
    assert!(!authentication[0].sql.contains("FOR UPDATE"));
    assert_eq!(authentication[0].rows_returned, 1);

    // The real second sign-in gets a new login_id and independently checks the online update throttling within the
    // same process, without waiting 60 seconds.
    let grant = http_auth::login(&identity, database, UID).await;
    let session = identity
        .session(&grant.session_token)
        .await
        .unwrap()
        .unwrap();
    let updates = Arc::new(RequestProfile::new());
    trace::scope(updates.clone(), async {
        identity.touch_online(&session, "192.0.2.7").await;
        identity.touch_online(&session, "192.0.2.8").await;
        identity.touch_online(&session, "192.0.2.9").await;
    })
    .await;
    let updates = application(&updates);
    assert_eq!(
        updates.len(),
        1,
        "several online updates for one login within a minute run only once: one SQL statement on the online table"
    );
    assert!(
        updates[0]
            .sql
            .starts_with("INSERT INTO gonof_common_session")
    );
    assert!(updates[0].error.is_none());
    let online = db::query("SELECT uid, client_ip FROM gonof_common_session WHERE sid = ?")
        .bind(&session.online_id)
        .fetch_one(database.executor())
        .await
        .unwrap();
    assert_eq!(online.try_get::<i64, _>("uid").unwrap(), UID);
    assert_eq!(
        online.try_get::<String, _>("client_ip").unwrap(),
        "192.0.2.7"
    );
}

#[tokio::test]
async fn online_table_failure_does_not_break_authentication_and_touches_are_throttled() {
    let fixture = TestFixture::migrated().await.unwrap();
    let database = test_database(&fixture).await;
    let outcome = AssertUnwindSafe(online_failure_contract(&fixture, &database))
        .catch_unwind()
        .await;
    database.pool.close().await;
    fixture.cleanup().await.unwrap();
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}
