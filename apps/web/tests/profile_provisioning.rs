#[path = "support/http_auth.rs"]
mod http_auth;

use gono_forum_web::{content::profiles, db, identity, manage, test_support::TestFixture};

#[tokio::test]
async fn bulk_api_key_provisioning_serializes_with_member_key_rotation() {
    // Only verifies the MySQL row-lock wait choreography.
    gono_forum_web::mysql_only!();
    use gono_forum_web::{
        content::api_keys,
        debug::trace::{self, RequestProfile},
    };
    use std::{sync::Arc, time::Duration};

    let fixture = TestFixture::migrated().await.unwrap();
    let database = &fixture.db;
    db::query("INSERT INTO gonof_common_member(uid,username) VALUES(9201,'cli-key')")
        .execute(database.executor())
        .await
        .unwrap();
    // Web rotation revokes the old token and inserts the new one under the member row lock. Pause before its insert.
    let mut rotation = database.begin_write().await.unwrap();
    db::query("SELECT uid FROM gonof_common_member WHERE uid=9201 FOR UPDATE")
        .fetch_one(database.observe(&mut *rotation))
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    let provisioning = trace::scope(
        profile.clone(),
        api_keys::provision_api_keys(database, &[9201], 3600, |_| Ok(())),
    );
    let finish_rotation = async {
        // First confirm that the batch provisioning's member row-lock statement has been issued and is waiting.
        tokio::time::timeout(Duration::from_secs(10), async {
            while !profile.snapshot().iter().any(|record| {
                record.sql == "SELECT uid FROM gonof_common_member WHERE uid = ? FOR UPDATE"
                    && record.rows_returned == 0
                    && record.elapsed.is_zero()
            }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bulk provisioning must stop at the member row lock");
        // Then wait for the wall clock to pass this second: any time taken before waiting for the lock is earlier than
        // the token time written below. This reproduces "crossing a whole second while waiting for the lock"; the
        // order is decided by the observed statements and the clock, not by sleep durations.
        let waiting_since = identity::now();
        tokio::time::timeout(Duration::from_secs(3), async {
            while identity::now() <= waiting_since {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the wall clock should move on");
        let timestamp = identity::now();
        db::query("INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at) VALUES(9201,?,'rotation',?,?)")
            .bind(identity::token_hash("synthetic-rotation-key")).bind(timestamp).bind(timestamp + 3600)
            .execute(database.observe(&mut *rotation)).await.unwrap();
        rotation.commit().await.unwrap();
    };
    let (created, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(provisioning, finish_rotation)
    })
    .await
    .unwrap();
    assert_eq!(created.expect("bulk provisioning should succeed").len(), 1);
    // Revocation time is not earlier than issue time. The database does not check this: provisioning takes the time
    // after acquiring the row lock, and the revoke statement also takes GREATEST(dateline, ?) as a fallback; check it
    // directly here.
    let early: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9201 AND revoked_at < dateline",
    )
    .fetch_one(database.executor())
    .await
    .unwrap();
    assert_eq!(
        early, 0,
        "revoking the token the other side just wrote cannot predate its creation"
    );
    let active: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9201 AND revoked_at IS NULL",
    )
    .fetch_one(database.executor())
    .await
    .unwrap();
    assert_eq!(
        active, 1,
        "bulk provisioning racing a web rotation must not leave two valid tokens"
    );
    fixture.cleanup().await.unwrap();
}

#[tokio::test]
async fn setting_a_password_from_the_cli_revokes_prior_login_credentials() {
    let fixture = TestFixture::migrated().await.unwrap();
    let database = &fixture.db;
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus) VALUES(9201,'cli-password','synthetic','cli-password@example.test',1)")
        .execute(database.executor()).await.unwrap();
    let identity = identity::Identity::new(
        database.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let login = http_auth::login(&identity, database, 9201).await;
    let timestamp = identity::now();
    for (i, purpose) in [
        "password_reset",
        "email_change",
        "appeal_access",
        "privacy_access",
    ]
    .into_iter()
    .enumerate()
    {
        db::query("INSERT INTO gonof_common_member_token(uid,purpose,token_hash,email,dateline,expires_at) VALUES(9201,? ,?,'cli-password@example.test',?,?)")
            .bind(purpose).bind(format!("{i:064x}")).bind(timestamp).bind(timestamp + 600)
            .execute(database.executor()).await.unwrap();
    }
    assert!(
        identity
            .session(&login.session_token)
            .await
            .unwrap()
            .is_some()
    );
    profiles::set_password(database, 9201, "New-cli-password-2026!")
        .await
        .unwrap();
    assert!(
        identity
            .session(&login.session_token)
            .await
            .unwrap()
            .is_none(),
        "a password set from the command line invalidates the old cookie"
    );
    let active: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_member_token WHERE uid=9201 AND revoked_at IS NULL",
    )
    .fetch_one(database.executor())
    .await
    .unwrap();
    assert_eq!(
        active, 0,
        "the old password reset and private data access links are revoked too"
    );
    // Leaves a trace like a web password change: the actor is not the account owner, so operationuid is empty.
    let audited = db::query(
        "SELECT operationuid,data,source FROM gonof_common_log
         WHERE uid=9201 AND type='identity.password.set'",
    )
    .fetch_one(database.executor())
    .await
    .unwrap();
    assert_eq!(
        audited.try_get::<Option<i64>, _>("operationuid").unwrap(),
        None
    );
    assert_eq!(db::text(&audited, "source").unwrap(), "cli");
    let data: serde_json::Value =
        serde_json::from_str(&db::text(&audited, "data").unwrap()).unwrap();
    assert_eq!(data["credentials_version"], 2);
    assert_eq!(data["revoked_tokens"], 4);
    assert_eq!(data["target_uid"], 9201);
    for (uid, role) in [(9202, 1), (9203, 2), (9204, 3)] {
        db::query("INSERT INTO gonof_common_member(uid,username,email,password,adminid) VALUES(?,?,?,'synthetic',?)")
            .bind(uid).bind(format!("audit-{uid}")).bind(format!("audit-{uid}@example.test")).bind(role)
            .execute(database.executor()).await.unwrap();
    }
    let filter = manage::LogFilter {
        kind: "identity".into(),
        target: "9201".into(),
        ..Default::default()
    };
    let (logs, total) = manage::logs_filtered(
        database,
        identity::Actor {
            uid: 9202,
            credentials_version: 1,
            api_key: None,
        },
        &filter,
    )
    .await
    .unwrap();
    // Administrators see every event, so the member's sign-in before the change is listed as well.
    assert_eq!(
        logs.iter()
            .map(|log| log.operation.as_str())
            .collect::<Vec<_>>(),
        ["identity.password.set", "identity.login"],
        "an administrator finds the CLI password record by target member"
    );
    assert_eq!(total, 2);
    assert_eq!(logs[0].label(), "Password set from the command line");
    assert_eq!(logs[0].target, "UID 9201");
    assert_eq!(logs[0].operator, "System");
    // A member's self-service password change only binds itself to the uid column, with no target_uid in data; the
    // target member filter must still find it.
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'identity.password.change',?,'web',?)")
        .bind(9201).bind(9201).bind(r#"{"credentials_version":3}"#).bind(identity::now())
        .execute(database.executor()).await.unwrap();
    let (logs, total) = manage::logs_filtered(
        database,
        identity::Actor {
            uid: 9202,
            credentials_version: 1,
            api_key: None,
        },
        &filter,
    )
    .await
    .unwrap();
    assert_eq!(
        total, 3,
        "the target member filter covers account security records the member triggered"
    );
    assert_eq!(logs[0].operation, "identity.password.change");
    for uid in [9203, 9204] {
        let (logs, total) = manage::logs_filtered(
            database,
            identity::Actor {
                uid,
                credentials_version: 1,
                api_key: None,
            },
            &filter,
        )
        .await
        .unwrap();
        assert_eq!(
            total, 0,
            "global and forum moderators cannot see the account security log"
        );
        assert!(logs.is_empty());
    }
    assert!(
        identity
            .login("cli-password", "Http-fixture-password-2026!")
            .await
            .is_err()
    );
    assert!(
        identity
            .login("cli-password", "New-cli-password-2026!")
            .await
            .is_ok()
    );
    fixture.cleanup().await.unwrap();
}

/// Profile import turns off foreign key checks before emptying the member table. If it fails midway,
/// this session variable must not go back to the pool with the connection; when the confirmation argument does
/// not match the current database name, nothing is touched.
#[tokio::test]
async fn profile_import_requires_the_database_name_and_never_leaks_disabled_foreign_keys() {
    let fixture = TestFixture::migrated().await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username) VALUES(9301,'before-import')")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let manifest = fixture.root.join("profiles.json");
    std::fs::write(
        &manifest,
        r#"{"roles":[{"uid":1,"username":"imported-admin","bio":""},{"uid":2,"username":"imported-member","bio":""}]}"#,
    )
    .unwrap();
    // A pool with a single connection: if the connection used by the import goes back to the pool, the next query
    // gets it.
    let mut config = fixture.config.database.clone();
    config.max_connections = 1;
    let single = db::Database::connect(&config).await.unwrap();
    // Database name: on MySQL it is the connected database, on SQLite the database file name without the extension
    // (the same check as the profile import).
    let name: String = single.pool.database_name().unwrap();
    let members = || async {
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_common_member WHERE uid=9301")
            .fetch_one(fixture.db.executor())
            .await
            .unwrap()
    };

    for wrong in ["", "gonof", "another_database"] {
        let error = profiles::import_member_profiles(&single, &manifest, wrong)
            .await
            .err()
            .expect("a wrong database name is refused");
        assert!(error.to_string().contains("--confirm-reset="), "{error}");
        assert_eq!(
            members().await,
            1,
            "a wrong database name must not empty the member table"
        );
    }

    // The emptying step fails: the transaction rolls back, the member table is unchanged, and foreign key checks on
    // the pooled connection stay on.
    // CREATE TRIGGER cannot use the prepared-statement protocol, so the test DDL uses raw execution.
    gono_forum_web::test_support::fail_when(
        &fixture.db,
        "gonof_test_member_delete_fails",
        "BEFORE DELETE",
        "gonof_common_member",
        "",
        "delete refused",
    )
    .await
    .unwrap();
    assert!(
        profiles::import_member_profiles(&single, &manifest, &name)
            .await
            .is_err()
    );
    let checks: i64 = db::query_scalar("SELECT @@SESSION.foreign_key_checks")
        .sqlite("PRAGMA foreign_keys")
        .fetch_one(single.executor())
        .await
        .unwrap();
    assert_eq!(
        checks, 1,
        "a connection with foreign key checks off must not go back to the pool"
    );
    assert_eq!(members().await, 1);
    gono_forum_web::test_support::drop_trigger(&fixture.db, "gonof_test_member_delete_fails")
        .await
        .unwrap();

    // With a matching database name the import proceeds as usual, and foreign key checks stay on afterwards.
    let summary = profiles::import_member_profiles(&single, &manifest, &name)
        .await
        .unwrap();
    assert_eq!(summary.inserted, 2);
    assert_eq!(members().await, 0);
    let checks: i64 = db::query_scalar("SELECT @@SESSION.foreign_key_checks")
        .sqlite("PRAGMA foreign_keys")
        .fetch_one(single.executor())
        .await
        .unwrap();
    assert_eq!(checks, 1);
    single.pool.close().await;
    fixture.cleanup().await.unwrap();
}

/// When the same uid appears twice in the list the whole batch is refused, so it never returns a token it has just
/// revoked itself.
#[tokio::test]
async fn bulk_api_key_provisioning_rejects_duplicate_uids() {
    let fixture = TestFixture::migrated().await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username) VALUES(9211,'cli-dup')")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let error = gono_forum_web::content::api_keys::provision_api_keys(
        &fixture.db,
        &[9211, 9211],
        3600,
        |_| Ok(()),
    )
    .await
    .err()
    .expect("a duplicate uid is refused");
    assert!(error.to_string().contains("duplicates"), "{error}");
    let keys: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9211")
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
    assert_eq!(keys, 0, "a refused batch writes no token at all");
    fixture.cleanup().await.unwrap();
}

/// The plaintext tokens are saved before the batch commits: when saving them fails (a full disk, say), the old token
/// keeps working and no new token exists, instead of every client losing its token to a replacement nobody has.
#[tokio::test]
async fn bulk_api_keys_are_saved_before_the_old_ones_are_revoked() {
    use gono_forum_web::content::api_keys;
    let fixture = TestFixture::migrated().await.unwrap();
    let database = &fixture.db;
    db::query("INSERT INTO gonof_common_member(uid,username) VALUES(9221,'cli-rotate')")
        .execute(database.executor())
        .await
        .unwrap();
    let timestamp = identity::now();
    db::query("INSERT INTO gonof_common_member_api_key(uid,key_hash,key_hint,dateline,expires_at) VALUES(9221,?,'before',?,?)")
        .bind(identity::token_hash("the-token-clients-use"))
        .bind(timestamp)
        .bind(timestamp + 3600)
        .execute(database.executor())
        .await
        .unwrap();
    let active = || async {
        db::query_scalar::<String>(
            "SELECT key_hash FROM gonof_common_member_api_key WHERE uid=9221 AND revoked_at IS NULL",
        )
        .fetch_all(database.executor())
        .await
        .unwrap()
    };

    let error = api_keys::provision_api_keys(database, &[9221], 3600, |keys| {
        assert_eq!(keys.len(), 1);
        anyhow::bail!("No space left on device")
    })
    .await
    .err()
    .expect("a failed save fails the batch");
    assert!(
        error.to_string().contains("nothing was revoked"),
        "{error:#}"
    );
    assert_eq!(
        active().await,
        [identity::token_hash("the-token-clients-use")],
        "the old token keeps working when the new ones could not be saved"
    );
    let rows: i64 =
        db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9221")
            .fetch_one(database.executor())
            .await
            .unwrap();
    assert_eq!(rows, 1, "no token was issued");

    // When saving works, the saved token is the one that is valid.
    let mut saved = Vec::new();
    let keys = api_keys::provision_api_keys(database, &[9221], 3600, |keys| {
        saved = keys.iter().map(|key| key.secret.clone()).collect();
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(saved, [keys[0].secret.clone()]);
    assert_eq!(active().await, [identity::token_hash(&saved[0])]);
    fixture.cleanup().await.unwrap();
}

async fn member_count(fixture: &TestFixture) -> i64 {
    db::query_scalar("SELECT COUNT(*) FROM gonof_common_member")
        .fetch_one(fixture.db.executor())
        .await
        .unwrap()
}

/// uid=1 is the only account the profile import makes an administrator, and the finished setup keeps the
/// administrator wizard closed: a list without uid=1 is refused before the member table is emptied.
#[tokio::test]
async fn profile_import_refuses_a_list_without_the_administrator() {
    let fixture = TestFixture::migrated().await.unwrap();
    db::query("INSERT INTO gonof_common_member(uid,username,email,emailstatus,password,adminid) VALUES(1,'site-owner','owner@example.test',1,'synthetic',1)")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let manifest = fixture.root.join("profiles.json");
    std::fs::write(
        &manifest,
        r#"{"roles":[{"uid":2,"username":"member-two","bio":""},{"uid":3,"username":"member-three","bio":""}]}"#,
    )
    .unwrap();
    let name = fixture.db.pool.database_name().unwrap();
    let error = profiles::import_member_profiles(&fixture.db, &manifest, &name)
        .await
        .err()
        .expect("a list without uid=1 is refused");
    assert!(error.to_string().contains("uid=1"), "{error}");
    let admins: Vec<String> =
        db::query_scalar("SELECT username FROM gonof_common_member WHERE adminid=1")
            .fetch_all(fixture.db.executor())
            .await
            .unwrap();
    assert_eq!(admins, ["site-owner"], "the administrator is still there");
    assert_eq!(member_count(&fixture).await, 1);
    fixture.cleanup().await.unwrap();
}

/// Imported usernames are stored the way sign-in looks them up (lowercase after NFKC), and names that differ only
/// in case are the same name: the administrator still signs in with the name as the list writes it.
#[tokio::test]
async fn imported_usernames_are_stored_the_way_sign_in_looks_them_up() {
    const PASSWORD: &str = "Profile-owner-password-1";
    let fixture = TestFixture::migrated().await.unwrap();
    let identity = identity::Identity::new(
        fixture.db.clone(),
        fixture.config.identity.clone(),
        fixture.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let owner = identity
        .setup_admin("Nora", Some("nora@example.test"), PASSWORD)
        .await
        .unwrap();
    assert_eq!(owner, 1);
    let name = fixture.db.pool.database_name().unwrap();
    let manifest = fixture.root.join("profiles.json");

    std::fs::write(
        &manifest,
        r#"{"roles":[{"uid":1,"username":"Nora","bio":""},{"uid":2,"username":"nora","bio":""}]}"#,
    )
    .unwrap();
    let error = profiles::import_member_profiles(&fixture.db, &manifest, &name)
        .await
        .err()
        .expect("names that differ only in case are duplicates");
    assert!(error.to_string().contains("duplicate username"), "{error}");
    assert_eq!(member_count(&fixture).await, 1);

    std::fs::write(
        &manifest,
        r#"{"roles":[{"uid":1,"username":"Nora","bio":""},{"uid":2,"username":"Ｆｅｌｉｘ","bio":""}]}"#,
    )
    .unwrap();
    profiles::import_member_profiles(&fixture.db, &manifest, &name)
        .await
        .unwrap();
    let names: Vec<String> =
        db::query_scalar("SELECT username FROM gonof_common_member ORDER BY uid")
            .fetch_all(fixture.db.executor())
            .await
            .unwrap();
    assert_eq!(names, ["nora", "felix"]);
    assert!(
        identity.login("Nora", PASSWORD).await.is_ok(),
        "the administrator signs in by username after the import"
    );
    fixture.cleanup().await.unwrap();
}

/// enable-interactions patches only synthetic authors: a stale uid that now belongs to a member who signs in by phone,
/// or to a member with another name, keeps its password and gets no placeholder e-mail.
#[tokio::test]
async fn enabling_interactions_leaves_accounts_that_are_not_synthetic_authors_alone() {
    let fixture = TestFixture::migrated().await.unwrap();
    for sql in [
        "INSERT INTO gonof_common_member(uid,username,mobile,password) VALUES(9401,'phone-member','+8613800000001','synthetic-phone-hash')",
        "INSERT INTO gonof_common_member(uid,username) VALUES(9402,'renamed-since')",
        "INSERT INTO gonof_common_member(uid,username) VALUES(9403,'synthetic')",
    ] {
        db::query(sql).execute(fixture.db.executor()).await.unwrap();
    }
    let list = fixture.root.join("members.json");
    std::fs::write(
        &list,
        r#"{"roles":[{"uid":9401,"username":"phone-member","bio":""},{"uid":9402,"username":"listed-name","bio":""},{"uid":9403,"username":"Synthetic","bio":""}]}"#,
    )
    .unwrap();
    let updated = profiles::enable_interactions(&fixture.db, &list)
        .await
        .unwrap();
    assert_eq!(updated, 1);
    let rows: Vec<(i64, Option<String>, i64, Option<String>)> =
        db::query("SELECT uid,email,emailstatus,password FROM gonof_common_member ORDER BY uid")
            .fetch_all(fixture.db.executor())
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.try_get("uid").unwrap(),
                    row.try_get("email").unwrap(),
                    row.try_get("emailstatus").unwrap(),
                    row.try_get("password").unwrap(),
                )
            })
            .collect();
    assert_eq!(
        rows[0],
        (9401, None, 0, Some("synthetic-phone-hash".to_owned())),
        "the phone member keeps its password"
    );
    assert_eq!(rows[1], (9402, None, 0, None), "another name is left alone");
    assert_eq!(rows[2].1.as_deref(), Some("member9403@gonof.test"));
    assert_eq!(rows[2].2, 1);
    assert!(rows[2].3.is_some());
    fixture.cleanup().await.unwrap();
}

/// set-password is a maintenance command for any site (docs/install.md): it follows the site's minimum password
/// length, like the web pages, in both directions.
#[tokio::test]
async fn the_cli_password_follows_the_site_minimum_length() {
    let fixture = TestFixture::migrated().await.unwrap();
    let database = &fixture.db;
    db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus) VALUES(9501,'cli-length','synthetic','cli-length@example.test',1)")
        .execute(database.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::set_setting(
        database,
        "settings.account.password_min_length",
        "20",
    )
    .await
    .unwrap();
    let error = profiles::set_password(database, 9501, "Twelve-chars")
        .await
        .expect_err("a password shorter than the site minimum is refused");
    assert!(error.to_string().contains("20-128"), "{error}");
    profiles::set_password(database, 9501, "Twenty-characters-ok")
        .await
        .unwrap();
    gono_forum_web::test_support::set_setting(
        database,
        "settings.account.password_min_length",
        "8",
    )
    .await
    .unwrap();
    profiles::set_password(database, 9501, "Eight-ch")
        .await
        .unwrap();
    let error = profiles::set_password(database, 9501, &"x".repeat(129))
        .await
        .expect_err("the upper limit still applies");
    assert!(error.to_string().contains("8-128"), "{error}");
    fixture.cleanup().await.unwrap();
}
