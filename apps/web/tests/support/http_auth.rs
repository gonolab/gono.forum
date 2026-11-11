//! Integration tests only: sets a real password for an existing synthetic member, then obtains the cookie through
//! the regular login flow.
use std::sync::OnceLock;

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use gono_forum_web::{
    db::{self, Database},
    identity::{Identity, SessionGrant},
};

pub async fn login(identity: &Identity, database: &Database, uid: i64) -> SessionGrant {
    const PASSWORD: &str = "Http-fixture-password-2026!";
    static HASH: OnceLock<String> = OnceLock::new();
    let hash = HASH.get_or_init(|| {
        let salt = SaltString::encode_b64(b"http-fixture-salt").unwrap();
        Argon2::default()
            .hash_password(PASSWORD.as_bytes(), &salt)
            .unwrap()
            .to_string()
    });
    db::query("UPDATE gonof_common_member SET password=? WHERE uid=?")
        .bind(hash)
        .bind(uid)
        .execute(database.executor())
        .await
        .unwrap();
    let username: String = db::query_scalar("SELECT username FROM gonof_common_member WHERE uid=?")
        .bind(uid)
        .fetch_one(database.executor())
        .await
        .unwrap();
    identity.login(&username, PASSWORD).await.unwrap()
}
