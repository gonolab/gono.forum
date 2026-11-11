//! Third-party accounts (Google, GitHub): logging in with a third party, completing registration when not linked,
//! linking and unlinking under account security, and setting a password for an account without one after re-authorizing
//! with the third party to confirm identity. The authorization protocol is in [`crate::oauth`].
//!
//! - The link table `gonof_common_member_external` is written, updated with the last login time and unlinked only by
//!   this module, and deleted by `privacy/worker.rs` on account deletion (pinned by a source scan). The identity key
//!   is the immutable ID given by the provider, never e-mail or username.
//! - One-time values between starting authorization and the callback, and identities confirmed by the callback but not
//!   yet stored, all live in encrypted cookies, never in the database.
//! - Linking and unlinking require the current password (an account without one sets a password first)
//!   and are rechecked under the member row lock; the lock order is member row first, then link row. Logging in with
//!   a third party also reads the member ID by identity first, locks the member row, then locks the link row to
//!   confirm it has not been unlinked, in the same order as unlinking.
//! - An account with an email nobody has verified yet cannot link, and password recovery of such an account removes
//!   its links ([`Identity::reset_password`]): otherwise someone could sign up with another person's address, link
//!   their own account and keep signing in after the address's owner recovered the account.
use super::*;
use crate::db::Tx;
use crate::oauth::{Profile, Provider};

/// How long from starting authorization to the callback at most (time spent on the provider's login page included).
pub const FLOW_TTL: i64 = 600;
/// How long an identity confirmed by the callback waits for "Finish signing up" or "Confirm linking".
pub const PENDING_TTL: i64 = 1800;
/// How long the set-password form stays valid after confirming identity with the third party.
pub const CONFIRM_TTL: i64 = 600;
const FLOW_AAD: &[u8] = b"gonof.oauth-flow.v1";
const PENDING_AAD: &[u8] = b"gonof.oauth-pending.v1";

pub(crate) const LINK_LOG: &str = "identity.external.link";
pub(crate) const UNLINK_LOG: &str = "identity.external.unlink";
/// What one authorization is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowPurpose {
    /// Not signed in: log in if linked, otherwise go on to complete registration.
    Login,
    /// Signed in: link to the current account (the current password must still be entered after the callback).
    Link,
    /// Signed in without a password: confirm identity with the linked third party, then set a password.
    Confirm,
}

impl FlowPurpose {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "" | "login" => Some(Self::Login),
            "link" => Some(Self::Link),
            "confirm" => Some(Self::Confirm),
            _ => None,
        }
    }
}

/// What is written into the encrypted cookie when authorization starts. Linking and confirming require the session at
/// callback time to be the same member as at the start, with an unchanged credentials version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flow {
    pub provider: Provider,
    pub purpose: FlowPurpose,
    pub uid: i64,
    pub credentials_version: i64,
    pub challenge: crate::oauth::Challenge,
    pub return_to: String,
    pub expires_at: i64,
}

/// A third-party identity confirmed by the callback but not yet stored (used by the three steps complete registration,
/// confirm link and set password). `uid` 0 means pending registration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub purpose: FlowPurpose,
    pub uid: i64,
    pub credentials_version: i64,
    pub provider: Provider,
    pub subject: String,
    pub display: String,
    /// The e-mail address verified by the provider.
    pub email: Option<String>,
    pub suggestion: String,
    pub expires_at: i64,
}

/// The link written along with registration.
pub(crate) struct NewLink {
    pub provider: Provider,
    pub subject: String,
    pub display: String,
}

/// Result of logging in with a third party: a linked login (or a switch to two-step verification); an unlinked one goes
/// on to complete registration, carrying the encrypted pending identity.
pub enum ExternalLoginOutcome {
    Login(LoginOutcome),
    Register(String),
}

/// One link as seen on the account security page, in the admin area and in data exports.
#[derive(Clone, Debug)]
pub struct LinkedAccount {
    pub provider: Provider,
    pub subject: String,
    pub display: String,
    pub dateline: i64,
    pub last_login_at: i64,
}

fn duplicate(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => crate::i18n::current().account_error_google_taken(),
        Provider::Github => crate::i18n::current().account_error_github_taken(),
    }
}

fn already(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => crate::i18n::current().account_error_google_already(),
        Provider::Github => crate::i18n::current().account_error_github_already(),
    }
}

/// Linking waits until the account's email is verified ([`Member::email_unverified`]).
pub(crate) fn verify_email_first() -> IdentityError {
    IdentityError::Conflict(crate::i18n::current().account_error_verify_email_first())
}

fn unregistered_closed(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => crate::i18n::current().account_error_google_closed(),
        Provider::Github => crate::i18n::current().account_error_github_closed(),
    }
}

/// Whether an identity is linked: checked under lock inside the transaction before registering and linking (the
/// primary key `(provider, subject)` and the unique key `(uid, provider)` are the safeguards).
pub(crate) async fn check_unlinked(db: &Database, tx: &mut Tx<'_>, link: &NewLink) -> Result<()> {
    let taken: Option<i64> = crate::db::query_scalar(
        "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=? FOR UPDATE",
    )
    .bind(link.provider.value())
    .bind(&link.subject)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    if taken.is_some() {
        return Err(IdentityError::Conflict(duplicate(link.provider)));
    }
    Ok(())
}

/// Writes one link (two places: registration and linking). The caller has locked the member row and confirmed the
/// identity is not linked.
pub(crate) async fn insert_link(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    link: &NewLink,
    timestamp: i64,
) -> Result<()> {
    crate::db::query("INSERT INTO gonof_common_member_external(provider,subject,uid,display,dateline) VALUES(?,?,?,?,?)")
        .bind(link.provider.value())
        .bind(&link.subject)
        .bind(uid)
        .bind(&link.display)
        .bind(timestamp)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

fn link_from_row(row: &DbRow) -> Result<LinkedAccount> {
    let provider = db::text(row, "provider")?;
    Ok(LinkedAccount {
        provider: Provider::parse(&provider).ok_or_else(|| {
            IdentityError::Internal(anyhow::anyhow!("unknown sign-in provider {provider}"))
        })?,
        subject: db::text(row, "subject")?,
        display: db::text(row, "display")?,
        dateline: row.try_get("dateline")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

/// A member's links (at most two rows, read by the unique key `(uid, provider)`).
pub async fn linked_accounts(db: &Database, uid: i64) -> Result<Vec<LinkedAccount>> {
    let rows = crate::db::query(
        "SELECT provider,subject,display,dateline,last_login_at FROM gonof_common_member_external WHERE uid=? ORDER BY provider",
    )
    .bind(uid)
    .fetch_all(db.executor())
    .await?;
    rows.iter().map(link_from_row).collect()
}

/// The links of one page of members (admin user list): uses the unique key via `uid IN (...)`, reading at most twice as
/// many rows as members on the page.
pub async fn linked_providers(db: &Database, uids: &[i64]) -> Result<Vec<(i64, Provider)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql = format!(
        "SELECT uid,provider FROM gonof_common_member_external WHERE uid IN ({placeholders}) ORDER BY uid,provider"
    );
    let mut query = crate::db::query(&sql);
    for uid in uids {
        query = query.bind(*uid);
    }
    let rows = query.fetch_all(db.executor()).await?;
    rows.iter()
        .map(|row| {
            let provider = db::text(row, "provider")?;
            Ok((
                row.try_get("uid")?,
                Provider::parse(&provider).ok_or_else(|| {
                    IdentityError::Internal(anyhow::anyhow!("unknown sign-in provider {provider}"))
                })?,
            ))
        })
        .collect()
}

impl Identity {
    /// Provider endpoints (tests swap in a local fake service; see [`crate::oauth::Endpoints`]).
    pub fn oauth_endpoints(&self) -> &crate::oauth::Endpoints {
        &self.oauth
    }

    pub fn seal_flow(&self, flow: &Flow) -> Result<String> {
        self.config
            .cookie
            .seal_value(FLOW_AAD, flow)
            .map_err(IdentityError::Internal)
    }

    /// Reads the cookie from when authorization started; `None` if expired or undecryptable.
    pub fn open_flow(&self, raw: &str) -> Option<Flow> {
        let timestamp = now();
        self.config
            .cookie
            .open_value::<Flow>(FLOW_AAD, raw)
            .filter(|flow| flow.expires_at > timestamp && flow.expires_at <= timestamp + FLOW_TTL)
    }

    fn seal_pending(
        &self,
        purpose: FlowPurpose,
        member: Option<&Member>,
        provider: Provider,
        profile: Profile,
        ttl: i64,
    ) -> Result<String> {
        self.config
            .cookie
            .seal_value(
                PENDING_AAD,
                &Pending {
                    purpose,
                    uid: member.map_or(0, |member| member.uid),
                    credentials_version: member.map_or(0, |member| member.credentials_version),
                    provider,
                    subject: profile.subject,
                    display: profile.display,
                    email: profile.email,
                    suggestion: profile.suggestion,
                    expires_at: now() + ttl,
                },
            )
            .map_err(IdentityError::Internal)
    }

    /// Reads the identity confirmed by the callback: both purpose and member must match; `None` if expired.
    pub fn open_pending(&self, raw: &str, purpose: FlowPurpose, uid: i64) -> Option<Pending> {
        let timestamp = now();
        let ttl = if purpose == FlowPurpose::Confirm {
            CONFIRM_TTL
        } else {
            PENDING_TTL
        };
        self.config
            .cookie
            .open_value::<Pending>(PENDING_AAD, raw)
            .filter(|pending| {
                pending.purpose == purpose
                    && pending.uid == uid
                    && pending.expires_at > timestamp
                    && pending.expires_at <= timestamp + ttl
            })
    }

    /// Providers with configured credentials: the Client ID and the decrypted Client Secret. `None` if not configured or
    /// undecryptable.
    pub async fn oauth_credentials(&self, provider: Provider) -> Result<Option<(String, String)>> {
        crate::sms::oauth_credentials(&self.db, &self.common, provider)
            .await
            .map_err(IdentityError::Internal)
    }

    /// Logging in with a third party: log in if linked (switching to the second step when two-step verification is on
    /// and the admin setting requires it); if not linked, see whether registration is possible.
    pub async fn external_login(
        &self,
        provider: Provider,
        profile: Profile,
        metadata: &SessionMetadata,
    ) -> Result<ExternalLoginOutcome> {
        let settings = self.settings().await?;
        let timestamp = now();
        let bound: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=?",
        )
        .bind(provider.value())
        .bind(&profile.subject)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(uid) = bound else {
            if settings.site.read_only {
                return Err(IdentityError::Suspended(
                    settings.site.read_only_notice(crate::i18n::current()),
                ));
            }
            if settings.signup.mode == "closed" {
                return Err(IdentityError::Conflict(unregistered_closed(provider)));
            }
            if let Some(email) = profile.email.as_deref() {
                let email = normalize_email(email).ok();
                let taken: Option<i64> = match &email {
                    Some(email) => {
                        crate::db::query_scalar("SELECT uid FROM gonof_common_member WHERE email=?")
                            .bind(email)
                            .fetch_optional(self.db.executor())
                            .await?
                    }
                    None => None,
                };
                if taken.is_some() {
                    return Err(IdentityError::Conflict(
                        crate::i18n::current().account_error_email_taken(),
                    ));
                }
            }
            let mut profile = profile;
            profile.email = profile
                .email
                .as_deref()
                .and_then(|email| normalize_email(email).ok());
            let token =
                self.seal_pending(FlowPurpose::Login, None, provider, profile, PENDING_TTL)?;
            return Ok(ExternalLoginOutcome::Register(token));
        };
        let mut tx = self.db.begin_write().await?;
        let member = self.member_locked(&mut tx, uid).await?;
        let still: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=? FOR UPDATE",
        )
        .bind(provider.value())
        .bind(&profile.subject)
        .fetch_optional(self.db.observe(&mut *tx))
        .await?;
        if still != Some(uid) {
            // Just unlinked or relinked: treat as not signed in and authorize again.
            return Err(IdentityError::Unauthorized);
        }
        if !member.active() || member.frozen {
            if let Some(message) = crate::moderation::frozen_notice(&self.db, &mut tx, uid).await? {
                return Err(IdentityError::Suspended(message));
            }
            return Err(IdentityError::Unauthorized);
        }
        crate::db::query(
            "UPDATE gonof_common_member_external SET last_login_at=GREATEST(dateline,?) WHERE provider=? AND subject=?",
        )
        .bind(timestamp)
        .bind(provider.value())
        .bind(&profile.subject)
        .execute(self.db.observe(&mut *tx))
        .await?;
        if settings.login.external_two_factor == "require"
            && self.has_two_factor_locked(&mut tx, uid).await?
        {
            tx.commit().await?;
            return Ok(ExternalLoginOutcome::Login(LoginOutcome::SecondFactor(
                self.pending_login(&member, timestamp)?,
            )));
        }
        let grant = self
            .finish_login(
                &mut tx,
                member,
                metadata,
                Some(provider.value()),
                None,
                timestamp,
                &settings.account,
            )
            .await?;
        tx.commit().await?;
        Ok(ExternalLoginOutcome::Login(LoginOutcome::Session(
            Box::new(grant),
        )))
    }

    /// "Finish signing up" for third-party sign-up: the member picks a username and enters an invite code under
    /// invite-only registration; an e-mail address verified by the provider is used directly and counts as verified;
    /// without one, an e-mail address is optional (if entered, it goes through the usual verification flow). No
    /// password is set.
    pub async fn register_with_external(
        &self,
        token: &str,
        username: &str,
        email: &str,
        invite_code: &str,
        metadata: &SessionMetadata,
        consent: Option<PolicyConsent>,
    ) -> Result<SessionGrant> {
        let pending =
            self.open_pending(token, FlowPurpose::Login, 0)
                .ok_or(IdentityError::Validation(
                    crate::i18n::current().account_error_external_login_expired(),
                ))?;
        let username = normalize_username(username)?;
        let (email, email_verified) = match &pending.email {
            Some(verified) => (Some(normalize_email(verified)?), true),
            None if email.trim().is_empty() => (None, false),
            None => (Some(normalize_email(email)?), false),
        };
        let invite_code = invite_code.trim();
        if invite_code.len() > 64 {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_invite_invalid(),
            ));
        }
        registration_open(&*self.settings().await?, invite_code)?;
        let method = pending.provider.value();
        self.create_account(NewAccount {
            username,
            email,
            email_verified,
            password: None,
            mobile: None,
            external: Some(NewLink {
                provider: pending.provider,
                subject: pending.subject,
                display: pending.display,
            }),
            invite_code,
            metadata,
            consent,
            method,
        })
        .await
    }

    /// A signed-in member returns from authorization: if the identity is not linked and the current account has no link
    /// for this provider yet, issues a pending link (the current password must still be entered).
    pub async fn prepare_link(
        &self,
        member: &Member,
        provider: Provider,
        profile: Profile,
    ) -> Result<String> {
        if member.email_unverified() {
            return Err(verify_email_first());
        }
        let bound: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=?",
        )
        .bind(provider.value())
        .bind(&profile.subject)
        .fetch_optional(self.db.executor())
        .await?;
        match bound {
            Some(uid) if uid == member.uid => {
                return Err(IdentityError::Conflict(match provider {
                    Provider::Google => crate::i18n::current().account_error_google_linked_here(),
                    Provider::Github => crate::i18n::current().account_error_github_linked_here(),
                }));
            }
            Some(_) => return Err(IdentityError::Conflict(duplicate(provider))),
            None => {}
        }
        let own: Option<String> = crate::db::query_scalar(
            "SELECT subject FROM gonof_common_member_external WHERE uid=? AND provider=?",
        )
        .bind(member.uid)
        .bind(provider.value())
        .fetch_optional(self.db.executor())
        .await?;
        if own.is_some() {
            return Err(IdentityError::Conflict(already(provider)));
        }
        self.seal_pending(
            FlowPurpose::Link,
            Some(member),
            provider,
            profile,
            PENDING_TTL,
        )
    }

    /// Confirms a link: the current password is correct, the pending link still exists, and neither the identity nor
    /// this provider is taken.
    pub async fn link_external(
        &self,
        raw_cookie: &str,
        current_password: &str,
        token: &str,
    ) -> Result<Provider> {
        let verified = self
            .check_current_password(raw_cookie, current_password)
            .await?;
        let pending = self
            .open_pending(token, FlowPurpose::Link, verified.claims.uid)
            .ok_or(IdentityError::Validation(
                crate::i18n::current().account_error_link_expired(),
            ))?;
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } =
            self.security_account_locked(&mut tx, raw_cookie).await?;
        if member.credentials_version != verified.claims.credentials_version
            || member.credentials_version != pending.credentials_version
            || password != verified.previous
        {
            return Err(IdentityError::Unauthorized);
        }
        if member.email_unverified() {
            return Err(verify_email_first());
        }
        let own: Option<String> = crate::db::query_scalar(
            "SELECT subject FROM gonof_common_member_external WHERE uid=? AND provider=? FOR UPDATE",
        )
        .bind(member.uid)
        .bind(pending.provider.value())
        .fetch_optional(self.db.observe(&mut *tx))
        .await?;
        if own.is_some() {
            return Err(IdentityError::Conflict(already(pending.provider)));
        }
        let link = NewLink {
            provider: pending.provider,
            subject: pending.subject,
            display: pending.display,
        };
        check_unlinked(&self.db, &mut tx, &link).await?;
        insert_link(&self.db, &mut tx, member.uid, &link, timestamp).await?;
        let log_id = self
            .audit(
                &mut tx,
                member.uid,
                LINK_LOG,
                serde_json::json!({"provider": link.provider.value(), "display": link.display}),
                timestamp,
            )
            .await?;
        two_factor::enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        Ok(link.provider)
    }

    /// Unlinks: requires the current password (so the account certainly has a password and the last login method cannot
    /// be removed); after unlinking there must still be a way to recover the password (e-mail, phone number, another
    /// third-party link, or being the site owner), otherwise it is refused.
    pub async fn unlink_external(
        &self,
        raw_cookie: &str,
        current_password: &str,
        provider: Provider,
    ) -> Result<()> {
        let verified = self
            .check_current_password(raw_cookie, current_password)
            .await?;
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } =
            self.security_account_locked(&mut tx, raw_cookie).await?;
        if member.credentials_version != verified.claims.credentials_version
            || password != verified.previous
        {
            return Err(IdentityError::Unauthorized);
        }
        let rows = crate::db::query(
            "SELECT provider,subject,display,dateline,last_login_at FROM gonof_common_member_external WHERE uid=? FOR UPDATE",
        )
        .bind(member.uid)
        .fetch_all(self.db.observe(&mut *tx))
        .await?;
        let links = rows.iter().map(link_from_row).collect::<Result<Vec<_>>>()?;
        let Some(target) = links.iter().find(|link| link.provider == provider) else {
            return Err(IdentityError::Conflict(match provider {
                Provider::Google => crate::i18n::current().account_error_google_not_linked(),
                Provider::Github => crate::i18n::current().account_error_github_not_linked(),
            }));
        };
        let allowed = rules::Credentials {
            password: password.as_deref(),
            email: member.email.as_deref(),
            mobile: member.mobile.as_deref(),
            external: links.iter().any(|link| link.provider != provider),
            emailstatus: i64::from(member.email_verified),
            adminid: member.adminid,
        }
        .check()
        .is_ok();
        if !allowed {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_unlink_last(),
            ));
        }
        crate::db::query("DELETE FROM gonof_common_member_external WHERE uid=? AND provider=?")
            .bind(member.uid)
            .bind(provider.value())
            .execute(self.db.observe(&mut *tx))
            .await?;
        let log_id = self
            .audit(
                &mut tx,
                member.uid,
                UNLINK_LOG,
                serde_json::json!({"provider": provider.value(), "display": target.display}),
                timestamp,
            )
            .await?;
        two_factor::enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Password recovery claims an account whose email was never verified ([`Identity::reset_password`]): every link
    /// goes, since whoever knew the password before may have linked their own account. Each removal is logged like an
    /// unlink, with the cause. The caller holds the member row lock.
    pub(super) async fn remove_links_on_claim(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        cause: &str,
        timestamp: i64,
    ) -> Result<()> {
        let rows = crate::db::query(
            "SELECT provider,subject,display,dateline,last_login_at FROM gonof_common_member_external WHERE uid=? FOR UPDATE",
        )
        .bind(uid)
        .fetch_all(self.db.observe(&mut **tx))
        .await?;
        let links = rows.iter().map(link_from_row).collect::<Result<Vec<_>>>()?;
        if links.is_empty() {
            return Ok(());
        }
        crate::db::query("DELETE FROM gonof_common_member_external WHERE uid=?")
            .bind(uid)
            .execute(self.db.observe(&mut **tx))
            .await?;
        for link in links {
            self.audit(
                tx,
                uid,
                UNLINK_LOG,
                serde_json::json!({"provider": link.provider.value(), "display": link.display, "cause": cause}),
                timestamp,
            )
            .await?;
        }
        Ok(())
    }

    /// An account without a password confirms identity with a linked third party: the identity returned by
    /// authorization must be exactly the one linked to this member.
    pub async fn confirm_external(
        &self,
        member: &Member,
        provider: Provider,
        profile: Profile,
    ) -> Result<String> {
        let bound: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=?",
        )
        .bind(provider.value())
        .bind(&profile.subject)
        .fetch_optional(self.db.executor())
        .await?;
        if bound != Some(member.uid) {
            return Err(IdentityError::Conflict(match provider {
                Provider::Google => crate::i18n::current().account_error_google_other(),
                Provider::Github => crate::i18n::current().account_error_github_other(),
            }));
        }
        self.seal_pending(
            FlowPurpose::Confirm,
            Some(member),
            provider,
            profile,
            CONFIRM_TTL,
        )
    }

    /// Sets a password after confirming identity with a third party: afterwards the credentials version increments and
    /// every device is signed out, as with a password change.
    pub async fn set_password_with_external(
        &self,
        raw_cookie: &str,
        token: &str,
        new_password: &str,
    ) -> Result<()> {
        validate_password(
            new_password,
            self.settings().await?.account.password_min_length,
        )?;
        let claims = self.cookie_claims(raw_cookie)?;
        let pending = self
            .open_pending(token, FlowPurpose::Confirm, claims.uid)
            .ok_or(IdentityError::Validation(
                crate::i18n::current().account_error_confirm_expired(),
            ))?;
        let encoded = hash_password(new_password.to_owned()).await?;
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let LockedAccount { member, password } =
            self.security_account_locked(&mut tx, raw_cookie).await?;
        if member.credentials_version != pending.credentials_version {
            return Err(IdentityError::Unauthorized);
        }
        if password.is_some() {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_password_exists(),
            ));
        }
        let bound: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_external WHERE provider=? AND subject=? FOR UPDATE",
        )
        .bind(pending.provider.value())
        .bind(&pending.subject)
        .fetch_optional(self.db.observe(&mut *tx))
        .await?;
        if bound != Some(member.uid) {
            return Err(IdentityError::Unauthorized);
        }
        rules::Credentials {
            password: Some(&encoded),
            email: member.email.as_deref(),
            mobile: member.mobile.as_deref(),
            external: true,
            emailstatus: i64::from(member.email_verified),
            adminid: member.adminid,
        }
        .check()?;
        let log_id = self
            .replace_password(
                &mut tx,
                &member,
                &encoded,
                timestamp,
                phone::PASSWORD_SET_LOG,
            )
            .await?;
        two_factor::enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Saves Google or GitHub credentials from the admin area (the Client Secret is encrypted with the private
    /// configuration's `[common] encryption_key`).
    pub async fn save_oauth_provider(
        &self,
        actor: Actor,
        input: crate::sms::OAuthInput,
    ) -> Result<crate::sms::Providers> {
        crate::sms::save_oauth(&self.db, &self.common, actor, input).await
    }
}
