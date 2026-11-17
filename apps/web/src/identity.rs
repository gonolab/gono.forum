//! Account writes happen in primary-database transactions; invite codes are always stored verbatim, credentials only
//! as digests.
use crate::db::DbRow;
use crate::db::Tx;
use std::{
    fmt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::{Rng, RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use unicode_normalization::UnicodeNormalization;

use crate::db::{self, Database};

mod api_key;
mod cookie;
pub mod external;
pub(crate) mod login_alert;
mod online;
pub use online::ONLINE_WINDOW;
pub mod phone;
mod profile;
mod reveal;
pub mod rules;
mod security;
mod skin_preview;
pub use skin_preview::{LayoutDraft, PREVIEW_TTL as SKIN_PREVIEW_TTL, SkinPreview};
pub(crate) mod two_factor;
pub use api_key::{API_KEY_PREFIX, ApiKey, ApiKeyGrant, valid_api_secret};
pub use cookie::CookieConfig;
pub use login_alert::{MARKER_MAX_AGE as DEVICE_MARKER_MAX_AGE, device_summary};
pub use profile::{ProfileSettings, SpaceVisibility};
pub use reveal::{SECRET_REVEAL_TTL, SecretReveal};
pub(crate) use security::VerifiedPassword;
pub use security::{ResetPreview, SessionMetadata};
pub use two_factor::{
    LoginOutcome, PENDING_TTL as SECOND_FACTOR_PENDING_TTL, PendingLogin, RECOVERY_CODE_COUNT,
    REVEAL_TTL as RECOVERY_REVEAL_TTL, RekeyReport, SecondFactorOutcome, TotpSetup,
    TwoFactorConfig, TwoFactorDisable, TwoFactorEnabled, TwoFactorStatus, display_recovery_code,
    totp_code_for_test,
};

/// Versions of the terms agreed to at registration: the `updated_at` (last admin-area save time) of the terms of use
/// and of the privacy notice; 0 means the built-in text was in use then. Terms are overwritten without history, so the
/// version only tells which save moment the agreed version came from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PolicyConsent {
    pub terms_version: i64,
    pub privacy_version: i64,
}

/// A new account ([`Identity::create_account`]): username, email and mobile number already normalized, password
/// already hashed.
pub(crate) struct NewAccount<'a> {
    username: String,
    email: Option<String>,
    /// The email was verified by the third-party provider: recorded as verified directly, and no verification email
    /// is sent.
    email_verified: bool,
    password: Option<String>,
    mobile: Option<String>,
    /// The link written in the same transaction on third-party registration.
    external: Option<external::NewLink>,
    invite_code: &'a str,
    metadata: &'a SessionMetadata,
    consent: Option<PolicyConsent>,
    /// Registration method, written to the registration log: `password` / `sms` / `google` / `github`.
    method: &'static str,
}

type Result<T> = std::result::Result<T, IdentityError>;
const BOOTSTRAP_KEY: &str = "identity.bootstrap_admin";
const MEMBER_COLUMNS: &str = "uid, username, avatar, avatar_aid, email, mobile,
    emailstatus, adminid, gonof_common_member.freeze, mute_until, credentials_version, unread_notifications, account_state";

/// Roles (`gonof_common_member.adminid`, docs/roles.md, "Staff roles").
pub const ROLE_MEMBER: i64 = 0;
pub const ROLE_ADMIN: i64 = 1;
pub const ROLE_SUPER_MODERATOR: i64 = 2;
pub const ROLE_MODERATOR: i64 = 3;
/// Trainee moderator: appointed to forums (`gonof_forum_moderator`; an appointment on a parent forum covers its child
/// forums), where they can only hide and unhide, lock and unlock threads, handle thread and reply reports, and accept
/// restoration requests that only need unhiding.
pub const ROLE_INTERN: i64 = 4;
/// Reviewer: site-wide, handles reports only (for thread and reply reports they can hide and unhide the reported item;
/// profile and attachment reports can only be marked handled or rejected).
pub const ROLE_REVIEWER: i64 = 5;

/// The translated label of a role, in the current language.
pub fn role_label(adminid: i64) -> &'static str {
    match adminid {
        ROLE_ADMIN => crate::i18n::current().common_role_admin(),
        ROLE_SUPER_MODERATOR => crate::i18n::current().common_role_super_moderator(),
        ROLE_MODERATOR => crate::i18n::current().common_role_moderator(),
        ROLE_INTERN => crate::i18n::current().common_role_intern(),
        ROLE_REVIEWER => crate::i18n::current().common_role_reviewer(),
        _ => crate::i18n::current().common_role_member(),
    }
}

/// Roles appointed to forums: moderators and trainee moderators, each with rows in `gonof_forum_moderator`.
pub fn forum_role(adminid: i64) -> bool {
    matches!(adminid, ROLE_MODERATOR | ROLE_INTERN)
}
const INVITE_COLUMNS: &str =
    "id, uid, code, request_key, fuid, dateline, endtime, regdateline, status";

/// Deployment config `[identity]`: only the local private directory and two key sets remain. Invitations, renaming,
/// session lifetimes, link validity periods and account-security rate limits are all admin-area settings
/// (docs/settings-and-caching.md, "Where settings live").
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityConfig {
    /// Local private directory (mail, setup log, data exports); may be a path relative to the project root.
    pub local_directory: PathBuf,
    pub cookie: CookieConfig,
    /// Keys that encrypt members' authenticator secrets (private config `[identity.two_factor]`), separate from the
    /// cookie keys.
    pub two_factor: TwoFactorConfig,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            local_directory: PathBuf::from(".local"),
            cookie: CookieConfig::default(),
            two_factor: TwoFactorConfig::default(),
        }
    }
}

impl IdentityConfig {
    /// Whether `key` is one of the cookie or two-factor keys (`[common] encryption_key` must not equal any of them).
    pub(crate) fn uses_key(&self, key: &str) -> bool {
        std::iter::once(&self.cookie.active_key)
            .chain(&self.cookie.previous_keys)
            .chain(self.two_factor.keys())
            .any(|candidate| candidate.eq_ignore_ascii_case(key))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.local_directory.as_os_str().is_empty(),
            "the identity local directory must not be empty"
        );
        self.cookie.validate()?;
        self.two_factor.validate()?;
        // The two key sets differ in purpose and rotation pace: at most 3 old cookie keys are kept and are gone once
        // deleted, while authenticator keys must stay decryptable long-term.
        let cookie_keys =
            std::iter::once(&self.cookie.active_key).chain(&self.cookie.previous_keys);
        for cookie_key in cookie_keys {
            anyhow::ensure!(
                !self
                    .two_factor
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case(cookie_key)),
                "{}",
                crate::i18n::current().account_error_two_factor_key_shared()
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Identity {
    db: Database,
    config: IdentityConfig,
    root: PathBuf,
    online: std::sync::Arc<online::OnlineState>,
    /// In-process global cache: session lifetimes, link validity periods and the registration switch are read from here
    /// (the authentication path issues no extra queries to read settings, docs/settings-and-caching.md, "Reading
    /// settings"); decisions inside write transactions still use the copy read by the write gate.
    global: crate::cache::GlobalCache,
    /// Private config `[common]`: decrypts the SMS provider and third-party login secrets saved in the admin area
    /// ([`Self::with_common`]; empty when not given, which allows only local delivery).
    common: crate::secrets::CommonConfig,
    /// Google and GitHub endpoints ([`Self::with_oauth`]; tests swap in local fake services).
    oauth: crate::oauth::Endpoints,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub account_state: String,
    pub uid: i64,
    pub username: String,
    pub avatar: i64,
    pub avatar_aid: Option<i64>,
    pub email: Option<String>,
    /// Bound mobile number (E.164); only written via an SMS code, so if present it is verified.
    pub mobile: Option<String>,
    /// Linked third-party accounts (`gonof_common_member_external`). Only read when this request's participation
    /// requirements include google/github and neither the email nor the mobile number satisfies them
    /// ([`Identity::fill_links`]); otherwise all false. Used only by [`Self::can_contribute`].
    pub linked: crate::login::Linked,
    pub email_verified: bool,
    pub is_admin: bool,
    pub adminid: i64,
    pub frozen: bool,
    /// Mute end time (`mute_until`; 0 means not muted), see [`crate::moderation::muted`].
    pub mute_until: i64,
    pub credentials_version: i64,
    pub unread_notifications: i64,
}

/// Raw fields read by the email-link precheck. For the password only "whether it is set" is kept, not the hash.
struct TokenPreview {
    token_email: String,
    dateline: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
    revoked_at: Option<i64>,
    member_email: Option<String>,
    email_verified: bool,
    frozen: bool,
    active: bool,
    has_password: bool,
}

impl TokenPreview {
    /// Not consumed, not revoked and not expired.
    fn unused_before(&self, timestamp: i64) -> bool {
        self.consumed_at.is_none() && self.revoked_at.is_none() && self.expires_at > timestamp
    }

    /// Beyond unused_before, also requires the issue time to have been reached; used for password recovery and email
    /// change.
    fn usable_at(&self, timestamp: i64) -> bool {
        self.dateline <= timestamp && self.unused_before(timestamp)
    }

    /// A normal account that can sign in with a password: not deleted, not frozen, password set.
    fn open_account(&self) -> bool {
        self.active && !self.frozen && self.has_password
    }
}

// Only sensitive account operations read this projection; never derive Debug for a password hash.
struct LockedAccount {
    member: Member,
    password: Option<String>,
}

enum MemberAuthentication<'a> {
    TrustedUid(i64),
    Cookie(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    pub uid: i64,
    pub credentials_version: i64,
    /// The token ID when authenticated by a member API token, `None` for a web cookie. After a write transaction locks
    /// the member row, [`api_key_locked`] rechecks that it is still valid.
    pub api_key: Option<i64>,
}

/// Request keys for forms and the API: 64 lowercase hex characters. Server-generated keys (`random_token`,
/// `token_hash`) all have this form, and only lowercase is accepted (checked by each entry point, the request write
/// entry of `identity::rules`, and the check scripts); the columns use binary collation, so the same key in a
/// different case is a different key, and entry points must not differ in whether they accept uppercase.
pub fn valid_request_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Rechecks the API token inside a write transaction: passes directly when there is no token (web cookie).
///
/// The caller must already hold the lock on the member row. Disabling or rotating by the member, revocation by an
/// administrator, and password recovery all lock the member row before changing the token, and this uses a current
/// read rather than a snapshot read, so it serializes with them: once a revocation commits, queued writes holding the
/// old token no longer commit. Expiry is judged by application time, consistent with `authenticate_api_key`.
pub async fn api_key_locked(db: &Database, tx: &mut Tx<'_>, actor: Actor) -> anyhow::Result<bool> {
    let Some(id) = actor.api_key else {
        return Ok(true);
    };
    let row = crate::db::query(
        "SELECT uid, expires_at, revoked_at FROM gonof_common_member_api_key WHERE id = ? LOCK IN SHARE MODE",
    )
    .bind(id)
    .fetch_optional(db.observe(&mut **tx))
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(row.try_get::<i64, _>("uid")? == actor.uid
        && row.try_get::<Option<i64>, _>("revoked_at")?.is_none()
        && row.try_get::<i64, _>("expires_at")? > now())
}

/// Read-only helpers must not be called while holding a transaction on a one-connection pool.
pub async fn valid_actor(db: &Database, actor: Actor) -> anyhow::Result<bool> {
    let row = crate::db::query(
        "SELECT credentials_version, `freeze`, account_state FROM gonof_common_member WHERE uid = ?",
    )
    .bind(actor.uid)
    .fetch_optional(db.executor())
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(
        row.try_get::<i64, _>("credentials_version")? == actor.credentials_version
            && row.try_get::<i64, _>("freeze")? == 0
            && db::text(&row, "account_state")? == "active",
    )
}

/// A moderator manages the forums they are appointed to and their child forums. A read-only check
/// (management links on pages, moderators downloading attachments in hidden or withdrawn posts): role, freeze and
/// account status are values the caller read fresh for this request, while appointments come from the in-process
/// moderator roster (docs/queries.md, "Process caches"), so after an appointment is revoked it may still count as
/// managing for up to one cache lifetime. Actual management operations still recheck with a locking read of the
/// appointment row inside the write transaction ([`crate::organization::moderator_locked`]).
pub fn manages_forum(
    adminid: i64,
    active: bool,
    roster: &[crate::organization::RosterEntry],
    (uid, fid, parent): (i64, i64, Option<i64>),
) -> bool {
    active
        && (matches!(adminid, 1 | 2)
            || (forum_role(adminid)
                && roster.iter().any(|entry| {
                    entry.uid == uid && (entry.fid == fid || Some(entry.fid) == parent)
                })))
}

/// Same as [`manages_forum`], but everything is read fresh without the in-process roster (for tests and callers with
/// no cache handle): the member row, the forum's parent forum, and the appointment rows on this forum and its parent,
/// each a single-table primary-key query.
pub async fn can_manage_forum(db: &Database, uid: i64, fid: i64) -> anyhow::Result<bool> {
    let Some(row) = crate::db::query(
        "SELECT adminid,`freeze`,account_state FROM gonof_common_member WHERE uid=?",
    )
    .bind(uid)
    .fetch_optional(db.executor())
    .await?
    else {
        return Ok(false);
    };
    let adminid: i64 = row.try_get("adminid")?;
    let active =
        row.try_get::<i64, _>("freeze")? == 0 && db::text(&row, "account_state")? == "active";
    if !active || !forum_role(adminid) {
        return Ok(manages_forum(adminid, active, &[], (uid, fid, None)));
    }
    let Some(parent) = crate::db::query_scalar::<Option<i64>>(
        "SELECT parent_fid FROM gonof_forum_forum WHERE fid=?",
    )
    .bind(fid)
    .fetch_optional(db.executor())
    .await?
    else {
        return Ok(false);
    };
    Ok(crate::db::query_scalar::<i64>(
        "SELECT fid FROM gonof_forum_moderator WHERE uid=? AND fid IN (?,?) LIMIT 1",
    )
    .bind(uid)
    .bind(fid)
    .bind(parent.unwrap_or(fid))
    .fetch_optional(db.executor())
    .await?
    .is_some())
}

#[derive(Clone, Debug)]
pub struct RoleAssignment {
    pub target_uid: i64,
    pub adminid: i64,
    pub forum_ids: Vec<i64>,
    pub expected_version: i64,
    pub request_key: String,
    pub reason: String,
}

impl Member {
    pub fn active(&self) -> bool {
        self.account_state == "active"
    }
    pub fn actor(&self) -> Actor {
        Actor {
            uid: self.uid,
            credentials_version: self.credentials_version,
            api_key: None,
        }
    }

    /// Staff: the five roles that can enter the admin area. Each page narrows further by role (reviewers only see
    /// reports, trainee moderators do not handle restoring withdrawn content, etc.).
    pub fn is_staff(&self) -> bool {
        self.active() && !self.frozen && matches!(self.adminid, 1..=5)
    }

    /// Roles that manage content (administrator, global moderator, moderator, trainee moderator): content management,
    /// the operation log and restoration requests. Reviewers are not included.
    pub fn manages_content(&self) -> bool {
        self.is_staff() && self.adminid != ROLE_REVIEWER
    }

    /// Has an email address nobody has proven yet. Until it is verified the account cannot gain another way to sign
    /// in (a linked Google or GitHub account, a phone number) or two-step verification: whoever signed up may not own
    /// the address, and its owner can still take the account over through password recovery
    /// ([`Identity::reset_password`]). Accounts without an email are not affected.
    pub fn email_unverified(&self) -> bool {
        self.email.as_deref().is_some_and(|email| !email.is_empty()) && !self.email_verified
    }

    /// Whether the member is muted right now.
    pub fn muted(&self) -> bool {
        crate::moderation::muted(self.mute_until, now())
    }

    pub fn role_label(&self) -> &'static str {
        role_label(self.adminid)
    }

    /// Can participate (threads, replies, private messages, interactions, uploads, invite codes, API tokens): the
    /// account is normal, and the member is an administrator or meets the participation requirements in effect for
    /// this request ([`crate::login::Participation::current`]).
    pub fn can_contribute(&self) -> bool {
        self.active()
            && !self.frozen
            && (self.is_admin || crate::login::Participation::current().met_by(self))
    }
}

#[derive(Clone)]
pub struct AuthSession {
    pub member: Member,
    pub csrf_hash: String,
    pub csrf_token: String,
    pub session_token: String,
    pub renewed_cookie: Option<String>,
    pub online_id: String,
    pub expires_at: i64,
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSession")
            .field("member", &self.member)
            .field("credentials", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

// Do not derive Debug: a session grant contains raw credentials.
pub struct SessionGrant {
    pub member: Member,
    pub session_token: String,
    pub csrf_token: String,
    pub expires_at: i64,
    /// The new encrypted device marker on web login and registration (see `identity::login_alert`), written to a
    /// cookie by the router; `None` for calls not made through the web (tests, CLI).
    pub device_marker: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Invite {
    pub id: i64,
    pub uid: i64,
    pub code: String,
    pub request_key: String,
    pub fuid: Option<i64>,
    pub dateline: i64,
    pub endtime: i64,
    pub regdateline: Option<i64>,
    pub status: String,
}

pub const INVITES_PAGE_SIZE: usize = 20;

pub struct InvitePage {
    pub rows: Vec<Invite>,
    pub total: i64,
    pub today_generated: i64,
    pub active: i64,
    pub timestamp: i64,
    pub has_next: bool,
}

impl Invite {
    pub fn is_available(&self) -> bool {
        self.status == "available" && self.endtime > now()
    }
}

#[derive(Debug)]
pub enum IdentityError {
    Validation(&'static str),
    Unauthorized,
    Suspended(String),
    Forbidden,
    NotFound,
    Conflict(&'static str),
    /// A refusal (403) whose text is computed from the current settings, such as the new-member external link block
    /// on bios, which states the observation period in days.
    Restricted(String),
    Internal(anyhow::Error),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Conflict(message) => f.write_str(message),
            Self::Suspended(message) | Self::Restricted(message) => f.write_str(message),
            Self::Unauthorized => f.write_str(crate::i18n::current().account_error_unauthorized()),
            Self::Forbidden => f.write_str(crate::i18n::current().account_error_verify_first()),
            Self::NotFound => f.write_str(crate::i18n::current().account_error_no_record()),
            Self::Internal(error) => write!(f, "identity service error: {error}"),
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<anyhow::Error> for IdentityError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<sqlx::Error> for IdentityError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .is_some_and(|error| error.is_unique_violation())
        {
            Self::Conflict(crate::i18n::current().account_error_taken())
        } else {
            Self::Internal(error.into())
        }
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs() as i64
}

pub fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

pub fn token_hash(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

pub fn check_csrf(raw: &str, session: &AuthSession) -> bool {
    bool::from(
        token_hash(raw)
            .as_bytes()
            .ct_eq(session.csrf_hash.as_bytes()),
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 15)] as char);
    }
    output
}

fn normalized(value: &str) -> String {
    value.trim().nfkc().collect::<String>().to_lowercase()
}

/// Usernames in the search box get the same normalization as stored usernames (trim, NFKC, lowercase), but no length
/// or character validation: the search term is only a prefix, and an invalid spelling simply finds nothing.
pub fn normalized_username_query(value: &str) -> String {
    normalized(value)
}

/// The key for rate limiting by username or email: the same normalization as before the database lookup in login and
/// password recovery, so full-width, case and other variants land on the same key; then digested, so the key has a
/// fixed length that does not grow with the submitted content.
pub fn identifier_limit_key(value: &str) -> String {
    token_hash(&normalized(value))
}

pub(crate) fn normalize_username(value: &str) -> Result<String> {
    let value = normalized(value);
    if value.is_empty()
        || value.chars().count() > 50
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '@' | '/' | '\\'))
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_error_username(),
        ));
    }
    Ok(value)
}

pub(crate) fn normalize_email(value: &str) -> Result<String> {
    let value = normalized(value);
    let valid = value.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty()
            && !domain.contains('@')
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
    });
    if !valid
        || value.chars().count() > 255
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_error_email(),
        ));
    }
    Ok(value)
}

/// Upper limit for new passwords, a constant; the lower limit is the admin-area setting
/// `settings.account.password_min_length` (8–64).
pub const PASSWORD_MAX_CHARS: usize = 128;

/// Validates new passwords only: raising the lower limit does not lock out existing passwords, and login and every
/// "current password" check only enforce the upper limit.
fn validate_password(password: &str, min_length: i64) -> Result<()> {
    let min = usize::try_from(min_length).unwrap_or(PASSWORD_MAX_CHARS);
    if !(min..=PASSWORD_MAX_CHARS).contains(&password.chars().count()) || password.len() > 512 {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_error_password_length(),
        ));
    }
    Ok(())
}

/// Validates the administrator the setup wizard is about to create, with the registration rules (username, optional
/// e-mail, the site's password minimum). Returns the normalized username and e-mail.
pub fn check_new_admin(
    username: &str,
    email: Option<&str>,
    password: &str,
    min_length: i64,
) -> Result<(String, Option<String>)> {
    let username = normalize_username(username)?;
    let email = email
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(normalize_email)
        .transpose()?;
    validate_password(password, min_length)?;
    Ok((username, email))
}

/// Upper limit on concurrently running Argon2 computations: half the CPU cores, at least 1 and at most 4.
///
/// With the default parameters (Argon2id, m = 19 MiB, t = 2, p = 1) each computation takes 19 MiB of memory and one
/// core. Without a limit, a wave of concurrent logins or registrations would fill tokio's blocking thread pool (512
/// threads by default), memory could exceed 9 GiB, and every core would be busy.
///
/// - Half the cores: hashing is pure computation, so concurrency beyond the idle cores does not finish sooner; it
///   only takes more memory and steals cores from page requests. On the test site MySQL and Redis share the machine
///   with the application and need cores too.
/// - Capped at 4: peak memory about 76 MiB. A local release build takes 40–65 ms per computation under full load, so
///   4 lanes still complete more than sixty per second, far above a forum's normal login and registration volume.
///
/// Requests beyond the limit queue in order without an error; what queues is only a very small future.
fn password_permits() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .div_ceil(2)
        .clamp(1, 4)
}

static PASSWORD_WORK: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(password_permits()));

/// Runs one Argon2 computation on a blocking thread, subject to the concurrency limit of [`PASSWORD_WORK`].
///
/// The permit moves into the blocking thread with the closure and is released only when the computation ends. When
/// the caller's future is dropped (e.g. the client disconnects), a computation already started still runs to the end
/// and still holds its memory, so the permit must stay held too; otherwise the limit would be meaningless.
async fn password_work<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> std::result::Result<T, tokio::task::JoinError> {
    let permit = PASSWORD_WORK
        .acquire()
        .await
        .expect("the password work semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

async fn hash_password(password: String) -> Result<String> {
    password_work(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| {
                IdentityError::Internal(anyhow::anyhow!("failed to hash the password: {error}"))
            })
    })
    .await
    .context("the password hashing task failed")?
}

pub(crate) async fn verify_password(password: String, encoded: Option<String>) -> Result<bool> {
    password_work(move || {
        if let Some(encoded) = encoded {
            let hash = PasswordHash::new(&encoded).map_err(|error| {
                IdentityError::Internal(anyhow::anyhow!("malformed password hash: {error}"))
            })?;
            Ok(Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok())
        } else {
            // Missing accounts still incur a full password operation.
            let salt = SaltString::generate(&mut OsRng);
            let _ = Argon2::default().hash_password(password.as_bytes(), &salt);
            Ok(false)
        }
    })
    .await
    .context("the password check task failed")?
}

/// Whether the site switches allow this registration.
fn registration_open(settings: &crate::runtime::SiteSettings, invite_code: &str) -> Result<()> {
    if settings.site.read_only {
        return Err(IdentityError::Suspended(
            settings.site.read_only_notice(crate::i18n::current()),
        ));
    }
    if settings.signup.mode == "closed" {
        return Err(IdentityError::Conflict(
            crate::i18n::current().account_error_registration_closed(),
        ));
    }
    if settings.signup.mode == "invite" && invite_code.is_empty() {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_error_invite_required(),
        ));
    }
    Ok(())
}

fn member_from_row(row: &DbRow) -> Result<Member> {
    Ok(Member {
        account_state: db::text(row, "account_state")?,
        uid: row.try_get("uid")?,
        username: db::text(row, "username")?,
        avatar: row.try_get("avatar")?,
        avatar_aid: row.try_get("avatar_aid")?,
        email: row.try_get("email")?,
        mobile: row.try_get("mobile")?,
        linked: crate::login::Linked::default(),
        email_verified: row.try_get::<i64, _>("emailstatus")? == 1,
        is_admin: row.try_get::<i64, _>("adminid")? == 1,
        adminid: row.try_get("adminid")?,
        frozen: row.try_get::<i64, _>("freeze")? != 0,
        mute_until: row.try_get("mute_until")?,
        credentials_version: row.try_get("credentials_version")?,
        unread_notifications: row.try_get("unread_notifications")?,
    })
}

fn invite_from_row(row: &DbRow) -> Result<Invite> {
    Ok(Invite {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        code: db::text(row, "code")?,
        request_key: db::text(row, "request_key")?,
        fuid: row.try_get("fuid")?,
        dateline: row.try_get("dateline")?,
        endtime: row.try_get("endtime")?,
        regdateline: row.try_get("regdateline")?,
        status: db::text(row, "status")?,
    })
}

impl Identity {
    pub fn private_directory(&self) -> PathBuf {
        self.root.join(&self.config.local_directory)
    }
    pub fn new(
        db: Database,
        config: IdentityConfig,
        root: PathBuf,
        global: crate::cache::GlobalCache,
    ) -> Self {
        Self {
            db,
            config,
            root,
            online: std::sync::Arc::new(online::OnlineState::default()),
            global,
            common: crate::secrets::CommonConfig::default(),
            oauth: crate::oauth::Endpoints::default(),
        }
    }

    /// Attaches the private config `[common]` (given at application start; tests and the command line may omit it
    /// when they send no real SMS).
    pub fn with_common(mut self, common: crate::secrets::CommonConfig) -> Self {
        self.common = common;
        self
    }

    /// Replaces the Google and GitHub endpoints (taken at application start from the deployment config's
    /// [`crate::config::Config::oauth`]; production uses the defaults).
    pub fn with_oauth(mut self, endpoints: crate::oauth::Endpoints) -> Self {
        self.oauth = endpoints;
        self
    }

    /// Site settings, taken from the in-process global cache (the same block as the page frame). Only for display,
    /// issuance lifetimes and entry prechecks.
    pub(crate) async fn settings(&self) -> Result<std::sync::Arc<crate::runtime::SiteSettings>> {
        Ok(self.global.settings(&self.db).await?.settings.clone())
    }

    pub async fn assign_role(&self, actor: Actor, mut input: RoleAssignment) -> Result<i64> {
        input.reason = input.reason.trim().to_owned();
        input.forum_ids.sort_unstable();
        input.forum_ids.dedup();
        if input.target_uid <= 0
            || !matches!(input.adminid, 0 | 2 | 3 | 4 | 5)
            || !(1..=500).contains(&input.reason.chars().count())
            || input.request_key.len() != 64
            || !input
                .request_key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || input.expected_version <= 0
            || input.forum_ids.iter().any(|fid| *fid <= 0)
            || forum_role(input.adminid) == input.forum_ids.is_empty()
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().forum_moderators_error_input(),
            ));
        }
        let hash = token_hash(
            &serde_json::json!({
                "operation": "identity.role.assign", "target_uid": input.target_uid,
                "adminid": input.adminid, "forum_ids": input.forum_ids,
                "expected_version": input.expected_version, "reason": input.reason,
            })
            .to_string(),
        );
        let mut tx = self.db.begin_write().await?;
        let mut ids = vec![actor.uid, input.target_uid];
        ids.sort_unstable();
        ids.dedup();
        let mut members = Vec::new();
        for uid in ids {
            members.push(self.member_locked(&mut tx, uid).await?);
        }
        let operator = members
            .iter()
            .find(|member| member.uid == actor.uid)
            .ok_or(IdentityError::Unauthorized)?;
        if !operator.active()
            || operator.frozen
            || operator.credentials_version != actor.credentials_version
        {
            return Err(IdentityError::Unauthorized);
        }
        if !operator.is_admin {
            return Err(IdentityError::Forbidden);
        }
        let existing = crate::db::query("SELECT request_hash, target_uid FROM gonof_common_member_role_request WHERE uid = ? AND request_key = ?")
            .bind(actor.uid).bind(&input.request_key).fetch_optional(self.db.observe(&mut *tx)).await?;
        if let Some(row) = existing {
            if db::text(&row, "request_hash")? != hash {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().forum_moderators_error_request(),
                ));
            }
            return Ok(row.try_get("target_uid")?);
        }
        let target = members
            .iter()
            .find(|member| member.uid == input.target_uid)
            .ok_or(IdentityError::NotFound)?;
        if !target.active() || target.is_admin {
            return Err(IdentityError::Validation(
                crate::i18n::current().forum_moderators_error_admin(),
            ));
        }
        if target.credentials_version != input.expected_version {
            return Err(IdentityError::Conflict(
                crate::i18n::current().forum_moderators_error_stale(),
            ));
        }
        let account: i64 = crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member WHERE uid = ? AND password IS NOT NULL AND email IS NOT NULL")
            .bind(input.target_uid).fetch_one(self.db.observe(&mut *tx)).await?;
        if account != 1 {
            return Err(IdentityError::Validation(
                crate::i18n::current().forum_moderators_error_account(),
            ));
        }
        for fid in &input.forum_ids {
            let exists: i64 =
                crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_forum WHERE fid = ?")
                    .bind(fid)
                    .fetch_one(self.db.observe(&mut *tx))
                    .await?;
            if exists != 1 {
                return Err(IdentityError::Validation(
                    crate::i18n::current().forum_moderators_error_forum(),
                ));
            }
        }
        let before_fids: Vec<i64> = crate::db::query_scalar(
            "SELECT fid FROM gonof_forum_moderator WHERE uid = ? ORDER BY fid",
        )
        .bind(input.target_uid)
        .fetch_all(self.db.observe(&mut *tx))
        .await?;
        let changed = target.adminid != input.adminid || before_fids != input.forum_ids;
        let timestamp = now();
        let version = if changed {
            target
                .credentials_version
                .checked_add(1)
                .ok_or(IdentityError::Conflict(
                    crate::i18n::current().forum_moderators_error_version(),
                ))?
        } else {
            target.credentials_version
        };
        if changed {
            crate::db::query("DELETE FROM gonof_forum_moderator WHERE uid = ?")
                .bind(input.target_uid)
                .execute(self.db.observe(&mut *tx))
                .await?;
            for fid in &input.forum_ids {
                crate::db::query("INSERT INTO gonof_forum_moderator (uid, fid) VALUES (?, ?)")
                    .bind(input.target_uid)
                    .bind(fid)
                    .execute(self.db.observe(&mut *tx))
                    .await?;
            }
            crate::db::query(
                "UPDATE gonof_common_member SET adminid = ?, credentials_version = ? WHERE uid = ?",
            )
            .bind(input.adminid)
            .bind(version)
            .bind(input.target_uid)
            .execute(self.db.observe(&mut *tx))
            .await?;
            crate::db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE uid=? AND purpose IN ('appeal_access','privacy_access') AND consumed_at IS NULL AND revoked_at IS NULL")
                .bind(timestamp).bind(input.target_uid).execute(self.db.observe(&mut *tx)).await?;
        }
        crate::db::query("INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline) VALUES (?, ?, 'identity.role.assign', ?, 'web', ?)")
            .bind(input.target_uid).bind(actor.uid)
            .bind(serde_json::json!({"target_uid":input.target_uid,"reason":input.reason,"before":{"adminid":target.adminid,"forum_ids":before_fids,"version":target.credentials_version},"after":{"adminid":input.adminid,"forum_ids":input.forum_ids,"version":version},"result":if changed {"changed"} else {"unchanged"}}).to_string())
            .bind(timestamp).execute(self.db.observe(&mut *tx)).await?;
        rules::check_role_request(&input.request_key, &hash)?;
        crate::db::query("INSERT INTO gonof_common_member_role_request (uid, request_key, request_hash, target_uid) VALUES (?, ?, ?, ?)")
            .bind(actor.uid).bind(&input.request_key).bind(hash).bind(input.target_uid).execute(self.db.observe(&mut *tx)).await?;
        tx.commit().await?;
        // The moderator roster is only for display (forum page header, admin forum list); authorization is queried
        // fresh and does not rely on it.
        self.global
            .announce(&[crate::cache::Block::Moderators])
            .await
            .map_err(IdentityError::Internal)?;
        Ok(input.target_uid)
    }

    /// Same as [`Self::member_locked`] without locking: for page reads in read-only transactions.
    async fn member_read(&self, transaction: &mut Tx<'_>, uid: i64) -> Result<Member> {
        let sql = &format!("SELECT {MEMBER_COLUMNS} FROM gonof_common_member WHERE uid = ?");
        let row = crate::db::query(sql)
            .bind(uid)
            .fetch_optional(self.db.observe(&mut **transaction))
            .await?
            .ok_or(IdentityError::Unauthorized)?;
        let mut member = member_from_row(&row)?;
        self.fill_links(self.db.observe(&mut **transaction), &mut member)
            .await?;
        Ok(member)
    }

    async fn member_locked(&self, transaction: &mut Tx<'_>, uid: i64) -> Result<Member> {
        let sql =
            &format!("SELECT {MEMBER_COLUMNS} FROM gonof_common_member WHERE uid = ? FOR UPDATE");
        let row = crate::db::query(sql)
            .bind(uid)
            .fetch_optional(self.db.observe(&mut **transaction))
            .await?
            .ok_or(IdentityError::Unauthorized)?;
        let mut member = member_from_row(&row)?;
        self.fill_links(self.db.observe(&mut **transaction), &mut member)
            .await?;
        Ok(member)
    }

    /// When this request's participation requirements look at third-party links and neither the email nor the mobile
    /// number on the member row satisfies them, also reads [`Member::linked`]; otherwise no database query.
    pub(crate) async fn fill_links<'c, E>(&self, executor: E, member: &mut Member) -> Result<()>
    where
        E: crate::db::Executor<'c>,
    {
        if !member.is_admin
            && crate::login::Participation::current()
                .needs_links(member.email_verified, member.mobile.is_some())
        {
            member.linked = crate::login::linked(executor, member.uid).await?;
        }
        Ok(())
    }

    async fn account_locked(&self, transaction: &mut Tx<'_>, uid: i64) -> Result<LockedAccount> {
        let sql = &format!(
            "SELECT {MEMBER_COLUMNS}, password FROM gonof_common_member WHERE uid = ? FOR UPDATE"
        );
        let row = crate::db::query(sql)
            .bind(uid)
            .fetch_optional(self.db.observe(&mut **transaction))
            .await?
            .ok_or(IdentityError::Unauthorized)?;
        let mut member = member_from_row(&row)?;
        self.fill_links(self.db.observe(&mut **transaction), &mut member)
            .await?;
        Ok(LockedAccount {
            member,
            password: row.try_get("password")?,
        })
    }

    async fn member_for_write(
        &self,
        transaction: &mut Tx<'_>,
        authentication: MemberAuthentication<'_>,
    ) -> Result<Member> {
        match authentication {
            MemberAuthentication::TrustedUid(uid) => self.member_locked(transaction, uid).await,
            MemberAuthentication::Cookie(raw) => {
                self.security_member_locked(transaction, raw).await
            }
        }
    }

    async fn audit(
        &self,
        transaction: &mut Tx<'_>,
        uid: i64,
        operation: &str,
        data: serde_json::Value,
        timestamp: i64,
    ) -> Result<i64> {
        let sql = "INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline)
            VALUES (?, ?, ?, ?, 'web', ?)";
        let inserted = crate::db::query(sql)
            .bind(uid)
            .bind(uid)
            .bind(operation)
            .bind(data.to_string())
            .bind(timestamp)
            .execute(self.db.observe(&mut **transaction))
            .await?;
        Ok(i64::try_from(inserted.last_insert_id()).context("log id out of range")?)
    }

    /// A session is just an encrypted cookie; issuing one writes nothing to the database. Lifetimes come from
    /// `settings.account.session_*_days`: the absolute lifetime is sealed into the cookie, so changing it only affects
    /// new logins; the sliding lifetime is computed from the settings at renewal time.
    fn grant_session(
        &self,
        member: Member,
        timestamp: i64,
        account: &crate::runtime::AccountSettings,
    ) -> Result<SessionGrant> {
        self.config.validate()?;
        let claims = cookie::CookieClaims {
            uid: member.uid,
            credentials_version: member.credentials_version,
            login_id: random_token(),
            csrf_token: random_token(),
            issued_at: timestamp,
            idle_expires_at: timestamp + account.session_idle_days.saturating_mul(86_400),
            absolute_expires_at: timestamp + account.session_absolute_days.saturating_mul(86_400),
        };
        let session_token = self.config.cookie.seal(&claims)?;
        Ok(SessionGrant {
            member,
            session_token,
            csrf_token: claims.csrf_token,
            expires_at: claims.idle_expires_at,
            device_marker: None,
        })
    }

    async fn enqueue_verification(
        &self,
        transaction: &mut Tx<'_>,
        member: &Member,
        (timestamp, ttl_hours): (i64, i64),
    ) -> Result<()> {
        let email = member.email.as_ref().ok_or(IdentityError::Forbidden)?;
        let token = random_token();
        let digest = token_hash(&token);
        rules::insert_token(
            &self.db,
            transaction,
            member.uid,
            rules::TokenPurpose::EmailVerification,
            &digest,
            email,
            timestamp,
            timestamp + ttl_hours.saturating_mul(3600),
        )
        .await?;
        let sql = "INSERT INTO gonof_common_outbox
            (event_key, event_type, aggregate_type, aggregate_id, payload, dateline, available_at)
            VALUES (?, 'email_verification', 'member', ?, ?, ?, ?)";
        crate::db::query(sql)
            .bind(format!("email_verification:{digest}"))
            .bind(member.uid)
            .bind(crate::mail::identity_payload(
                email,
                &member.username,
                &token,
            ))
            .bind(timestamp)
            .bind(timestamp)
            .execute(self.db.observe(&mut **transaction))
            .await?;
        Ok(())
    }

    /// Whether the site still has no administrator: the preseeded marker row `identity.bootstrap_admin` stays
    /// `pending` until the setup wizard creates the first one.
    pub async fn needs_admin(&self) -> Result<bool> {
        let sql = "SELECT svalue FROM gonof_common_settings WHERE skey = ?";
        let value: Option<String> = crate::db::query_scalar(sql)
            .bind(BOOTSTRAP_KEY)
            .fetch_optional(self.db.executor())
            .await?;
        Ok(value.as_deref() == Some("pending"))
    }

    /// The database and global cache this identity service writes through (the setup wizard saves the site time zone
    /// with them right after creating the administrator).
    pub(crate) fn storage(&self) -> (&Database, &crate::cache::GlobalCache) {
        (&self.db, &self.global)
    }

    /// Creates the site's first administrator with the name and password chosen in the setup wizard.
    ///
    /// The preseeded marker row is locked first and moves from `pending` to `complete` in the same transaction as the
    /// account, so concurrent wizards, or several processes on one database, create exactly one. Argon2 runs before
    /// the write transaction.
    pub async fn setup_admin(
        &self,
        username: &str,
        email: Option<&str>,
        password: &str,
    ) -> Result<i64> {
        self.config.validate()?;
        let minimum = self.settings().await?.account.password_min_length;
        let (username, email) = check_new_admin(username, email, password, minimum)?;
        let password = hash_password(password.to_owned()).await?;
        let mut transaction = self.db.begin_write().await?;
        let sql = "SELECT svalue FROM gonof_common_settings WHERE skey = ? FOR UPDATE";
        let marker: String = crate::db::query_scalar(sql)
            .bind(BOOTSTRAP_KEY)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        if marker != "pending" {
            return Err(IdentityError::Conflict(
                crate::i18n::current().setup_error_admin_exists(),
            ));
        }
        // The address comes from the site owner in the wizard, so it counts as verified: mail delivery is only
        // configured afterwards in the admin area.
        let emailstatus = i64::from(email.is_some());
        rules::Credentials {
            password: Some(&password),
            email: email.as_deref(),
            mobile: None,
            external: false,
            emailstatus,
            adminid: 1,
        }
        .check()?;
        let timestamp = now();
        let sql = "INSERT INTO gonof_common_member (username, email, password, emailstatus, adminid, regdate)
            VALUES (?, ?, ?, ?, 1, ?)";
        crate::db::query(sql)
            .bind(&username)
            .bind(email.as_deref())
            .bind(&password)
            .bind(emailstatus)
            .bind(timestamp)
            .execute(self.db.observe(&mut *transaction))
            .await?;
        let sql = "SELECT uid FROM gonof_common_member WHERE username = ?";
        let uid: i64 = crate::db::query_scalar(sql)
            .bind(&username)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        let sql = "UPDATE gonof_common_settings SET svalue = ? WHERE skey = ?";
        crate::db::query(sql)
            .bind(serde_json::json!({"state": "complete", "uid": uid}).to_string())
            .bind(BOOTSTRAP_KEY)
            .execute(self.db.observe(&mut *transaction))
            .await?;
        self.audit(
            &mut transaction,
            uid,
            "identity.bootstrap_admin",
            serde_json::json!({"method": "setup_wizard"}),
            timestamp,
        )
        .await?;
        transaction.commit().await?;
        Ok(uid)
    }

    pub async fn login(&self, identifier: &str, password: &str) -> Result<SessionGrant> {
        self.login_with_metadata(identifier, password, &SessionMetadata::default())
            .await
    }

    /// For tests and the command line: skips the web's second step. Refuses when the account has two-factor
    /// authentication on, so it cannot be used to bypass it.
    pub async fn login_with_metadata(
        &self,
        identifier: &str,
        password: &str,
        metadata: &SessionMetadata,
    ) -> Result<SessionGrant> {
        match self.begin_login(identifier, password, metadata).await? {
            LoginOutcome::Session(grant) => Ok(*grant),
            LoginOutcome::SecondFactor(_) => Err(IdentityError::Validation(
                crate::i18n::current().account_error_two_factor_web(),
            )),
        }
    }

    /// Step one of web login: once the password checks out, reads the two-factor state under the member row lock (it
    /// is only read at this point). Without two-factor a session is issued; with it only a credential awaiting step
    /// two is issued, and [`Self::complete_login`] issues the session.
    pub async fn begin_login(
        &self,
        identifier: &str,
        password: &str,
        metadata: &SessionMetadata,
    ) -> Result<LoginOutcome> {
        if password.len() > 512
            || password.chars().count() > 128
            || identifier.chars().count() > 255
        {
            return Err(IdentityError::Unauthorized);
        }
        let identifier = normalized(identifier);
        let column = if identifier.contains('@') {
            "email"
        } else {
            "username"
        };
        let sql = &format!(
            "SELECT uid, password
            FROM gonof_common_member WHERE {column} = ?"
        );
        let candidate = crate::db::query(sql)
            .bind(&identifier)
            .fetch_optional(self.db.executor())
            .await?;
        let encoded = candidate
            .as_ref()
            .map(|row| row.try_get::<Option<String>, _>("password"))
            .transpose()?
            .flatten();
        let encoded = encoded.filter(|value| !value.is_empty());
        if !verify_password(password.to_owned(), encoded.clone()).await? {
            return Err(IdentityError::Unauthorized);
        }
        let uid: i64 = candidate
            .ok_or(IdentityError::Unauthorized)?
            .try_get("uid")?;
        let settings = self.settings().await?;
        let mut transaction = self.db.begin_write().await?;
        let LockedAccount { member, password } = self.account_locked(&mut transaction, uid).await?;
        if encoded != password {
            return Err(IdentityError::Unauthorized);
        }
        if !member.active() || member.frozen {
            if let Some(message) =
                crate::moderation::frozen_notice(&self.db, &mut transaction, uid).await?
            {
                return Err(IdentityError::Suspended(message));
            }
            return Err(IdentityError::Unauthorized);
        }
        let timestamp = now();
        if self.has_two_factor_locked(&mut transaction, uid).await? {
            transaction.commit().await?;
            return Ok(LoginOutcome::SecondFactor(
                self.pending_login(&member, timestamp)?,
            ));
        }
        let grant = self
            .finish_login(
                &mut transaction,
                member,
                metadata,
                None,
                None,
                timestamp,
                &settings.account,
            )
            .await?;
        transaction.commit().await?;
        Ok(LoginOutcome::Session(Box::new(grant)))
    }

    pub async fn register(
        &self,
        username: &str,
        email: &str,
        password: &str,
        invite_code: &str,
    ) -> Result<SessionGrant> {
        self.register_with_metadata(
            username,
            email,
            password,
            invite_code,
            &SessionMetadata::default(),
        )
        .await
    }

    pub async fn register_with_metadata(
        &self,
        username: &str,
        email: &str,
        password: &str,
        invite_code: &str,
        metadata: &SessionMetadata,
    ) -> Result<SessionGrant> {
        self.register_with_consent(username, email, password, invite_code, metadata, None)
            .await
    }

    /// Web registration: the member ticked agreement to the terms and the privacy notice, and the consent record is
    /// written together with the registration log. The tick itself is validated by the web form; this only records
    /// it. Tests and fixtures use the entry point without a consent record.
    pub async fn register_with_consent(
        &self,
        username: &str,
        email: &str,
        password: &str,
        invite_code: &str,
        metadata: &SessionMetadata,
        consent: Option<PolicyConsent>,
    ) -> Result<SessionGrant> {
        let username = normalize_username(username)?;
        let email = normalize_email(email)?;
        // Take the site switches and the password minimum from the process cache first: when registration is closed
        // or the site is read-only there is no need to spend 19 MiB and a core hashing first. The transaction below
        // checks again with a locking read, and that check is authoritative.
        let cached = self.settings().await?;
        validate_password(password, cached.account.password_min_length)?;
        let invite_code = invite_code.trim();
        if invite_code.len() > 64 {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_invite_invalid(),
            ));
        }
        registration_open(&cached, invite_code)?;
        let password = hash_password(password.to_owned()).await?;
        self.create_account(NewAccount {
            username,
            email: Some(email),
            email_verified: false,
            password: Some(password),
            mobile: None,
            external: None,
            invite_code,
            metadata,
            consent,
            method: "password",
        })
        .await
    }

    /// The account-creation transaction (shared by password, SMS and third-party registration): site switches, invite
    /// codes and prefixes, reserved usernames, credential combinations, the third-party link, the verification email
    /// (when there is an unverified email), the registration log and the session. The entry point has already
    /// normalized the username, email and mobile number and hashed the password.
    pub(super) async fn create_account(&self, account: NewAccount<'_>) -> Result<SessionGrant> {
        let NewAccount {
            username,
            email,
            email_verified,
            password,
            mobile,
            external,
            invite_code,
            metadata,
            consent,
            method,
        } = account;
        let mut transaction = self.db.begin_write().await?;
        // Site switches precede the shared username namespace and member locks.
        let settings = crate::runtime::locked(&self.db, &mut transaction).await?;
        registration_open(&settings, invite_code)?;
        // A supplied code is always checked and consumed, including open registration.
        // Invite code prefixes set by administrators (`settings.invite.prefixes`): codes starting with any prefix pass
        // directly, are not consumed and can be reused. When the table happens to hold this code it is still consumed
        // as an invitation, and the prefixes are consulted once that invitation is no longer usable.
        let prefix = if invite_code.is_empty() {
            None
        } else {
            settings.invite.prefix_of(invite_code).map(str::to_owned)
        };
        let owner_uid: Option<i64> = if invite_code.is_empty() {
            None
        } else {
            let owner = crate::db::query_scalar("SELECT uid FROM gonof_common_invite WHERE code=?")
                .bind(invite_code)
                .fetch_optional(self.db.observe(&mut *transaction))
                .await?;
            if owner.is_none() && prefix.is_none() {
                return Err(IdentityError::Validation(
                    crate::i18n::current().account_error_invite_invalid(),
                ));
            }
            owner
        };
        self.lock_username_namespace(&mut transaction).await?;
        self.check_username_reservation(&mut transaction, &username, None, now())
            .await?;
        if let Some(mobile) = &mobile {
            let taken: Option<i64> = crate::db::query_scalar(
                "SELECT uid FROM gonof_common_member WHERE mobile=? FOR UPDATE",
            )
            .bind(mobile)
            .fetch_optional(self.db.observe(&mut *transaction))
            .await?;
            if taken.is_some() {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().account_error_mobile_registered(),
                ));
            }
        }
        if let Some(link) = &external {
            external::check_unlinked(&self.db, &mut transaction, link).await?;
            // A third-party email equal to an existing account's email is not merged.
            if let Some(email) = &email {
                let taken: Option<i64> = crate::db::query_scalar(
                    "SELECT uid FROM gonof_common_member WHERE email=? FOR UPDATE",
                )
                .bind(email)
                .fetch_optional(self.db.observe(&mut *transaction))
                .await?;
                if taken.is_some() {
                    return Err(IdentityError::Conflict(
                        crate::i18n::current().account_error_email_taken(),
                    ));
                }
            }
        }
        let timestamp = now();
        let invite = if let Some(owner_uid) = owner_uid {
            let inviter = self.member_locked(&mut transaction, owner_uid).await?;
            let usable = if inviter.can_contribute() {
                let row = crate::db::query(&format!(
                    "SELECT {INVITE_COLUMNS} FROM gonof_common_invite WHERE code=? FOR UPDATE"
                ))
                .bind(invite_code)
                .fetch_optional(self.db.observe(&mut *transaction))
                .await?;
                row.as_ref()
                    .map(invite_from_row)
                    .transpose()?
                    .filter(|invite| {
                        invite.uid == owner_uid
                            && invite.status == "available"
                            && invite.endtime > timestamp
                    })
            } else {
                None
            };
            if usable.is_none() && prefix.is_none() {
                return Err(IdentityError::Validation(
                    crate::i18n::current().account_error_invite_invalid(),
                ));
            }
            usable
        } else {
            None
        };
        // Record the prefix only when no invitation was consumed and the code passed by prefix.
        let prefix = if invite.is_none() { prefix } else { None };
        // The entry point already ran normalize_username, normalize_email and validate_password; this backs up the
        // member value rules of `identity::rules`.
        rules::check_username(&username)?;
        let emailstatus = i64::from(email_verified && email.is_some());
        rules::Credentials {
            password: password.as_deref(),
            email: email.as_deref(),
            mobile: mobile.as_deref(),
            external: external.is_some(),
            emailstatus,
            adminid: 0,
        }
        .check()?;
        let sql = "INSERT INTO gonof_common_member (username, email, emailstatus, password, mobile, regdate) VALUES (?, ?, ?, ?, ?, ?)";
        crate::db::query(sql)
            .bind(&username)
            .bind(&email)
            .bind(emailstatus)
            .bind(&password)
            .bind(&mobile)
            .bind(timestamp)
            .execute(self.db.observe(&mut *transaction))
            .await?;
        let sql = &format!("SELECT {MEMBER_COLUMNS} FROM gonof_common_member WHERE username = ?");
        let row = crate::db::query(sql)
            .bind(&username)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        let mut member = member_from_row(&row)?;
        if let Some(link) = &external {
            external::insert_link(&self.db, &mut transaction, member.uid, link, timestamp).await?;
            member.linked = crate::login::Linked {
                google: link.provider == crate::oauth::Provider::Google,
                github: link.provider == crate::oauth::Provider::Github,
            };
        }
        if let Some(invite) = &invite {
            // Single use: status, user and registration time are written in one statement; the registration time is
            // not before issuance (the issue time is used when the clock goes backwards) and not after expiry (the
            // endtime>? in the condition).
            let updated = crate::db::query("UPDATE gonof_common_invite SET status=?,fuid=?,regdateline=GREATEST(dateline,?) WHERE id=? AND status=? AND endtime>?")
                .bind(rules::InviteStatus::Used.value()).bind(member.uid).bind(timestamp).bind(invite.id)
                .bind(rules::InviteStatus::Available.value()).bind(timestamp)
                .execute(self.db.observe(&mut *transaction)).await?.rows_affected();
            if updated != 1 {
                return Err(IdentityError::Validation(
                    crate::i18n::current().account_error_invite_invalid(),
                ));
            }
        }
        if member.email.is_some() && !member.email_verified {
            self.enqueue_verification(
                &mut transaction,
                &member,
                (timestamp, settings.account.verification_ttl_hours),
            )
            .await?;
        }
        self.audit(
            &mut transaction,
            member.uid,
            "identity.register",
            serde_json::json!({"inviter_uid":invite.as_ref().map(|i| i.uid),"invite_id":invite.as_ref().map(|i| i.id),
                "invite_prefix": prefix,
                "method": method,
                "mobile": mobile.as_deref().map(phone::mask_mobile),
                "external": external.as_ref().map(|link| serde_json::json!({
                    "provider": link.provider.value(), "display": link.display})),
                "registration_source": metadata.registration_source,
                "user_agent": metadata.user_agent, "client_ip": metadata.client_ip,
                "consent": consent.map(|c| serde_json::json!({
                    "terms_version": c.terms_version, "privacy_version": c.privacy_version}))}),
            timestamp,
        )
        .await?;
        crate::registration_sources::record(
            &self.db,
            &mut transaction,
            member.uid,
            &metadata.registration_source,
            invite.as_ref().map(|i| i.uid),
            prefix.as_deref(),
        )
        .await?;
        let mut grant = self.grant_session(member, timestamp, &settings.account)?;
        // The registering browser is recorded as a known device of this account, so later logins here send no alert.
        if let Some(raw) = &metadata.device {
            grant.device_marker = Some(login_alert::remember(
                &self.config.cookie,
                raw,
                grant.member.uid,
            )?);
        }
        transaction.commit().await?;
        Ok(grant)
    }

    fn cookie_claims(&self, raw_cookie: &str) -> Result<cookie::CookieClaims> {
        self.config.cookie.validate()?;
        self.config
            .cookie
            .open(raw_cookie)
            .filter(|claims| claims.valid_at(now()))
            .ok_or(IdentityError::Unauthorized)
    }

    /// An ordinary request reads the member from the primary database once; a missing presence record does not change
    /// the signed-in identity.
    pub async fn session(&self, raw_cookie: &str) -> Result<Option<AuthSession>> {
        let mut claims = match self.cookie_claims(raw_cookie) {
            Ok(claims) => claims,
            Err(IdentityError::Unauthorized) => return Ok(None),
            Err(error) => return Err(error),
        };
        let row = crate::db::query(&format!(
            "SELECT {MEMBER_COLUMNS} FROM gonof_common_member WHERE uid = ?"
        ))
        .bind(claims.uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut member = member_from_row(&row)?;
        // The primary-database read may wait for a connection; lifetimes must be rechecked against the time the read
        // finished.
        let timestamp = now();
        if !member.active()
            || member.frozen
            || member.credentials_version != claims.credentials_version
            || !claims.valid_at(timestamp)
        {
            return Ok(None);
        }
        self.fill_links(self.db.executor(), &mut member).await?;
        let idle = self.settings().await?.account.session_idle_days;
        let renewed = claims.renew_at(timestamp, idle.saturating_mul(86_400));
        let renewed_cookie = if renewed || !self.config.cookie.uses_active_key(raw_cookie) {
            Some(self.config.cookie.seal(&claims)?)
        } else {
            None
        };
        Ok(Some(AuthSession {
            member,
            csrf_hash: token_hash(&claims.csrf_token),
            csrf_token: claims.csrf_token,
            session_token: renewed_cookie
                .clone()
                .unwrap_or_else(|| raw_cookie.to_owned()),
            renewed_cookie,
            online_id: token_hash(&claims.login_id),
            expires_at: claims.idle_expires_at,
        }))
    }

    /// Invite code page. "Generated today" counts calendar days in the site time zone, with the time zone taken from
    /// the process cache (read-only display); the quota check at generation uses the write gate's copy.
    pub async fn invites(&self, uid: i64, page: usize) -> Result<InvitePage> {
        let offset_minutes = self.settings().await?.time.offset_minutes;
        let mut transaction = self.db.begin_write().await?;
        if !self
            .member_locked(&mut transaction, uid)
            .await?
            .can_contribute()
        {
            return Err(IdentityError::Forbidden);
        }
        let offset = page
            .checked_sub(1)
            .and_then(|page| page.checked_mul(INVITES_PAGE_SIZE))
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(IdentityError::NotFound)?;
        let total: i64 =
            crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_invite WHERE uid = ?")
                .bind(uid)
                .fetch_one(self.db.observe(&mut *transaction))
                .await?;
        if offset >= total && page != 1 {
            return Err(IdentityError::NotFound);
        }
        let timestamp = now();
        let day_start = crate::clock::day_start(timestamp, offset_minutes);
        let today_generated = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_common_invite WHERE uid = ? AND dateline >= ?",
        )
        .bind(uid)
        .bind(day_start)
        .fetch_one(self.db.observe(&mut *transaction))
        .await?;
        let active = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_common_invite WHERE uid = ? AND status IN ('available', 'frozen') AND endtime > ?",
        )
        .bind(uid)
        .bind(timestamp)
        .fetch_one(self.db.observe(&mut *transaction))
        .await?;
        let sql = &format!(
            "SELECT {INVITE_COLUMNS} FROM gonof_common_invite WHERE uid = ? ORDER BY id DESC LIMIT ? OFFSET ?"
        );
        let rows = crate::db::query(sql)
            .bind(uid)
            .bind(INVITES_PAGE_SIZE as i64)
            .bind(offset)
            .fetch_all(self.db.observe(&mut *transaction))
            .await?;
        let invites = rows.iter().map(invite_from_row).collect::<Result<_>>()?;
        transaction.commit().await?;
        Ok(InvitePage {
            rows: invites,
            total,
            today_generated,
            active,
            timestamp,
            has_next: total.saturating_sub(offset) > INVITES_PAGE_SIZE as i64,
        })
    }

    /// Trusted internal call; HTTP requests must use generate_invite_for_session.
    pub async fn generate_invite(&self, uid: i64, request_key: &str) -> Result<Invite> {
        self.generate_invite_as(MemberAuthentication::TrustedUid(uid), request_key)
            .await
    }

    pub async fn generate_invite_for_session(
        &self,
        raw_cookie: &str,
        request_key: &str,
    ) -> Result<Invite> {
        self.generate_invite_as(MemberAuthentication::Cookie(raw_cookie), request_key)
            .await
    }

    async fn generate_invite_as(
        &self,
        authentication: MemberAuthentication<'_>,
        request_key: &str,
    ) -> Result<Invite> {
        if request_key.is_empty()
            || request_key.len() > 64
            || !request_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_invite_request(),
            ));
        }
        let mut transaction = self.db.begin_write().await?;
        let settings = crate::runtime::locked(&self.db, &mut transaction).await?;
        if settings.site.read_only {
            return Err(IdentityError::Suspended(
                settings.site.read_only_notice(crate::i18n::current()),
            ));
        }
        if !settings.invite.enabled {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_invites_off(),
            ));
        }
        let member = self
            .member_for_write(&mut transaction, authentication)
            .await?;
        if !member.can_contribute() {
            return Err(IdentityError::Forbidden);
        }
        // Level gate (docs/roles.md, "Level gates"): staff are not restricted; when the required level is 1 growth
        // points are not read.
        if !member.is_staff()
            && crate::growth::level_gate(
                &self.db,
                &mut transaction,
                &settings.growth,
                member.uid,
                settings.growth.invite_level,
            )
            .await?
            .is_some()
        {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_invite_level(),
            ));
        }
        let uid = member.uid;
        let sql = &format!(
            "SELECT {INVITE_COLUMNS} FROM gonof_common_invite WHERE uid = ? AND request_key = ? FOR UPDATE"
        );
        if let Some(row) = crate::db::query(sql)
            .bind(uid)
            .bind(request_key)
            .fetch_optional(self.db.observe(&mut *transaction))
            .await?
        {
            let invite = invite_from_row(&row)?;
            transaction.commit().await?;
            return Ok(invite);
        }
        let timestamp = now();
        // The daily quota counts calendar days in the site time zone; the time zone and the quota both come from this
        // write-gate copy.
        let day_start = crate::clock::day_start(timestamp, settings.time.offset_minutes);
        let sql =
            "SELECT COUNT(*) AS total FROM gonof_common_invite WHERE uid = ? AND dateline >= ?";
        let row = crate::db::query(sql)
            .bind(uid)
            .bind(day_start)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        if row.try_get::<i64, _>("total")? >= settings.invite.daily_limit {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_invite_daily(),
            ));
        }
        let sql = "SELECT COUNT(*) AS total FROM gonof_common_invite
            WHERE uid = ? AND status IN ('available', 'frozen') AND endtime > ?";
        let row = crate::db::query(sql)
            .bind(uid)
            .bind(timestamp)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        if row.try_get::<i64, _>("total")? >= settings.invite.active_limit {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_invite_active(),
            ));
        }
        let code = random_token();
        rules::insert_invite(
            &self.db,
            &mut transaction,
            uid,
            &code,
            request_key,
            timestamp,
            timestamp + settings.invite.ttl_days.saturating_mul(86_400),
        )
        .await?;
        let sql = &format!(
            "SELECT {INVITE_COLUMNS} FROM gonof_common_invite WHERE uid = ? AND request_key = ?"
        );
        let row = crate::db::query(sql)
            .bind(uid)
            .bind(request_key)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        let invite = invite_from_row(&row)?;
        self.audit(
            &mut transaction,
            uid,
            "identity.invite.generate",
            serde_json::json!({"invite_id":invite.id}),
            timestamp,
        )
        .await?;
        transaction.commit().await?;
        Ok(invite)
    }

    /// Trusted internal call; HTTP requests must use revoke_invite_for_session.
    pub async fn revoke_invite(&self, uid: i64, id: i64) -> Result<()> {
        self.revoke_invite_as(MemberAuthentication::TrustedUid(uid), id)
            .await
    }

    pub async fn revoke_invite_for_session(&self, raw_cookie: &str, id: i64) -> Result<()> {
        self.revoke_invite_as(MemberAuthentication::Cookie(raw_cookie), id)
            .await
    }

    async fn revoke_invite_as(
        &self,
        authentication: MemberAuthentication<'_>,
        id: i64,
    ) -> Result<()> {
        // Revoking one's own invite code brings no new accounts, so it works as usual in read-only mode; generating is
        // still blocked.
        let mut transaction = self.db.begin_write().await?;
        let member = self
            .member_for_write(&mut transaction, authentication)
            .await?;
        if !member.can_contribute() {
            return Err(IdentityError::Forbidden);
        }
        let uid = member.uid;
        let sql = "SELECT status FROM gonof_common_invite WHERE id = ? AND uid = ? FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(id)
            .bind(uid)
            .fetch_optional(self.db.observe(&mut *transaction))
            .await?
            .ok_or(IdentityError::NotFound)?;
        match db::text(&row, "status")?.as_str() {
            "available" | "frozen" => {}
            "revoked" => {
                transaction.commit().await?;
                return Ok(());
            }
            _ => {
                return Err(IdentityError::Conflict(
                    crate::i18n::current().account_error_invite_revoke(),
                ));
            }
        }
        let sql = "UPDATE gonof_common_invite SET status = 'revoked' WHERE id = ? AND uid = ? AND status IN ('available', 'frozen')";
        let affected = crate::db::query(sql)
            .bind(id)
            .bind(uid)
            .execute(self.db.observe(&mut *transaction))
            .await?
            .rows_affected();
        if affected != 1 {
            return Err(IdentityError::Conflict(
                crate::i18n::current().account_error_invite_revoke(),
            ));
        }
        self.audit(
            &mut transaction,
            uid,
            "identity.invite.revoke",
            serde_json::json!({"invite_id":id}),
            now(),
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Read-only precheck for email-link pages. Locates only by token digest and joins the member row by primary key,
    /// returning raw fields; the caller judges account status and token validity in code, condition by condition
    /// matching the locking recheck at submission.
    async fn token_preview(&self, raw_token: &str, purpose: &str) -> Result<Option<TokenPreview>> {
        let sql =
            "SELECT t.email AS token_email, t.dateline, t.expires_at, t.consumed_at, t.revoked_at,
            m.email, m.emailstatus, m.`freeze`, m.account_state, m.password
            FROM gonof_common_member_token t JOIN gonof_common_member m ON m.uid = t.uid
            WHERE t.token_hash = ? AND t.purpose = ?";
        let Some(row) = crate::db::query(sql)
            .bind(token_hash(raw_token))
            .bind(purpose)
            .fetch_optional(self.db.executor())
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(TokenPreview {
            token_email: db::text(&row, "token_email")?,
            dateline: row.try_get("dateline")?,
            expires_at: row.try_get("expires_at")?,
            consumed_at: row.try_get("consumed_at")?,
            revoked_at: row.try_get("revoked_at")?,
            member_email: row.try_get("email")?,
            email_verified: row.try_get::<i64, _>("emailstatus")? == 1,
            frozen: row.try_get::<i64, _>("freeze")? != 0,
            active: db::text(&row, "account_state")? == "active",
            has_password: row
                .try_get::<Option<String>, _>("password")?
                .is_some_and(|password| !password.is_empty()),
        }))
    }

    pub async fn verification_status(&self, raw_token: &str) -> Result<bool> {
        if raw_token.len() != 64 {
            return Ok(false);
        }
        let Some(preview) = self.token_preview(raw_token, "email_verification").await? else {
            return Ok(false);
        };
        // Consistent with the in-lock recheck of verify_email: verification tokens ignore the issue time.
        Ok(preview.active
            && !preview.frozen
            && !preview.email_verified
            && preview.unused_before(now())
            && preview.member_email.as_deref() == Some(preview.token_email.as_str()))
    }

    pub async fn verify_email(&self, raw_token: &str) -> Result<()> {
        if raw_token.len() != 64 {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_verify_invalid(),
            ));
        }
        let digest = token_hash(raw_token);
        let sql = "SELECT uid FROM gonof_common_member_token WHERE token_hash = ? AND purpose = 'email_verification'";
        let row = crate::db::query(sql)
            .bind(&digest)
            .fetch_optional(self.db.executor())
            .await?
            .ok_or(IdentityError::Validation(
                crate::i18n::current().account_error_verify_invalid(),
            ))?;
        let uid: i64 = row.try_get("uid")?;
        let mut transaction = self.db.begin_write().await?;
        let member = self.member_locked(&mut transaction, uid).await?;
        if !member.active() || member.frozen || member.email_verified {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_verify_invalid(),
            ));
        }
        let sql =
            "SELECT id, email, expires_at, consumed_at, revoked_at FROM gonof_common_member_token
            WHERE token_hash = ? AND purpose = 'email_verification' FOR UPDATE";
        let row = crate::db::query(sql)
            .bind(&digest)
            .fetch_one(self.db.observe(&mut *transaction))
            .await?;
        let timestamp = now();
        if row.try_get::<i64, _>("expires_at")? <= timestamp
            || row.try_get::<Option<i64>, _>("consumed_at")?.is_some()
            || row.try_get::<Option<i64>, _>("revoked_at")?.is_some()
            || member.email.as_deref() != Some(db::text(&row, "email")?.as_str())
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_verify_invalid(),
            ));
        }
        let id: i64 = row.try_get("id")?;
        // Verification tokens ignore the issue time; the redemption time is not before issuance, and the issue time is
        // used when the clock goes backwards.
        let sql = "UPDATE gonof_common_member_token SET consumed_at = GREATEST(dateline, ?)
            WHERE id = ? AND consumed_at IS NULL AND revoked_at IS NULL AND expires_at > ?";
        let updated = crate::db::query(sql)
            .bind(timestamp)
            .bind(id)
            .bind(timestamp)
            .execute(self.db.observe(&mut *transaction))
            .await?
            .rows_affected();
        if updated != 1 {
            return Err(IdentityError::Validation(
                crate::i18n::current().account_error_verify_invalid(),
            ));
        }
        let sql = "UPDATE gonof_common_member SET emailstatus = 1 WHERE uid = ?";
        crate::db::query(sql)
            .bind(uid)
            .execute(self.db.observe(&mut *transaction))
            .await?;
        self.audit(
            &mut transaction,
            uid,
            "identity.email.verify",
            serde_json::json!({}),
            timestamp,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Password recovery of an account whose email was never verified: opening the link mailed to the address proves
    /// it, so the address counts as verified from now on and verification links still out are voided. The caller
    /// holds the member row lock and has checked that the token was issued for the member's current address.
    pub(super) async fn verify_email_by_recovery(
        &self,
        transaction: &mut Tx<'_>,
        uid: i64,
        timestamp: i64,
    ) -> Result<()> {
        let sql =
            "UPDATE gonof_common_member SET emailstatus = 1 WHERE uid = ? AND emailstatus = 0";
        crate::db::query(sql)
            .bind(uid)
            .execute(self.db.observe(&mut **transaction))
            .await?;
        let sql = "UPDATE gonof_common_member_token SET revoked_at = GREATEST(dateline, ?)
            WHERE uid = ? AND purpose = 'email_verification' AND consumed_at IS NULL AND revoked_at IS NULL";
        crate::db::query(sql)
            .bind(timestamp)
            .bind(uid)
            .execute(self.db.observe(&mut **transaction))
            .await?;
        self.audit(
            transaction,
            uid,
            "identity.email.verify",
            serde_json::json!({"method": "password_reset"}),
            timestamp,
        )
        .await?;
        Ok(())
    }

    /// Trusted internal call; HTTP requests must use resend_verification_for_session.
    pub async fn resend_verification(&self, uid: i64) -> Result<()> {
        self.resend_verification_as(MemberAuthentication::TrustedUid(uid))
            .await
    }

    pub async fn resend_verification_for_session(&self, raw_cookie: &str) -> Result<()> {
        self.resend_verification_as(MemberAuthentication::Cookie(raw_cookie))
            .await
    }

    async fn resend_verification_as(&self, authentication: MemberAuthentication<'_>) -> Result<()> {
        let ttl_hours = self.settings().await?.account.verification_ttl_hours;
        let mut transaction = self.db.begin_write().await?;
        let member = self
            .member_for_write(&mut transaction, authentication)
            .await?;
        let uid = member.uid;
        if !member.active() || member.frozen || member.email_verified || member.email.is_none() {
            return Err(IdentityError::Forbidden);
        }
        let timestamp = now();
        let sql = "UPDATE gonof_common_member_token SET revoked_at = GREATEST(dateline, ?)
            WHERE uid = ? AND purpose = 'email_verification' AND consumed_at IS NULL AND revoked_at IS NULL";
        crate::db::query(sql)
            .bind(timestamp)
            .bind(uid)
            .execute(self.db.observe(&mut *transaction))
            .await?;
        self.enqueue_verification(&mut transaction, &member, (timestamp, ttl_hours))
            .await?;
        self.audit(
            &mut transaction,
            uid,
            "identity.email.resend",
            serde_json::json!({}),
            timestamp,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_is_bound_to_session_and_uses_constant_length_digests() {
        let csrf = random_token();
        assert_eq!(csrf.len(), 64);
        assert_ne!(csrf, random_token());
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn normalization_matches_existing_seed_and_rejects_ambiguous_login_names() {
        // "\u{ff21}" is a full-width capital A: compatibility folding makes it "a".
        assert_eq!(normalize_username(" \u{ff21}lice ").unwrap(), "alice");
        assert!(normalize_username("alice@example.test").is_err());
        assert!(normalize_username("a b").is_err());
        assert_eq!(
            normalize_email(" User@EXAMPLE.test ").unwrap(),
            "user@example.test"
        );
        assert!(normalize_email("user@@example.test").is_err());
    }

    /// Takes every permit and returns them automatically after the test. Other unit tests in the same process that
    /// need to hash meanwhile queue for these few seconds.
    async fn hold_every_password_permit() -> tokio::sync::SemaphorePermit<'static> {
        PASSWORD_WORK
            .acquire_many(password_permits() as u32)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn password_work_waits_for_a_free_permit() {
        assert!((1..=4).contains(&password_permits()));
        let all = hold_every_password_permit().await;
        let hashing = tokio::spawn(hash_password("sample-password".into()));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !hashing.is_finished(),
            "no Argon2 run starts while every permit is taken"
        );
        drop(all);
        let encoded = tokio::time::timeout(std::time::Duration::from_secs(60), hashing)
            .await
            .expect("the run finishes once the permits are returned")
            .unwrap()
            .unwrap();
        assert!(encoded.starts_with("$argon2id$"));
    }

    #[tokio::test]
    async fn closed_or_invite_only_registration_is_rejected_before_hashing() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let global = crate::test_support::detached_global();
        let identity = Identity::new(
            services.db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            global.clone(),
        );
        // The entry precheck reads the process cache: after changing the database directly, clear this process's copy
        // so the next registration reads the new value.
        let set = |key: &'static str, value: &'static str| {
            let db = services.db.clone();
            let global = global.clone();
            async move {
                crate::db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?) ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)")
                    .bind(key)
                    .bind(value)
                    .execute(db.executor())
                    .await
                    .unwrap();
                global.clear_local();
            }
        };
        let register = |invite: &'static str| {
            let identity = identity.clone();
            async move {
                // All permits are taken: whichever path tries to hash first gets stuck here until the timeout.
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    identity.register(
                        "gatecheck",
                        "gatecheck@example.test",
                        "sample-password",
                        invite,
                    ),
                )
                .await
                .expect("a registration the site does not allow is rejected before hashing")
            }
        };
        let permits = hold_every_password_permit().await;
        set("settings.signup.mode", "closed").await;
        assert!(matches!(
            register("").await,
            Err(IdentityError::Conflict(_))
        ));
        set("settings.signup.mode", "invite").await;
        assert!(matches!(
            register("").await,
            Err(IdentityError::Validation(_))
        ));
        set("settings.signup.mode", "open").await;
        set("settings.site.read_only", "true").await;
        assert!(matches!(
            register("").await,
            Err(IdentityError::Suspended(_))
        ));
        drop(permits);
        set("settings.site.read_only", "false").await;
        register("").await.unwrap();
        services.db.pool.close().await;
        services.cleanup_ref().await.unwrap();
    }

    #[tokio::test]
    async fn salted_passwords_are_argon2id_and_verify_only_the_original() {
        let first = hash_password("sample-password".into()).await.unwrap();
        let second = hash_password("sample-password".into()).await.unwrap();
        assert!(first.starts_with("$argon2id$"));
        assert_ne!(first, second);
        assert!(
            verify_password("sample-password".into(), Some(first.clone()))
                .await
                .unwrap()
        );
        assert!(
            !verify_password("wrong-password".into(), Some(first))
                .await
                .unwrap()
        );
    }
}
