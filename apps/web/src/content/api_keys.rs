//! Issues API tokens to members in bulk; writes the database directly, bypassing the self-service flow with its
//! session/CSRF/password re-check (that flow is for the actual person; this one provisions test accounts in bulk). The
//! plaintext token appears only once, at generation, and the caller must store `ProvisionedKey::secret` somewhere safe
//! itself; the database only ever keeps the SHA-256 digest.
use anyhow::{Context, Result};

use crate::db::Database;

pub struct ProvisionedKey {
    pub uid: i64,
    pub secret: String,
    pub hint: String,
    pub expires_at: i64,
}

/// Tokens in the same format as self-service issuance: `gonof_` prefix + 64 random hex digits, digested with SHA-256.
/// For each uid, its existing unrevoked tokens are revoked first (an account has only one valid token at a time), then
/// the new one is written. A duplicate uid in the list rejects the whole batch: otherwise the later entry would revoke
/// the token the earlier one just wrote, leaving an already invalid token in the result.
pub async fn provision_api_keys(
    db: &Database,
    uids: &[i64],
    ttl_seconds: i64,
) -> Result<Vec<ProvisionedKey>> {
    // Uses the same member row locks as web generation and rotation, to avoid concurrent inserts after each side
    // revokes, or a deadlock from locking tokens first and then waiting on the member foreign key check, the reverse of
    // the web's lock order.
    let mut lock_uids = uids.to_vec();
    lock_uids.sort_unstable();
    lock_uids.dedup();
    anyhow::ensure!(
        lock_uids.len() == uids.len(),
        crate::i18n::current().settings_error_duplicate_uids()
    );
    let mut transaction = db.begin_write().await?;
    for uid in lock_uids {
        crate::db::query_scalar::<i64>(
            "SELECT uid FROM gonof_common_member WHERE uid = ? FOR UPDATE",
        )
        .bind(uid)
        .fetch_optional(db.observe(&mut *transaction))
        .await?
        .with_context(|| format!("uid={uid} does not exist"))?;
    }
    // Take the time only after all member row locks are held: the other side may have just written a token while we
    // waited, and revoking it with a time from before the wait would predate its issue time
    // (the revoke statement additionally uses GREATEST(dateline, ?) as a fallback).
    let timestamp = crate::identity::now();
    let expires_at = timestamp
        .checked_add(ttl_seconds)
        .context("token lifetime out of range")?;
    let mut results = Vec::with_capacity(uids.len());
    for &uid in uids {
        crate::identity::rules::revoke_api_keys(db, &mut transaction, uid, None, timestamp).await?;

        let secret = format!(
            "{}{}",
            crate::identity::API_KEY_PREFIX,
            crate::identity::random_token()
        );
        let hash = crate::identity::token_hash(&secret);
        // Same as the self-service page: the hint keeps only the first 14 characters of the plaintext (prefix
        // included), not the full token.
        let hint: String = secret.chars().take(14).collect();

        crate::identity::rules::insert_api_key(
            db,
            &mut transaction,
            uid,
            &hash,
            &hint,
            timestamp,
            expires_at,
        )
        .await?;

        results.push(ProvisionedKey {
            uid,
            secret,
            hint,
            expires_at,
        });
    }
    transaction.commit().await?;
    Ok(results)
}
