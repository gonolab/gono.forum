//! Database contract of member API tokens: only digests are stored, a single token is valid, and revocation
//! and expiry take effect immediately.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use gono_forum_web::{
    content,
    db::{self, Database},
    identity::{self, Identity, IdentityError, SessionGrant},
    test_support::TestFixture,
};

const PASSWORD: &str = "Api-key-fixture-password-2026!";

struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
}

fn password_hash() -> String {
    let salt = SaltString::encode_b64(b"api-key-fixture-salt").unwrap();
    Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string()
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        let config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        for (uid, verified) in [(9101_i64, 1_i64), (9102, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(?,?,?,?,?,1)")
                .bind(uid)
                .bind(format!("api-key-{uid}"))
                .bind(password_hash())
                .bind(format!("api-key-{uid}@example.test"))
                .bind(verified)
                .execute(db.executor())
                .await
                .unwrap();
        }
        Self {
            services,
            db,
            identity,
        }
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        let username: String =
            db::query_scalar("SELECT username FROM gonof_common_member WHERE uid=?")
                .bind(uid)
                .fetch_one(self.db.executor())
                .await
                .unwrap();
        self.identity.login(&username, PASSWORD).await.unwrap()
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

    async fn actor(&self, uid: i64) -> identity::Actor {
        let credentials_version = self
            .scalar(&format!(
                "SELECT credentials_version FROM gonof_common_member WHERE uid={uid}"
            ))
            .await;
        identity::Actor {
            uid,
            credentials_version,
            api_key: None,
        }
    }
}

#[tokio::test]
async fn migration_creates_api_key_table_and_bad_rows_are_caught() {
    let f = Fixture::new().await;
    // The migration created the table (it can be queried and is still empty); the same statement on both
    // backends, without querying information_schema.
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_api_key")
            .await,
        0
    );
    assert_eq!(
        f.scalar("SELECT MAX(version) FROM _sqlx_migrations").await,
        gono_forum_web::db::migrations::LATEST
    );
    // The database does not check these values: code writes only through `identity::rules::insert_api_key` and
    // checks before writing; rows written bypassing the entry point are found by the identity consistency check
    // (`tests/consistency/identity.sql`). Foreign keys are still enforced by the database.
    let attempts = [
        "INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at) VALUES(9101,'short','gonof_00',1,2)",
        &format!(
            "INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at) VALUES(9101,'{}','gonof_00',2,2)",
            "a".repeat(64)
        ),
        &format!(
            "INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at,revoked_by) VALUES(9101,'{}','gonof_00',1,2,9101)",
            "b".repeat(64)
        ),
        &format!(
            "INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at) VALUES(424242,'{}','gonof_00',1,2)",
            "c".repeat(64)
        ),
    ];
    for sql in attempts {
        gono_forum_web::test_support::assert_identity_rejected_or_flagged(
            &f.db,
            sql,
            "API tokens with invalid values",
        )
        .await;
    }
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_api_key")
            .await,
        0
    );
    f.services.cleanup().await.unwrap();
}

/// Issuing reads the API switch and the token lifetime again under its locks: a lifetime shortened since the page
/// was shown caps the new token, and an API switched off since refuses it and keeps the current token.
#[tokio::test]
async fn issuing_follows_the_api_settings_saved_meanwhile() {
    let f = Fixture::new().await;
    let grant = f.login(9101).await;
    let set = |key: &'static str, value: &'static str| {
        let db = f.db.clone();
        async move {
            gono_forum_web::test_support::set_setting(&db, key, value)
                .await
                .unwrap()
        }
    };
    set("settings.api.key_ttl_days", "1").await;
    let first = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 365 * 86_400)
        .await
        .unwrap();
    assert_eq!(first.key.expires_at - first.key.dateline, 86_400);
    set("settings.api.enabled", "false").await;
    assert!(matches!(
        f.identity
            .create_api_key(&grant.session_token, PASSWORD, 86_400)
            .await,
        Err(IdentityError::Suspended(_))
    ));
    assert_eq!(
        f.identity.api_key(9101).await.unwrap().unwrap().id,
        first.key.id,
        "the current token is not revoked"
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn keys_store_only_hashes_and_rotate_or_revoke_within_one_transaction() {
    let f = Fixture::new().await;
    let grant = f.login(9101).await;
    let first = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 30 * 86_400)
        .await
        .unwrap();
    assert!(identity::valid_api_secret(&first.secret));
    assert!(first.secret.starts_with(identity::API_KEY_PREFIX));
    assert_eq!(first.key.hint, first.secret[..14]);
    assert_eq!(first.key.expires_at - first.key.dateline, 30 * 86_400);
    let stored: String =
        db::query_scalar("SELECT key_hash FROM gonof_common_member_api_key WHERE id=?")
            .bind(first.key.id)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(stored, identity::token_hash(&first.secret));
    assert_ne!(stored, first.secret);
    assert!(matches!(
        f.identity
            .create_api_key(&grant.session_token, "wrong-password", 86_400)
            .await,
        Err(IdentityError::Validation(_))
    ));
    let (key, member) = f
        .identity
        .authenticate_api_key(&first.secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((key.id, member.uid), (first.key.id, 9101));
    assert!(
        f.identity
            .authenticate_api_key(&stored)
            .await
            .unwrap()
            .is_none(),
        "the stored digest must not work as a token"
    );
    assert_eq!(
        f.identity.api_key(9101).await.unwrap().unwrap().id,
        first.key.id
    );

    let second = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&first.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.identity
            .authenticate_api_key(&second.secret)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9101 AND revoked_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9101")
            .await,
        2
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT revoked_by FROM gonof_common_member_api_key WHERE id={}",
            first.key.id
        ))
        .await,
        9101
    );

    assert!(
        f.identity
            .revoke_api_key(&grant.session_token)
            .await
            .unwrap()
    );
    assert!(
        f.identity
            .authenticate_api_key(&second.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.identity.api_key(9101).await.unwrap().is_none());
    assert!(
        !f.identity
            .revoke_api_key(&grant.session_token)
            .await
            .unwrap()
    );

    let types: Vec<String> = db::query_scalar(
        "SELECT type FROM gonof_common_log WHERE uid=9101 AND type LIKE 'api.key.%' ORDER BY id",
    )
    .fetch_all(f.db.executor())
    .await
    .unwrap();
    assert_eq!(
        types,
        ["api.key.create", "api.key.rotate", "api.key.revoke"]
    );
    let leaked = f
        .scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_log WHERE data LIKE '%{}%' OR data LIKE '%{}%' OR data LIKE '%{}%'",
            first.secret, second.secret, stored
        ))
        .await;
    assert_eq!(
        leaked, 0,
        "the log must not contain the token or its digest"
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn expiry_freeze_and_admin_revocation_stop_authentication() {
    let f = Fixture::new().await;
    let grant = f.login(9101).await;
    let expired = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    let now = identity::now();
    db::query("UPDATE gonof_common_member_api_key SET dateline=?, expires_at=? WHERE id=?")
        .bind(now - 100)
        .bind(now - 50)
        .bind(expired.key.id)
        .execute(f.db.executor())
        .await
        .unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&expired.secret)
            .await
            .unwrap()
            .is_none()
    );
    let shown = f.identity.api_key(9101).await.unwrap().unwrap();
    assert_eq!(shown.id, expired.key.id);
    assert!(
        !shown.active_at(identity::now()),
        "the page must show the key as expired"
    );

    let key = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    f.execute("UPDATE gonof_common_member SET freeze=1 WHERE uid=9101")
        .await;
    let (_, member) = f
        .identity
        .authenticate_api_key(&key.secret)
        .await
        .unwrap()
        .unwrap();
    assert!(
        member.frozen,
        "the frozen state comes back with the member; the caller rejects it"
    );
    f.execute("UPDATE gonof_common_member SET freeze=0 WHERE uid=9101")
        .await;

    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(9100,'api-key-admin',?,'api-key-admin@example.test',1,1,1)")
        .bind(password_hash())
        .execute(f.db.executor())
        .await
        .unwrap();
    let admin = f.actor(9100).await;
    let member = f.actor(9101).await;
    assert!(matches!(
        f.identity
            .revoke_member_api_key(member, 9101, "not allowed")
            .await,
        Err(IdentityError::Forbidden)
    ));
    assert!(matches!(
        f.identity
            .revoke_member_api_key(admin, 424242, "missing")
            .await,
        Err(IdentityError::NotFound)
    ));
    assert!(
        f.identity
            .revoke_member_api_key(admin, 9101, "test revocation")
            .await
            .unwrap()
    );
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT revoked_by FROM gonof_common_member_api_key WHERE id={}",
            key.key.id
        ))
        .await,
        9100
    );
    assert_eq!(
        f.scalar("SELECT operationuid FROM gonof_common_log WHERE type='api.key.revoke' AND uid=9101 ORDER BY id DESC LIMIT 1").await,
        9100
    );
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn unverified_members_cannot_enable_and_usage_is_throttled() {
    let f = Fixture::new().await;
    let unverified = f.login(9102).await;
    assert!(matches!(
        f.identity
            .create_api_key(&unverified.session_token, PASSWORD, 86_400)
            .await,
        Err(IdentityError::Forbidden)
    ));

    let grant = f.login(9101).await;
    let key = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    let now = identity::now();
    f.identity
        .touch_api_key(key.key.id, "203.0.113.5", now)
        .await
        .unwrap();
    let shown = f.identity.api_key(9101).await.unwrap().unwrap();
    assert_eq!(
        (shown.last_used_at, shown.last_used_ip.as_str()),
        (now, "203.0.113.5")
    );
    f.identity
        .touch_api_key(key.key.id, "203.0.113.6", now + 30)
        .await
        .unwrap();
    let shown = f.identity.api_key(9101).await.unwrap().unwrap();
    assert_eq!(
        shown.last_used_at, now,
        "use within a minute is not written again"
    );
    f.identity
        .touch_api_key(key.key.id, "203.0.113.6", now + 61)
        .await
        .unwrap();
    let shown = f.identity.api_key(9101).await.unwrap().unwrap();
    assert_eq!(
        (shown.last_used_at, shown.last_used_ip.as_str()),
        (now + 61, "203.0.113.6")
    );

    // Signing out of all devices is treated as a suspected theft, and the token is revoked with it (changing the
    // password while signed in does not revoke it, see the next test).
    f.identity.logout_all(&grant.session_token).await.unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_none()
    );
    f.services.cleanup().await.unwrap();
}

// Resetting a forgotten password and signing out of all devices revoke tokens; changing the password while
// signed in does not.
#[tokio::test]
async fn reset_and_logout_all_revoke_keys_but_password_change_keeps_them() {
    let f = Fixture::new().await;
    let grant = f.login(9101).await;
    let key = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    let changed = "Api-key-changed-password-2026!";
    f.identity
        .change_password(&grant.session_token, PASSWORD, changed)
        .await
        .unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_some()
    );

    let username = "api-key-9101";
    let grant = f.identity.login(username, changed).await.unwrap();
    f.identity.logout_all(&grant.session_token).await.unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT revoked_by FROM gonof_common_member_api_key WHERE id={}",
            key.key.id
        ))
        .await,
        9101
    );

    let grant = f.identity.login(username, changed).await.unwrap();
    let key = f
        .identity
        .create_api_key(&grant.session_token, changed, 86_400)
        .await
        .unwrap();
    f.identity
        .request_password_reset("api-key-9101@example.test")
        .await
        .unwrap();
    let payload: String = db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE event_type='password_reset' AND aggregate_id=9101 ORDER BY eventid DESC LIMIT 1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    f.identity
        .reset_password(payload["token"].as_str().unwrap(), PASSWORD)
        .await
        .unwrap();
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE uid=9101 AND type='api.key.revoke'")
            .await,
        2
    );
    f.services.cleanup().await.unwrap();
}

/// When an administrator revokes the token of a member with a smaller uid, locks are still taken
/// in ascending uid order.
///
/// A separate transaction locks the target member first in ascending order, and locks the administrator only
/// after the revocation statement has been sent: locking the administrator first would make the two sides wait
/// for each other in a deadlock.
#[tokio::test]
async fn admin_revocation_locks_members_in_uid_order() {
    // Only verifies MySQL's row-lock order and lock wait timeout.
    gono_forum_web::mysql_only!();
    use gono_forum_web::debug::trace::{self, QuerySource, RequestProfile};
    use std::{sync::Arc, time::Duration};
    let f = Fixture::new().await;
    let grant = f.login(9101).await;
    let key = f
        .identity
        .create_api_key(&grant.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(9200,'api-key-late-admin',?,'api-key-late-admin@example.test',1,1,1)")
        .bind(password_hash())
        .execute(f.db.executor())
        .await
        .unwrap();
    let admin = f.actor(9200).await;
    let mut other = f.db.begin_write().await.unwrap();
    db::query("SET SESSION innodb_lock_wait_timeout=1")
        .execute(&mut *other)
        .await
        .unwrap();
    db::query("SELECT uid FROM gonof_common_member WHERE uid=9101 FOR UPDATE")
        .fetch_one(&mut *other)
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let revoking = {
        let identity = f.identity.clone();
        let profile = profile.clone();
        tokio::spawn(async move {
            trace::scope(
                profile,
                Box::pin(identity.revoke_member_api_key(admin, 9101, "concurrent revocation")),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !profile.snapshot().iter().any(|record| {
            record.source == QuerySource::Application
                && record.sql.contains("FROM gonof_common_member WHERE uid")
                && record.sql.ends_with("FOR UPDATE")
                && record.rows_returned == 0
                && record.elapsed.is_zero()
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the revocation must wait on the member row lock");
    let locked = db::query("SELECT uid FROM gonof_common_member WHERE uid=9200 FOR UPDATE")
        .fetch_optional(&mut *other)
        .await;
    assert!(
        matches!(locked, Ok(Some(_))),
        "a transaction locking in ascending order must get the admin row: {locked:?}"
    );
    other.commit().await.unwrap();
    assert!(
        revoking.await.unwrap().unwrap(),
        "the revocation completes once the lock is released"
    );
    assert!(
        f.identity
            .authenticate_api_key(&key.secret)
            .await
            .unwrap()
            .is_none()
    );
    f.services.cleanup().await.unwrap();
}
