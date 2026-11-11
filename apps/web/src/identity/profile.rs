//! Profile writes recheck the current session; username changes serialize with registration.
use super::*;
use crate::db::Tx;

fn invalid_email_change() -> IdentityError {
    IdentityError::Validation(crate::i18n::current().account_error_email_change_invalid())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpaceVisibility {
    Public,
    Members,
    Private,
}

impl SpaceVisibility {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "public" => Ok(Self::Public),
            "members" => Ok(Self::Members),
            "private" => Ok(Self::Private),
            _ => Err(IdentityError::Validation(
                crate::i18n::current().settings_error_visibility(),
            )),
        }
    }

    pub fn value(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Members => "members",
            Self::Private => "private",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Public => crate::i18n::current().settings_visibility_public(),
            Self::Members => crate::i18n::current().settings_visibility_members(),
            Self::Private => crate::i18n::current().settings_visibility_private(),
        }
    }

    pub fn can_view(self, owner_uid: i64, viewer_uid: Option<i64>) -> bool {
        viewer_uid == Some(owner_uid)
            || match self {
                Self::Public => true,
                Self::Members => viewer_uid.is_some(),
                Self::Private => false,
            }
    }
}

#[derive(Clone, Debug)]
pub struct ProfileSettings {
    pub member: Member,
    pub bio: String,
    pub space_visibility: SpaceVisibility,
    pub username_available_at: i64,
    pub pending_email: Option<String>,
}

impl Identity {
    // Always acquire this before any member lock. A name spans current members
    // and history, so either table's unique constraint alone cannot serialize it.
    pub(super) async fn lock_username_namespace(&self, tx: &mut Tx<'_>) -> Result<()> {
        crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey = 'identity.username_namespace' FOR UPDATE")
        .fetch_one(self.db.observe(&mut **tx))
        .await?;
        Ok(())
    }

    pub(super) async fn check_username_reservation(
        &self,
        tx: &mut Tx<'_>,
        username: &str,
        owner: Option<i64>,
        timestamp: i64,
    ) -> Result<()> {
        let reserved: Option<i64> = crate::db::query_scalar("SELECT uid FROM gonof_common_member_username_history WHERE username = ? AND expires_at > ?",)
        .bind(username)
        .bind(timestamp)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?;
        if reserved.is_some() && reserved != owner {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_username_reserved(),
            ));
        }
        Ok(())
    }

    /// When the username can next be changed: the last rename plus the rename cooldown
    /// (`settings.username.cooldown_days`). Computed live from the last rename, so a settings change takes effect for
    /// everyone immediately.
    async fn username_available_at(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        cooldown_days: i64,
    ) -> Result<i64> {
        let latest: Option<i64> = crate::db::query_scalar("SELECT dateline FROM gonof_common_member_username_history WHERE uid = ? ORDER BY dateline DESC LIMIT 1 FOR UPDATE")
        .bind(uid)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?;
        Ok(cooldown_until(latest, cooldown_days))
    }

    /// The profile settings page is read-only: it reads a consistent snapshot in a read-only transaction, without locks
    /// and without the write lock (on SQLite opening a write transaction takes the site-wide write lock). Saving locks
    /// and rechecks in a write transaction. The rename cooldown is read before the transaction opens.
    pub async fn profile(&self, raw_sid: &str) -> Result<ProfileSettings> {
        let cooldown = self.settings().await?.username.cooldown_days;
        let mut tx = self.db.begin_read_only().await?;
        let member = self.security_member_read(&mut tx, raw_sid).await?;
        let row =
            crate::db::query("SELECT bio, space_visibility FROM gonof_common_member WHERE uid = ?")
                .bind(member.uid)
                .fetch_one(self.db.observe(&mut *tx))
                .await?;
        let bio = db::text(&row, "bio")?;
        let space_visibility = SpaceVisibility::parse(&db::text(&row, "space_visibility")?)?;
        let latest: Option<i64> = crate::db::query_scalar("SELECT dateline FROM gonof_common_member_username_history WHERE uid = ? ORDER BY dateline DESC LIMIT 1")
            .bind(member.uid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?;
        let username_available_at = cooldown_until(latest, cooldown);
        let timestamp = now();
        let pending = crate::db::query(
            "SELECT email FROM gonof_common_member_token WHERE uid = ? AND purpose = 'email_change'
             AND consumed_at IS NULL AND revoked_at IS NULL AND dateline <= ? AND expires_at > ?
             ORDER BY id DESC LIMIT 1",
        )
        .bind(member.uid)
        .bind(timestamp)
        .bind(timestamp)
        .fetch_optional(self.db.observe(&mut *tx))
        .await?;
        let pending_email = pending
            .as_ref()
            .map(|row| db::text(row, "email"))
            .transpose()?;
        tx.commit().await?;
        Ok(ProfileSettings {
            member,
            bio,
            space_visibility,
            username_available_at,
            pending_email,
        })
    }

    pub async fn update_space_visibility(&self, raw_sid: &str, value: &str) -> Result<()> {
        let visibility = SpaceVisibility::parse(value)?;
        let mut tx = self.db.begin_write().await?;
        let member = self.security_member_locked(&mut tx, raw_sid).await?;
        let changed = crate::db::query(
            "UPDATE gonof_common_member SET space_visibility = ? WHERE uid = ? AND space_visibility <> ?",
        )
        .bind(visibility.value())
        .bind(member.uid)
        .bind(visibility.value())
        .execute(self.db.observe(&mut *tx))
        .await?
        .rows_affected();
        self.audit(
            &mut tx,
            member.uid,
            "identity.privacy.update",
            serde_json::json!({"space_visibility": visibility.value()}),
            now(),
        )
        .await?;
        if changed != 0 {
            crate::content::copies::bump_site(&self.db, &mut tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_profile(&self, raw_sid: &str, username: &str, bio: &str) -> Result<()> {
        let username = normalize_username(username)?;
        let bio = bio
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim()
            .to_owned();
        if bio.chars().count() > 500
            || bio
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().settings_error_bio(),
            ));
        }
        let mut tx = self.db.begin_write().await?;
        let settings = crate::runtime::guard_write(&self.db, &mut tx, false).await?;
        self.lock_username_namespace(&mut tx).await?;
        let member = self.security_member_locked(&mut tx, raw_sid).await?;
        let timestamp = now();
        let renamed = username != member.username;
        // Bios show their addresses as links, so the new-member link block and the link level gate apply to the
        // addresses a save adds (docs/roles.md, "Level gates"). A bio belongs to no board: only the staff exempt in
        // every board, administrators and global moderators, are exempt here.
        let links = !matches!(member.adminid, 1 | 2)
            && (settings.posting.new_member_link_block || settings.growth.link_level > 1);
        // The current bio is read only while muted or when the link rules may apply.
        if member.muted() || links {
            let row =
                crate::db::query("SELECT bio, regdate FROM gonof_common_member WHERE uid = ?")
                    .bind(member.uid)
                    .fetch_one(self.db.observe(&mut *tx))
                    .await?;
            let current = crate::db::text(&row, "bio")?;
            // While muted, the username and bio cannot be changed; submitting them unchanged still passes.
            if member.muted() && (renamed || current != bio) {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().settings_error_muted(),
                ));
            }
            let regdate: i64 = row.try_get("regdate")?;
            if links
                && current != bio
                && crate::content::antispam::link_gates_apply(&settings, regdate, timestamp)
                && let Some(message) = crate::content::antispam::link_refusal(
                    &self.db,
                    &mut tx,
                    &settings,
                    (member.uid, regdate),
                    timestamp,
                    crate::content::antispam::Linked::body(&bio),
                    Some(crate::content::antispam::Linked::body(&current)),
                    None,
                )
                .await?
            {
                return Err(IdentityError::Restricted(message));
            }
        }
        if renamed {
            if self
                .username_available_at(&mut tx, member.uid, settings.username.cooldown_days)
                .await?
                > timestamp
            {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().settings_error_username_cooldown(),
                ));
            }
            self.check_username_reservation(&mut tx, &username, Some(member.uid), timestamp)
                .await?;
            // The namespace lock protects this read from registrations and renames.
            let occupied: i64 = crate::db::query_scalar(
                "SELECT COUNT(*) FROM gonof_common_member WHERE username = ? AND uid <> ?",
            )
            .bind(&username)
            .bind(member.uid)
            .fetch_one(self.db.observe(&mut *tx))
            .await?;
            if occupied != 0 {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().settings_error_username_taken(),
                ));
            }
            // Retain rows even when the owner reclaims a historical name: deleting
            // the most recent row could otherwise erase that account's cooldown.
            crate::db::query("DELETE FROM gonof_common_member_username_history WHERE username = ?")
                .bind(&member.username)
                .execute(self.db.observe(&mut *tx))
                .await?;
            let expires_at = timestamp + settings.username.history_days.saturating_mul(86_400);
            rules::check_username_history(&member.username, timestamp, expires_at)?;
            crate::db::query("INSERT INTO gonof_common_member_username_history (username, uid, dateline, expires_at) VALUES (?, ?, ?, ?)").bind(&member.username).bind(member.uid).bind(timestamp)
                .bind(expires_at)
                .execute(self.db.observe(&mut *tx)).await?;
        }
        crate::db::query("UPDATE gonof_common_member SET username = ?, bio = ? WHERE uid = ?")
            .bind(&username)
            .bind(&bio)
            .bind(member.uid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        if renamed {
            // The author copy on the member's own threads and the last replier on threads ending with the member's
            // reply are rewritten along with it, and the site-wide version goes up by one.
            crate::content::copies::member_changed(&self.db, &mut tx, member.uid).await?;
        }
        self.audit(
            &mut tx,
            member.uid,
            "identity.profile.update",
            serde_json::json!({"username_changed": renamed}),
            timestamp,
        )
        .await?;
        tx.commit().await?;
        // The moderator roster stores usernames (shown in the forum page header, global cache 3.5).
        if renamed {
            self.global
                .announce(&[crate::cache::Block::Moderators])
                .await
                .map_err(IdentityError::Internal)?;
        }
        Ok(())
    }

    pub(super) async fn revoke_email_change_tokens(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        timestamp: i64,
    ) -> Result<()> {
        crate::db::query("UPDATE gonof_common_member_token SET revoked_at = CASE WHEN dateline > ? THEN dateline ELSE ? END
             WHERE uid = ? AND purpose = 'email_change' AND consumed_at IS NULL AND revoked_at IS NULL").bind(timestamp).bind(timestamp).bind(uid).execute(self.db.observe(&mut **tx)).await?;
        Ok(())
    }

    pub async fn request_email_change(
        &self,
        raw_sid: &str,
        current_password: &str,
        new_email: &str,
    ) -> Result<()> {
        let email = normalize_email(new_email)?;
        let claims = self.cookie_claims(raw_sid)?;
        if current_password.len() > 512 || current_password.chars().count() > 128 {
            return Err(IdentityError::Unauthorized);
        }
        // Written as an absolute time when issued, so a settings change only affects links sent afterwards.
        let ttl_hours = self.settings().await?.account.email_change_ttl_hours;
        let candidate = crate::db::query(
            "SELECT m.password, m.credentials_version
             FROM gonof_common_member m WHERE m.uid = ?",
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
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } =
            self.security_account_locked(&mut tx, raw_sid).await?;
        if member.credentials_version != previous_version || password != previous {
            return Err(IdentityError::Unauthorized);
        }
        if member.email.as_deref() == Some(email.as_str()) {
            return Err(IdentityError::Validation(
                crate::i18n::current().settings_error_email_same(),
            ));
        }
        let occupied: i64 =
            crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member WHERE email = ?")
                .bind(&email)
                .fetch_one(self.db.observe(&mut *tx))
                .await?;
        if occupied != 0 {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_email_taken(),
            ));
        }
        let timestamp = now();
        self.revoke_email_change_tokens(&mut tx, member.uid, timestamp)
            .await?;
        let token = random_token();
        let digest = token_hash(&token);
        rules::insert_token(
            &self.db,
            &mut tx,
            member.uid,
            rules::TokenPurpose::EmailChange,
            &digest,
            &email,
            timestamp,
            timestamp + ttl_hours.saturating_mul(3600),
        )
        .await?;
        crate::db::query("INSERT INTO gonof_common_outbox (event_key, event_type, aggregate_type, aggregate_id, payload, dateline, available_at)
             VALUES (?, 'email_change', 'member', ?, ?, ?, ?)").bind(format!("email_change:{digest}")).bind(member.uid)
            .bind(crate::mail::identity_payload(&email, &member.username, &token))
            .bind(timestamp).bind(timestamp).execute(self.db.observe(&mut *tx)).await?;
        self.audit(
            &mut tx,
            member.uid,
            "identity.email.change_request",
            serde_json::json!({}),
            timestamp,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn email_change_status(&self, raw_token: &str) -> Result<Option<String>> {
        if raw_token.len() != 64 || !raw_token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let Some(preview) = self.token_preview(raw_token, "email_change").await? else {
            return Ok(None);
        };
        // Matches the recheck under lock in confirm_email_change.
        let open = preview.open_account()
            && preview.usable_at(now())
            && preview.member_email.as_deref() != Some(preview.token_email.as_str());
        Ok(open.then_some(preview.token_email))
    }

    pub async fn confirm_email_change(&self, raw_token: &str) -> Result<()> {
        if raw_token.len() != 64 || !raw_token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid_email_change());
        }
        let digest = token_hash(raw_token);
        let uid: Option<i64> = crate::db::query_scalar("SELECT uid FROM gonof_common_member_token WHERE token_hash = ? AND purpose = 'email_change'").bind(&digest).fetch_optional(self.db.executor()).await?;
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } = self
            .account_locked(&mut tx, uid.ok_or_else(invalid_email_change)?)
            .await?;
        let row = crate::db::query("SELECT id, email, dateline, expires_at, consumed_at, revoked_at FROM gonof_common_member_token
             WHERE token_hash = ? AND purpose = 'email_change' AND uid = ? FOR UPDATE").bind(&digest).bind(member.uid).fetch_one(self.db.observe(&mut *tx)).await?;
        let timestamp = now();
        let email = db::text(&row, "email")?;
        if !member.active()
            || member.frozen
            || password.as_deref().is_none_or(str::is_empty)
            || member.email.as_deref() == Some(email.as_str())
            || row.try_get::<i64, _>("dateline")? > timestamp
            || row.try_get::<i64, _>("expires_at")? <= timestamp
            || row.try_get::<Option<i64>, _>("consumed_at")?.is_some()
            || row.try_get::<Option<i64>, _>("revoked_at")?.is_some()
        {
            return Err(invalid_email_change());
        }
        let consumed = crate::db::query("UPDATE gonof_common_member_token SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL AND revoked_at IS NULL AND dateline <= ? AND expires_at > ?").bind(timestamp).bind(row.try_get::<i64, _>("id")?).bind(timestamp).bind(timestamp)
            .execute(self.db.observe(&mut *tx)).await?.rows_affected();
        if consumed != 1 {
            return Err(invalid_email_change());
        }
        let version = member.credentials_version.checked_add(1).ok_or_else(|| {
            IdentityError::Internal(anyhow::anyhow!("credentials version at its limit"))
        })?;
        crate::db::query("UPDATE gonof_common_member SET email = ?, emailstatus = 1, credentials_version = ? WHERE uid = ?")
            .bind(&email).bind(version).bind(member.uid).execute(self.db.observe(&mut *tx)).await?;
        crate::db::query("UPDATE gonof_common_member_token SET revoked_at = CASE WHEN dateline > ? THEN dateline ELSE ? END WHERE uid = ? AND consumed_at IS NULL AND revoked_at IS NULL").bind(timestamp).bind(timestamp).bind(member.uid).execute(self.db.observe(&mut *tx)).await?;
        self.audit(
            &mut tx,
            member.uid,
            "identity.email.change",
            serde_json::json!({"credentials_version": version}),
            timestamp,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// When the username can next be changed: the last rename plus the cooldown, or 0 if never renamed.
fn cooldown_until(latest: Option<i64>, cooldown_days: i64) -> i64 {
    latest.map_or(0, |timestamp| {
        timestamp.saturating_add(cooldown_days.saturating_mul(86_400))
    })
}

#[cfg(test)]
mod tests {
    use super::{IdentityError, SpaceVisibility};

    #[test]
    fn space_visibility_accepts_only_exact_values() {
        for visibility in [
            SpaceVisibility::Public,
            SpaceVisibility::Members,
            SpaceVisibility::Private,
        ] {
            assert_eq!(
                SpaceVisibility::parse(visibility.value()).unwrap(),
                visibility
            );
        }
        for invalid in [
            "",
            "PUBLIC",
            "Public",
            " public",
            "public ",
            "members\n",
            "private\0",
            "friends",
        ] {
            assert!(matches!(
                SpaceVisibility::parse(invalid),
                Err(IdentityError::Validation(_))
            ));
        }
    }

    #[test]
    fn space_visibility_grants_only_the_selected_audience() {
        for visibility in [
            SpaceVisibility::Public,
            SpaceVisibility::Members,
            SpaceVisibility::Private,
        ] {
            assert!(visibility.can_view(101, Some(101)));
        }
        assert!(SpaceVisibility::Public.can_view(101, None));
        assert!(SpaceVisibility::Public.can_view(101, Some(102)));
        assert!(!SpaceVisibility::Members.can_view(101, None));
        assert!(SpaceVisibility::Members.can_view(101, Some(102)));
        assert!(!SpaceVisibility::Private.can_view(101, None));
        assert!(!SpaceVisibility::Private.can_view(101, Some(102)));
    }
}
