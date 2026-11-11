//! Sensitive account operations first verify the encrypted cookie, then lock in the order member → one-time token.
use super::*;
use crate::db::Tx;

fn invalid_reset() -> IdentityError {
    IdentityError::Validation(crate::i18n::current().account_error_reset_used())
}

const RESET_LOG: &str = "identity.password.reset";

/// What a usable password reset link will do, read before the new password is entered.
#[derive(Clone, Copy, Debug)]
pub struct ResetPreview {
    /// The account's email was never verified: resetting verifies it and removes the other ways to sign in.
    pub claims: bool,
}

/// Bound to the verified session and the password snapshot; submitting a sensitive operation must still re-check
/// inside the member row lock.
pub(crate) struct VerifiedPassword<'a> {
    pub(super) raw_sid: &'a str,
    pub(super) claims: cookie::CookieClaims,
    pub(super) previous: Option<String>,
    pub(super) username: String,
}

/// Binds a new password that already passed the length check to this credential check.
pub(crate) struct PasswordChange<'a> {
    current: VerifiedPassword<'a>,
    replacement: &'a str,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMetadata {
    pub(super) user_agent: String,
    pub(super) client_ip: String,
    pub(super) registration_source: crate::registration_sources::RegistrationSource,
    /// The raw device-marker cookie the browser sent; an empty string when absent. `None` means the caller is not
    /// a web sign-in (tests, CLI): no new-device check is made and no new marker is issued.
    pub(super) device: Option<String>,
}

impl SessionMetadata {
    pub fn new(user_agent: &str, client_ip: &str) -> Self {
        Self {
            user_agent: user_agent
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect::<String>()
                .trim()
                .to_owned(),
            client_ip: client_ip
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Web sign-in and registration: pass the device marker from the browser (an empty string if there is none).
    pub fn with_device(mut self, marker: &str) -> Self {
        self.device = Some(marker.to_owned());
        self
    }

    pub(crate) fn with_registration_source(
        mut self,
        origin: Option<&str>,
        channel: Option<&str>,
    ) -> Self {
        self.registration_source =
            crate::registration_sources::RegistrationSource::from_request(origin, channel);
        self
    }
}

fn valid_token(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl Identity {
    /// After locking the member, verifies the credential version and the cookie lifetime again; the online table
    /// plays no part in authorization.
    pub(super) async fn security_member_locked(
        &self,
        tx: &mut Tx<'_>,
        raw_cookie: &str,
    ) -> Result<Member> {
        let claims = self.cookie_claims(raw_cookie)?;
        let member = self.member_locked(tx, claims.uid).await?;
        if !member.active()
            || member.frozen
            || member.credentials_version != claims.credentials_version
            || !claims.valid_at(now())
        {
            return Err(IdentityError::Unauthorized);
        }
        Ok(member)
    }

    /// Same as [`Self::security_member_locked`] without locking: only for page reads in read-only transactions;
    /// writes still rely on the re-check inside the lock.
    pub(super) async fn security_member_read(
        &self,
        tx: &mut Tx<'_>,
        raw_cookie: &str,
    ) -> Result<Member> {
        let claims = self.cookie_claims(raw_cookie)?;
        let member = self.member_read(tx, claims.uid).await?;
        if !member.active()
            || member.frozen
            || member.credentials_version != claims.credentials_version
            || !claims.valid_at(now())
        {
            return Err(IdentityError::Unauthorized);
        }
        Ok(member)
    }

    pub(super) async fn security_account_locked(
        &self,
        tx: &mut Tx<'_>,
        raw_cookie: &str,
    ) -> Result<LockedAccount> {
        let claims = self.cookie_claims(raw_cookie)?;
        let account = self.account_locked(tx, claims.uid).await?;
        if !account.member.active()
            || account.member.frozen
            || account.member.credentials_version != claims.credentials_version
            || !claims.valid_at(now())
        {
            return Err(IdentityError::Unauthorized);
        }
        Ok(account)
    }

    async fn revoke_reset_tokens(&self, tx: &mut Tx<'_>, uid: i64, timestamp: i64) -> Result<()> {
        crate::db::query("UPDATE gonof_common_member_token SET revoked_at = CASE WHEN dateline > ? THEN dateline ELSE ? END
             WHERE uid = ? AND purpose = 'password_reset' AND consumed_at IS NULL AND revoked_at IS NULL")
            .bind(timestamp).bind(timestamp).bind(uid)
            .execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }

    pub(super) async fn replace_password(
        &self,
        tx: &mut Tx<'_>,
        member: &Member,
        encoded: &str,
        timestamp: i64,
        operation: &str,
    ) -> Result<i64> {
        let version = member.credentials_version.checked_add(1).ok_or_else(|| {
            IdentityError::Internal(anyhow::anyhow!("credentials version at its limit"))
        })?;
        let changed = crate::db::query("UPDATE gonof_common_member SET password = ?, credentials_version = ? WHERE uid = ? AND credentials_version = ?")
            .bind(encoded).bind(version).bind(member.uid).bind(member.credentials_version)
            .execute(self.db.observe(&mut **tx)).await?.rows_affected();
        if changed != 1 {
            return Err(IdentityError::Unauthorized);
        }
        crate::db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND purpose IN ('appeal_access','privacy_access') AND consumed_at IS NULL AND revoked_at IS NULL")
            .bind(timestamp).bind(member.uid).execute(self.db.observe(&mut **tx)).await?;
        self.revoke_reset_tokens(tx, member.uid, timestamp).await?;
        self.revoke_email_change_tokens(tx, member.uid, timestamp)
            .await?;
        // Two-step verification stays enabled (password recovery must not bypass it); only the failure count
        // is reset so the owner is not locked out. Recovery that claims an account with an unverified email has
        // already removed it (`claim_account`).
        self.clear_two_factor_failures(tx, member.uid).await?;
        self.audit(
            tx,
            member.uid,
            operation,
            serde_json::json!({"credentials_version": version}),
            timestamp,
        )
        .await
    }

    pub async fn change_password(
        &self,
        raw_sid: &str,
        current_password: &str,
        new_password: &str,
    ) -> Result<()> {
        let verified = self
            .check_password_change(raw_sid, current_password, new_password)
            .await?;
        self.change_verified_password(verified).await
    }

    pub(crate) async fn check_password_change<'a>(
        &self,
        raw_sid: &'a str,
        current_password: &str,
        new_password: &'a str,
    ) -> Result<PasswordChange<'a>> {
        validate_password(
            new_password,
            self.settings().await?.account.password_min_length,
        )?;
        if current_password.len() > 512 || current_password.chars().count() > 128 {
            return Err(IdentityError::Unauthorized);
        }
        Ok(PasswordChange {
            current: self
                .check_current_password(raw_sid, current_password)
                .await?,
            replacement: new_password,
        })
    }

    pub(crate) async fn check_current_password<'a>(
        &self,
        raw_sid: &'a str,
        current_password: &str,
    ) -> Result<VerifiedPassword<'a>> {
        let claims = self.cookie_claims(raw_sid)?;
        if current_password.len() > 512 || current_password.chars().count() > 128 {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_wrong_password(),
            ));
        }
        let candidate = crate::db::query(
            "SELECT username, password, credentials_version FROM gonof_common_member WHERE uid = ?",
        )
        .bind(claims.uid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(IdentityError::Unauthorized)?;
        if candidate.try_get::<i64, _>("credentials_version")? != claims.credentials_version {
            return Err(IdentityError::Unauthorized);
        }
        let previous: Option<String> = candidate.try_get("password")?;
        // Accounts registered by SMS have no password: say plainly that one must be set first, and do not count it as a
        // "wrong password".
        if previous.as_deref().is_none_or(str::is_empty) {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_no_password(),
            ));
        }
        if !verify_password(current_password.to_owned(), previous.clone()).await? {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_wrong_password(),
            ));
        }
        Ok(VerifiedPassword {
            raw_sid,
            claims,
            previous,
            username: db::text(&candidate, "username")?,
        })
    }

    pub(crate) async fn change_verified_password(&self, change: PasswordChange<'_>) -> Result<()> {
        let verified = change.current;
        let encoded = hash_password(change.replacement.to_owned()).await?;
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } = self
            .security_account_locked(&mut tx, verified.raw_sid)
            .await?;
        if member.credentials_version != verified.claims.credentials_version
            || password != verified.previous
        {
            return Err(IdentityError::Unauthorized);
        }
        self.replace_password(
            &mut tx,
            &member,
            &encoded,
            now(),
            "identity.password.change",
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// No account eligibility details escape this method. Request throttling and
    /// response timing are applied by HTTP before returning the same receipt.
    pub async fn request_password_reset(&self, email: &str) -> Result<()> {
        let email = normalize_email(email)?;
        let uid: Option<i64> =
            crate::db::query_scalar("SELECT uid FROM gonof_common_member WHERE email = ?")
                .bind(&email)
                .fetch_optional(self.db.executor())
                .await?;
        let Some(uid) = uid else {
            return Ok(());
        };
        // Written as an absolute instant when issued; changing the setting only affects links issued afterwards.
        let ttl_minutes = self.settings().await?.account.reset_ttl_minutes;
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } = self.account_locked(&mut tx, uid).await?;
        if !member.active()
            || member.frozen
            || member.email.as_deref() != Some(email.as_str())
            || password.as_deref().is_none_or(str::is_empty)
        {
            tx.commit().await?;
            return Ok(());
        }
        let timestamp = now();
        self.revoke_reset_tokens(&mut tx, uid, timestamp).await?;
        let token = random_token();
        let digest = token_hash(&token);
        rules::insert_token(
            &self.db,
            &mut tx,
            uid,
            rules::TokenPurpose::PasswordReset,
            &digest,
            &email,
            timestamp,
            timestamp + ttl_minutes.saturating_mul(60),
        )
        .await?;
        crate::db::query("INSERT INTO gonof_common_outbox (event_key, event_type, aggregate_type, aggregate_id, payload, dateline, available_at)
             VALUES (?, 'password_reset', 'member', ?, ?, ?, ?)")
            .bind(format!("password_reset:{digest}")).bind(uid)
            .bind(crate::mail::identity_payload(&email, &member.username, &token))
            .bind(timestamp).bind(timestamp).execute(self.db.observe(&mut *tx)).await?;
        self.audit(
            &mut tx,
            uid,
            "identity.password.request",
            serde_json::json!({}),
            timestamp,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn password_reset_status(&self, raw_token: &str) -> Result<bool> {
        Ok(self.password_reset_preview(raw_token).await?.is_some())
    }

    /// `None` when the link cannot be used; otherwise whether using it claims the account (its email was never
    /// verified, see [`Self::reset_password`]), which the reset page says before the new password is entered.
    pub async fn password_reset_preview(&self, raw_token: &str) -> Result<Option<ResetPreview>> {
        if !valid_token(raw_token) {
            return Ok(None);
        }
        let Some(preview) = self.token_preview(raw_token, "password_reset").await? else {
            return Ok(None);
        };
        // Same as the re-check inside the lock in reset_password.
        let usable = preview.open_account()
            && preview.usable_at(now())
            && preview.member_email.as_deref() == Some(preview.token_email.as_str());
        Ok(usable.then_some(ResetPreview {
            claims: !preview.email_verified,
        }))
    }

    pub async fn reset_password(&self, raw_token: &str, new_password: &str) -> Result<()> {
        validate_password(
            new_password,
            self.settings().await?.account.password_min_length,
        )?;
        if !self.password_reset_status(raw_token).await? {
            return Err(invalid_reset());
        }
        // Never hold a database lock during Argon2. The token is checked again below.
        let encoded = hash_password(new_password.to_owned()).await?;
        let digest = token_hash(raw_token);
        let uid: Option<i64> = crate::db::query_scalar("SELECT uid FROM gonof_common_member_token WHERE token_hash = ? AND purpose = 'password_reset'")
            .bind(&digest).fetch_optional(self.db.executor()).await?;
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } = self
            .account_locked(&mut tx, uid.ok_or_else(invalid_reset)?)
            .await?;
        let row = crate::db::query("SELECT id, email, dateline, expires_at, consumed_at, revoked_at FROM gonof_common_member_token
             WHERE token_hash = ? AND purpose = 'password_reset' AND uid = ? FOR UPDATE")
            .bind(&digest).bind(member.uid).fetch_optional(self.db.observe(&mut *tx)).await?
            .ok_or_else(invalid_reset)?;
        let timestamp = now();
        if !member.active()
            || member.frozen
            || password.as_deref().is_none_or(str::is_empty)
            || member.email.as_deref() != Some(db::text(&row, "email")?.as_str())
            || row.try_get::<i64, _>("dateline")? > timestamp
            || row.try_get::<i64, _>("expires_at")? <= timestamp
            || row.try_get::<Option<i64>, _>("consumed_at")?.is_some()
            || row.try_get::<Option<i64>, _>("revoked_at")?.is_some()
        {
            return Err(invalid_reset());
        }
        let consumed = crate::db::query("UPDATE gonof_common_member_token SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL
             AND revoked_at IS NULL AND dateline <= ? AND expires_at > ?")
            .bind(timestamp).bind(row.try_get::<i64, _>("id")?).bind(timestamp).bind(timestamp)
            .execute(self.db.observe(&mut *tx)).await?.rows_affected();
        if consumed != 1 {
            return Err(invalid_reset());
        }
        if !member.email_verified {
            self.claim_account(&mut tx, &member, timestamp).await?;
        }
        self.replace_password(&mut tx, &member, &encoded, timestamp, RESET_LOG)
            .await?;
        self.revoke_api_keys_after(&mut tx, member.uid, RESET_LOG, timestamp)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Recovering an account whose email was never verified is the first time anyone proved they own the address, so
    /// the recovery claims the account: the address counts as verified, and everything else that could sign in or
    /// stand in the owner's way goes — linked Google and GitHub accounts, the phone number and two-step verification,
    /// all set up by whoever knew the password before. Without this, someone who signed up with another person's
    /// address could link their own account and sign in again after the owner recovered it. The password itself,
    /// every session, API tokens and the feed URL are replaced or revoked by the reset as for any account.
    /// The caller holds the member row lock.
    async fn claim_account(&self, tx: &mut Tx<'_>, member: &Member, timestamp: i64) -> Result<()> {
        self.verify_email_by_recovery(tx, member.uid, timestamp)
            .await?;
        self.remove_links_on_claim(tx, member.uid, RESET_LOG, timestamp)
            .await?;
        self.remove_mobile_on_claim(tx, member, RESET_LOG, timestamp)
            .await?;
        self.remove_two_factor_on_claim(tx, member.uid, RESET_LOG, timestamp)
            .await
    }

    /// Rotates the account credential version; after commit every old cookie must sign in again.
    pub async fn logout_all(&self, raw_cookie: &str) -> Result<()> {
        let mut tx = self.db.begin_write().await?;
        let member = self.security_member_locked(&mut tx, raw_cookie).await?;
        let version = member
            .credentials_version
            .checked_add(1)
            .ok_or(IdentityError::Conflict(
                crate::i18n::current().account_error_credentials_version(),
            ))?;
        let changed = crate::db::query("UPDATE gonof_common_member SET credentials_version = ? WHERE uid = ? AND credentials_version = ? AND `freeze` = 0")
            .bind(version).bind(member.uid).bind(member.credentials_version)
            .execute(self.db.observe(&mut *tx)).await?.rows_affected();
        if changed != 1 {
            return Err(IdentityError::Unauthorized);
        }
        let timestamp = now();
        self.audit(
            &mut tx,
            member.uid,
            "identity.session.logout_all",
            serde_json::json!({"credentials_version": version}),
            timestamp,
        )
        .await?;
        self.revoke_api_keys_after(
            &mut tx,
            member.uid,
            "identity.session.logout_all",
            timestamp,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_metadata_is_bounded_and_uses_only_valid_ip_addresses() {
        // Cut by characters, not bytes: "\u{8349}" (the Chinese for "grass") is three bytes.
        let metadata = SessionMetadata::new(
            &format!("Chrome\r\n\t{}", "\u{8349}".repeat(600)),
            "2001:db8::1",
        );
        assert_eq!(metadata.user_agent.chars().count(), 512);
        assert!(!metadata.user_agent.chars().any(char::is_control));
        assert_eq!(metadata.client_ip, "2001:db8::1");
        assert!(
            SessionMetadata::new("", "forwarded, 127.0.0.1")
                .client_ip
                .is_empty()
        );
    }
}
