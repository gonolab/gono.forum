//! Tokens for personal feed URLs (`gonof_home_feed_token`).
//!
//! The raw token is 32 random bytes as 64 lowercase hex digits, handed to the page only once, when generated; the
//! database stores only the SHA-256 digest and the first 8 characters of the raw token as a hint. At most one per
//! member: regenerating replaces the digest in place and the old URL stops working immediately; disabling deletes the
//! row. Feed requests look up the owner by digest; frozen owners and owners whose account is being deleted or has been
//! deleted are all treated as nonexistent.
use crate::db::Tx;
use anyhow::Context;

use crate::{
    content::writes::ContentWriteError,
    db::{self, Database},
    identity::{self, Actor},
};

type Result<T> = std::result::Result<T, ContentWriteError>;

/// Length of the hint: the first 8 characters of the raw token, leaking 32 bits and leaving 224.
const HINT_CHARS: usize = 8;

/// Status shown on the settings page, without the digest.
pub struct FeedToken {
    pub hint: String,
    pub include_members: bool,
    pub dateline: i64,
}

/// Owner resolved from a feed request.
pub struct FeedOwner {
    pub uid: i64,
    pub include_members: bool,
}

/// Raw token format: 64 lowercase hex digits. Other formats are not looked up in the database.
pub fn valid_secret(secret: &str) -> bool {
    secret.len() == 64
        && secret
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub async fn get(db: &Database, uid: i64) -> anyhow::Result<Option<FeedToken>> {
    let row = crate::db::query(
        "SELECT token_hint,include_members,dateline FROM gonof_home_feed_token WHERE uid=?",
    )
    .bind(uid)
    .fetch_optional(db.executor())
    .await?;
    row.map(|row| {
        Ok(FeedToken {
            hint: db::text(&row, "token_hint")?,
            include_members: row.try_get::<i64, _>("include_members")? == 1,
            dateline: row.try_get("dateline")?,
        })
    })
    .transpose()
}

async fn log(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    kind: &str,
    data: serde_json::Value,
    timestamp: i64,
) -> anyhow::Result<()> {
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
        .bind(uid)
        .bind(uid)
        .bind(kind)
        .bind(data.to_string())
        .bind(timestamp)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Enables or regenerates: returns the new raw token, this one time only. "Include members-only threads" goes back
/// to off.
pub async fn issue(db: &Database, actor: Actor) -> Result<String> {
    let mut tx = db.begin_write().await?;
    crate::interaction::lock_actor(db, &mut tx, actor, false).await?;
    let secret = identity::random_token();
    let hash = identity::token_hash(&secret);
    let hint: String = secret.chars().take(HINT_CHARS).collect();
    let timestamp = identity::now();
    // The member row lock serializes generating and disabling for the same member, so this read sees the results
    // committed before it.
    let existing: Option<i64> =
        crate::db::query_scalar("SELECT uid FROM gonof_home_feed_token WHERE uid=?")
            .bind(actor.uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    if existing.is_some() {
        crate::db::query("UPDATE gonof_home_feed_token SET token_hash=?,token_hint=?,include_members=0,dateline=? WHERE uid=?")
            .bind(&hash)
            .bind(&hint)
            .bind(timestamp)
            .bind(actor.uid)
            .execute(db.observe(&mut *tx))
            .await?;
    } else {
        crate::db::query("INSERT INTO gonof_home_feed_token(uid,token_hash,token_hint,include_members,dateline) VALUES(?,?,?,0,?)")
            .bind(actor.uid)
            .bind(&hash)
            .bind(&hint)
            .bind(timestamp)
            .execute(db.observe(&mut *tx))
            .await?;
    }
    let kind = if existing.is_some() {
        "feed.token.rotate"
    } else {
        "feed.token.create"
    };
    log(
        db,
        &mut tx,
        actor.uid,
        kind,
        serde_json::json!({}),
        timestamp,
    )
    .await?;
    tx.commit().await?;
    Ok(secret)
}

/// Turns "Include members-only threads" on or off. Returns `NotFound` when no feed URL is enabled.
pub async fn set_include_members(db: &Database, actor: Actor, include: bool) -> Result<()> {
    let mut tx = db.begin_write().await?;
    crate::interaction::lock_actor(db, &mut tx, actor, false).await?;
    let current: Option<i64> =
        crate::db::query_scalar("SELECT include_members FROM gonof_home_feed_token WHERE uid=?")
            .bind(actor.uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    let Some(current) = current else {
        return Err(ContentWriteError::NotFound);
    };
    if (current == 1) != include {
        let timestamp = identity::now();
        crate::db::query("UPDATE gonof_home_feed_token SET include_members=? WHERE uid=?")
            .bind(i64::from(include))
            .bind(actor.uid)
            .execute(db.observe(&mut *tx))
            .await?;
        log(
            db,
            &mut tx,
            actor.uid,
            "feed.token.members",
            serde_json::json!({"include_members": include}),
            timestamp,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Disables: deletes the row, and the old URL returns 404 immediately. Returns whether a row was actually deleted.
pub async fn disable(db: &Database, actor: Actor) -> Result<bool> {
    let mut tx = db.begin_write().await?;
    crate::interaction::lock_actor(db, &mut tx, actor, false).await?;
    let removed = delete_locked(db, &mut tx, actor.uid).await?;
    if removed {
        log(
            db,
            &mut tx,
            actor.uid,
            "feed.token.disable",
            serde_json::json!({}),
            identity::now(),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(removed)
}

/// Deletes inside a transaction in which the caller already holds the member row lock. Shared by password reset,
/// sign-out from all devices and account deletion cleanup.
pub async fn delete_locked(db: &Database, tx: &mut Tx<'_>, uid: i64) -> anyhow::Result<bool> {
    Ok(
        crate::db::query("DELETE FROM gonof_home_feed_token WHERE uid=?")
            .bind(uid)
            .execute(db.observe(&mut **tx))
            .await
            .context("failed to delete the personal feed token")?
            .rows_affected()
            > 0,
    )
}

/// Feed request: looks up the owner by digest; freeze and account status are checked in code. Does not write to the
/// database.
pub async fn resolve(db: &Database, secret: &str) -> anyhow::Result<Option<FeedOwner>> {
    if !valid_secret(secret) {
        return Ok(None);
    }
    let row = crate::db::query(
        "SELECT k.uid,k.include_members,m.`freeze`,m.account_state FROM gonof_home_feed_token k
         JOIN gonof_common_member m ON m.uid=k.uid WHERE k.token_hash=?",
    )
    .bind(identity::token_hash(secret))
    .fetch_optional(db.executor())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.try_get::<i64, _>("freeze")? != 0 || db::text(&row, "account_state")? != "active" {
        return Ok(None);
    }
    Ok(Some(FeedOwner {
        uid: row.try_get("uid")?,
        include_members: row.try_get::<i64, _>("include_members")? == 1,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lowercase_hex_of_the_right_length_is_looked_up() {
        assert!(valid_secret(&"a1".repeat(32)));
        assert!(!valid_secret(&"A1".repeat(32)));
        assert!(!valid_secret(&"a1".repeat(31)));
        assert!(!valid_secret(&format!("{}g", "a".repeat(63))));
        assert!(!valid_secret(""));
    }
}
