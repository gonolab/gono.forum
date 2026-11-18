//! Member API tokens: long-lived credentials are stored only as digests; generation, rotation and revocation happen
//! under the member row lock.
//!
//! Tokens are independent of Cookie sessions and the credentials version: changing the password while signed in does
//! not revoke them, while a password reset by mail, signing out of all devices and a two-step verification reset do
//! (`revoke_api_keys_after`). Freeze, account deletion and any non-active status are rejected on every request, and
//! write transactions lock the account and check once more.
use super::*;
use crate::db::Tx;

/// Prefix of the raw token, to make it easy to recognize by eye and in leak scans.
pub const API_KEY_PREFIX: &str = "gonof_";
/// Length of the token hint shown on pages: the prefix plus the first 8 characters of the raw token.
const HINT_CHARS: usize = 14;
/// The last-used time is written to the database at most once a minute.
const TOUCH_INTERVAL_SECONDS: i64 = 60;
const API_KEY_COLUMNS: &str =
    "id, uid, key_hint, dateline, expires_at, last_used_at, last_used_ip, revoked_at, revoked_by";

#[derive(Clone, Debug)]
pub struct ApiKey {
    pub id: i64,
    pub uid: i64,
    pub hint: String,
    pub dateline: i64,
    pub expires_at: i64,
    pub last_used_at: i64,
    pub last_used_ip: String,
    pub revoked_at: Option<i64>,
    pub revoked_by: Option<i64>,
}

impl ApiKey {
    pub fn active_at(&self, timestamp: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at > timestamp
    }
}

// No Debug derive: the generated result contains the raw token, which is returned to the member only with this
// response.
pub struct ApiKeyGrant {
    pub key: ApiKey,
    pub secret: String,
}

/// The raw token must be the prefix plus 64 lowercase hex digits; the HTTP layer rejects anything else first, without
/// querying the database.
pub fn valid_api_secret(raw: &str) -> bool {
    raw.strip_prefix(API_KEY_PREFIX).is_some_and(|rest| {
        rest.len() == 64
            && rest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn api_key_from_row(row: &DbRow) -> Result<ApiKey> {
    Ok(ApiKey {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        hint: db::text(row, "key_hint")?,
        dateline: row.try_get("dateline")?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
        last_used_ip: db::text(row, "last_used_ip")?,
        revoked_at: row.try_get("revoked_at")?,
        revoked_by: row.try_get("revoked_by")?,
    })
}

impl Identity {
    /// The most recent unrevoked token; whether it has expired is judged by the caller from the time.
    pub async fn api_key(&self, uid: i64) -> Result<Option<ApiKey>> {
        let row = crate::db::query(&format!(
            "SELECT {API_KEY_COLUMNS} FROM gonof_common_member_api_key
             WHERE uid = ? AND revoked_at IS NULL ORDER BY id DESC LIMIT 1"
        ))
        .bind(uid)
        .fetch_optional(self.db.executor())
        .await?;
        row.as_ref().map(api_key_from_row).transpose()
    }

    /// Generates or regenerates a token: the password is verified outside the lock; under the account row lock the
    /// session, password and posting eligibility are checked again, and the same transaction revokes the old token
    /// and writes the new digest.
    ///
    /// `ttl_seconds` is the lifetime the page offered. The transaction takes the settings gate before the account
    /// row (docs/settings-and-caching.md, "Write gate and locks"), so an API switched off or a lifetime shortened
    /// while the password was being checked holds: the old token stays, and no token outlives the current lifetime.
    pub async fn create_api_key(
        &self,
        raw_sid: &str,
        current_password: &str,
        ttl_seconds: i64,
    ) -> Result<ApiKeyGrant> {
        let claims = self.cookie_claims(raw_sid)?;
        if current_password.len() > 512 || current_password.chars().count() > 128 {
            return Err(IdentityError::Unauthorized);
        }
        let candidate = crate::db::query(
            "SELECT password, credentials_version FROM gonof_common_member WHERE uid = ?",
        )
        .bind(claims.uid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(IdentityError::Unauthorized)?;
        let previous: Option<String> = candidate.try_get("password")?;
        let previous_version: i64 = candidate.try_get("credentials_version")?;
        if previous_version != claims.credentials_version {
            return Err(IdentityError::Unauthorized);
        }
        if previous.as_deref().is_none_or(str::is_empty)
            || !verify_password(current_password.to_owned(), previous.clone()).await?
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_wrong_password(),
            ));
        }
        let secret = format!("{API_KEY_PREFIX}{}", random_token());
        let hash = token_hash(&secret);
        let hint: String = secret.chars().take(HINT_CHARS).collect();
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let api = crate::runtime::locked(&self.db, &mut tx).await?.api;
        if !api.enabled {
            return Err(IdentityError::Suspended(
                crate::i18n::current()
                    .settings_error_api_site_disabled()
                    .into(),
            ));
        }
        let expires_at = timestamp
            .checked_add(ttl_seconds.min(api.key_ttl_days.saturating_mul(86_400)))
            .ok_or_else(|| {
                IdentityError::Internal(anyhow::anyhow!("token lifetime out of range"))
            })?;
        let LockedAccount { member, password } =
            self.security_account_locked(&mut tx, raw_sid).await?;
        if member.credentials_version != previous_version || password != previous {
            return Err(IdentityError::Unauthorized);
        }
        if !member.can_contribute() {
            return Err(IdentityError::Forbidden);
        }
        let rotated = self
            .revoke_active_api_keys(&mut tx, member.uid, member.uid, timestamp)
            .await?
            > 0;
        let id = rules::insert_api_key(
            &self.db, &mut tx, member.uid, &hash, &hint, timestamp, expires_at,
        )
        .await?;
        self.audit(
            &mut tx,
            member.uid,
            if rotated {
                "api.key.rotate"
            } else {
                "api.key.create"
            },
            serde_json::json!({"key_id": id, "key_hint": hint, "expires_at": expires_at}),
            timestamp,
        )
        .await?;
        tx.commit().await?;
        Ok(ApiKeyGrant {
            key: ApiKey {
                id,
                uid: member.uid,
                hint,
                dateline: timestamp,
                expires_at,
                last_used_at: 0,
                last_used_ip: String::new(),
                revoked_at: None,
                revoked_by: None,
            },
            secret,
        })
    }

    /// The member turns off the API: revokes all unrevoked tokens; returns false when there are no tokens.
    pub async fn revoke_api_key(&self, raw_sid: &str) -> Result<bool> {
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let member = self.security_member_locked(&mut tx, raw_sid).await?;
        let revoked = self
            .revoke_active_api_keys(&mut tx, member.uid, member.uid, timestamp)
            .await?;
        if revoked > 0 {
            self.audit(
                &mut tx,
                member.uid,
                "api.key.revoke",
                serde_json::json!({"revoked": revoked}),
                timestamp,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(revoked > 0)
    }

    /// An administrator revokes a given member's tokens, with the reason logged; the action is recorded once even when
    /// there are no tokens.
    pub async fn revoke_member_api_key(
        &self,
        actor: Actor,
        target_uid: i64,
        reason: &str,
    ) -> Result<bool> {
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        // The two member rows are locked in ascending uid order, consistent with `activities::lock_members` and other
        // multi-account transactions; locking oneself first and then the other side would deadlock when administrators
        // with smaller uids revoke each other, or when meeting a transaction that locks in ascending order. The other
        // side's row is taken first, but whether it exists is not judged here; errors still check administrator status
        // first, so non-administrators cannot probe whether a uid exists.
        if target_uid < actor.uid {
            crate::db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
                .bind(target_uid)
                .fetch_optional(self.db.observe(&mut *tx))
                .await?;
        }
        crate::runtime::admin_locked(&self.db, &mut tx, actor).await?;
        let target =
            self.member_locked(&mut tx, target_uid)
                .await
                .map_err(|error| match error {
                    IdentityError::Unauthorized => IdentityError::NotFound,
                    other => other,
                })?;
        let revoked = self
            .revoke_active_api_keys(&mut tx, target.uid, actor.uid, timestamp)
            .await?;
        crate::db::query("INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline) VALUES (?, ?, 'api.key.revoke', ?, 'web', ?)")
            .bind(target.uid)
            .bind(actor.uid)
            .bind(serde_json::json!({"target_uid": target.uid, "reason": reason, "revoked": revoked}).to_string())
            .bind(timestamp)
            .execute(self.db.observe(&mut *tx))
            .await?;
        tx.commit().await?;
        Ok(revoked > 0)
    }

    /// Password reset and signing out of all devices both imply the account may be stolen: whoever knows the password
    /// may have created tokens themselves, so those are revoked too. Changing the password while signed in, freezes and
    /// role changes do not go through here, and tokens stay as they are.
    pub(super) async fn revoke_api_keys_after(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        cause: &str,
        timestamp: i64,
    ) -> Result<()> {
        let revoked = self.revoke_active_api_keys(tx, uid, uid, timestamp).await?;
        if revoked > 0 {
            self.audit(
                tx,
                uid,
                "api.key.revoke",
                serde_json::json!({"revoked": revoked, "cause": cause}),
                timestamp,
            )
            .await?;
        }
        // The personal feed URL is a credential as well and is invalidated too.
        if crate::content::feed_tokens::delete_locked(&self.db, tx, uid).await? {
            self.audit(
                tx,
                uid,
                "feed.token.revoke",
                serde_json::json!({"cause": cause}),
                timestamp,
            )
            .await?;
        }
        Ok(())
    }

    async fn revoke_active_api_keys(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        revoked_by: i64,
        timestamp: i64,
    ) -> Result<u64> {
        Ok(rules::revoke_api_keys(&self.db, tx, uid, Some(revoked_by), timestamp).await?)
    }

    /// Each request looks up the token exactly by digest and then reads the member once more; no locks, the account
    /// status is checked again by the caller and the write transaction.
    pub async fn authenticate_api_key(&self, secret: &str) -> Result<Option<(ApiKey, Member)>> {
        if !valid_api_secret(secret) {
            return Ok(None);
        }
        let row = crate::db::query(&format!(
            "SELECT {API_KEY_COLUMNS} FROM gonof_common_member_api_key WHERE key_hash = ?"
        ))
        .bind(token_hash(secret))
        .fetch_optional(self.db.executor())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let key = api_key_from_row(&row)?;
        if !key.active_at(now()) {
            return Ok(None);
        }
        let row = crate::db::query(&format!(
            "SELECT {MEMBER_COLUMNS} FROM gonof_common_member WHERE uid = ?"
        ))
        .bind(key.uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut member = member_from_row(&row)?;
        self.fill_links(self.db.executor(), &mut member).await?;
        Ok(Some((key, member)))
    }

    /// The last-used time is written at most once a minute; the conditional update does no extra read.
    pub async fn touch_api_key(&self, id: i64, client_ip: &str, timestamp: i64) -> Result<()> {
        let client_ip: String = client_ip
            .chars()
            .filter(|c| !c.is_control())
            .take(45)
            .collect();
        // A revoked key is no longer recorded: a request that authenticated just before the account deletion revoked
        // the key and cleared its IP must not write the IP back afterwards.
        crate::db::query(
            "UPDATE gonof_common_member_api_key SET last_used_at = ?, last_used_ip = ?
             WHERE id = ? AND revoked_at IS NULL AND last_used_at < ? AND dateline <= ?",
        )
        .bind(timestamp)
        .bind(client_ip)
        .bind(id)
        .bind(timestamp - TOUCH_INTERVAL_SECONDS)
        // The use time must not be earlier than issuance (required by the former trigger): with clock skew across
        // machines or a clock going backwards, this use is not recorded.
        .bind(timestamp)
        .execute(self.db.executor())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_secret_requires_prefix_and_64_hex_digits() {
        let secret = format!("{API_KEY_PREFIX}{}", random_token());
        assert!(valid_api_secret(&secret));
        assert!(!valid_api_secret(&secret[1..]));
        assert!(!valid_api_secret(&secret[..secret.len() - 1]));
        assert!(!valid_api_secret(&format!("{secret}0")));
        assert!(!valid_api_secret(
            &secret.to_ascii_uppercase().replace("GONOF_", "gonof_")
        ));
        assert!(!valid_api_secret(&format!(
            "{API_KEY_PREFIX}{}",
            "g".repeat(64)
        )));
        assert!(!valid_api_secret(""));
    }

    #[test]
    fn api_key_is_active_only_before_expiry_and_without_revocation() {
        let mut key = ApiKey {
            id: 1,
            uid: 1,
            hint: "gonof_00000000".into(),
            dateline: 100,
            expires_at: 200,
            last_used_at: 0,
            last_used_ip: String::new(),
            revoked_at: None,
            revoked_by: None,
        };
        assert!(key.active_at(199));
        assert!(!key.active_at(200));
        key.revoked_at = Some(150);
        assert!(!key.active_at(120));
    }
}
